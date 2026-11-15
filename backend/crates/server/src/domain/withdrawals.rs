//! Rút tiền: chỉ rút từ `earnings_available`. Tiền nạp (`wallet_spendable`) không bao giờ rút được.

use chrono::{DateTime, Utc};
use ledger::{kinds, quote_withdrawal, AccountKey, TxBuilder};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{idem_or_random, lock_user, notifications, post_or, require_admin, AdminCtx};
use crate::audit::{self, AuditEntry};
use crate::error::{AppError, AppResult};
use crate::settings::{self, Settings};
use crate::util::format_vnd;

const COLS: &str = "id, user_id, amount, fee, net, status, bank_txn_ref, reason, created_at, \
                    reviewed_at";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Withdrawal {
    pub id: Uuid,
    pub user_id: Uuid,
    pub amount: i64,
    pub fee: i64,
    pub net: i64,
    pub status: String,
    pub bank_txn_ref: Option<String>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub amount: i64,
    pub fee: i64,
    pub net: i64,
    pub fee_bps: i64,
    pub withdraw_min: i64,
    pub earnings_available: i64,
    /// Có thể tạo lệnh rút với số tiền này ngay bây giờ không.
    pub eligible: bool,
    /// Mã lý do nếu không đủ điều kiện (`below_minimum`, `kyc_required`, `has_debt`, `insufficient_earnings`).
    pub reason: Option<String>,
}

struct Rules {
    settings: Settings,
    kyc_approved: bool,
    debt: i64,
    available: i64,
}

async fn load_rules(conn: &mut PgConnection, user_id: Uuid) -> AppResult<Rules> {
    let settings = settings::load(&mut *conn).await?;
    let kyc_status =
        sqlx::query_scalar::<_, String>("SELECT status FROM seller_kyc WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?;
    let debt = ledger::store::balance_of(&mut *conn, &AccountKey::seller_debt(user_id)).await?;
    let available =
        ledger::store::balance_of(&mut *conn, &AccountKey::earnings_available(user_id)).await?;
    Ok(Rules {
        settings,
        kyc_approved: kyc_status.as_deref() == Some("approved"),
        debt,
        available,
    })
}

/// Thứ tự kiểm tra: mức tối thiểu, KYC, công nợ, số dư doanh thu.
fn check_rules(rules: &Rules, amount: i64) -> Result<(), AppError> {
    if amount < rules.settings.withdraw_min {
        return Err(AppError::unprocessable(
            "below_minimum",
            format!(
                "Số tiền rút tối thiểu là {}.",
                format_vnd(rules.settings.withdraw_min)
            ),
        ));
    }
    if !rules.kyc_approved {
        return Err(AppError::unprocessable(
            "kyc_required",
            "Bạn cần gửi thông tin KYC và được admin duyệt trước khi rút tiền.",
        ));
    }
    if rules.debt > 0 {
        return Err(AppError::unprocessable(
            "has_debt",
            format!(
                "Bạn đang còn công nợ {}. Hãy xử lý hết công nợ trước khi rút.",
                format_vnd(rules.debt)
            ),
        ));
    }
    if amount > rules.available {
        return Err(insufficient_earnings(rules.available));
    }
    Ok(())
}

fn insufficient_earnings(available: i64) -> AppError {
    AppError::unprocessable(
        "insufficient_earnings",
        format!(
            "Số tiền vượt quá doanh thu có thể rút ({}). Tiền nạp vào ví chỉ dùng để mua hàng, không rút được.",
            format_vnd(available)
        ),
    )
}

/// Xem trước phí và số thực nhận, kèm việc có rút được hay không. Không ghi gì.
pub async fn preview(db: &PgPool, user_id: Uuid, amount: i64) -> AppResult<Preview> {
    if amount <= 0 {
        return Err(AppError::bad_request(
            "invalid_amount",
            "Số tiền rút phải lớn hơn 0.",
        ));
    }
    let mut conn = db.acquire().await?;
    let rules = load_rules(&mut conn, user_id).await?;
    let quote = quote_withdrawal(amount, rules.settings.withdraw_fee_bps)?;
    let verdict = check_rules(&rules, amount);
    Ok(Preview {
        amount,
        fee: quote.fee,
        net: quote.net,
        fee_bps: rules.settings.withdraw_fee_bps,
        withdraw_min: rules.settings.withdraw_min,
        earnings_available: rules.available,
        eligible: verdict.is_ok(),
        reason: verdict.err().map(|e| e.code().to_string()),
    })
}

/// Tạo lệnh rút: khóa `amount` từ `earnings_available` sang `withdrawal_hold`.
/// Phí và số thực nhận được chốt tại thời điểm tạo (đúng số người dùng đã thấy ở bước xem trước).
/// `idem` là Idempotency-Key của client: gửi lại cùng khóa thì trả lệnh cũ, không tạo thêm.
pub async fn create(
    db: &PgPool,
    user_id: Uuid,
    amount: i64,
    idem: Option<&str>,
) -> AppResult<Withdrawal> {
    let idem = idem_or_random(idem);
    let mut tx = db.begin().await?;
    lock_user(&mut tx, user_id).await?;

    let sql = format!("SELECT {COLS} FROM withdrawals WHERE user_id = $1 AND idempotency_key = $2");
    if let Some(existing) = sqlx::query_as::<_, Withdrawal>(&sql)
        .bind(user_id)
        .bind(&idem)
        .fetch_optional(&mut *tx)
        .await?
    {
        return Ok(existing);
    }

    let rules = load_rules(&mut tx, user_id).await?;
    check_rules(&rules, amount)?;
    let quote = quote_withdrawal(amount, rules.settings.withdraw_fee_bps)?;

    let id = Uuid::new_v4();
    let ledger_tx = TxBuilder::new(kinds::WITHDRAWAL_CREATE, format!("withdrawal:create:{id}"))
        .reference("withdrawal", id)
        .debit(AccountKey::earnings_available(user_id), amount)
        .credit(AccountKey::withdrawal_hold(user_id), amount)
        .build()?;
    post_or(&mut tx, &ledger_tx, insufficient_earnings).await?;

    let insert = format!(
        "INSERT INTO withdrawals (id, user_id, amount, fee, net, idempotency_key) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {COLS}"
    );
    let w = sqlx::query_as::<_, Withdrawal>(&insert)
        .bind(id)
        .bind(user_id)
        .bind(amount)
        .bind(quote.fee)
        .bind(quote.net)
        .bind(&idem)
        .fetch_one(&mut *tx)
        .await?;
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(user_id),
            action: "withdrawal.create",
            target_type: "withdrawal",
            target_id: Some(id.to_string()),
            before: None,
            after: Some(json!({ "amount": amount, "fee": quote.fee, "net": quote.net })),
            ip: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(w)
}

pub async fn list_for_user(db: &PgPool, user_id: Uuid, limit: i64) -> AppResult<Vec<Withdrawal>> {
    let sql = format!(
        "SELECT {COLS} FROM withdrawals WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2"
    );
    let rows = sqlx::query_as::<_, Withdrawal>(&sql)
        .bind(user_id)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminWithdrawalRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub amount: i64,
    pub fee: i64,
    pub net: i64,
    pub status: String,
    pub bank_txn_ref: Option<String>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub kyc_full_name: Option<String>,
    pub kyc_bank_name: Option<String>,
    pub kyc_account_enc: Option<Vec<u8>>,
    pub kyc_key_id: Option<String>,
}

pub async fn list_admin(
    db: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> AppResult<Vec<AdminWithdrawalRow>> {
    let rows = sqlx::query_as::<_, AdminWithdrawalRow>(
        "SELECT w.id, w.user_id, u.email, w.amount, w.fee, w.net, w.status, w.bank_txn_ref, \
                w.reason, w.created_at, \
                k.full_name AS kyc_full_name, k.bank_name AS kyc_bank_name, \
                k.account_no_enc AS kyc_account_enc, k.key_id AS kyc_key_id \
         FROM withdrawals w \
         JOIN users u ON u.id = w.user_id \
         LEFT JOIN seller_kyc k ON k.user_id = w.user_id \
         WHERE ($1::text IS NULL OR w.status = $1) \
         ORDER BY w.created_at DESC LIMIT $2",
    )
    .bind(status)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

async fn lock_pending(conn: &mut PgConnection, id: Uuid) -> AppResult<Withdrawal> {
    let sql = format!("SELECT {COLS} FROM withdrawals WHERE id = $1 FOR UPDATE");
    let w = sqlx::query_as::<_, Withdrawal>(&sql)
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::not_found("Không tìm thấy lệnh rút."))?;
    if w.status != "pending" {
        return Err(AppError::conflict(
            "withdrawal_not_pending",
            "Lệnh rút này đã được xử lý trước đó.",
        ));
    }
    Ok(w)
}

/// Admin duyệt: sau khi đã chuyển khoản ngoài hệ thống, nhập mã giao dịch ngân hàng.
/// Bút toán: `withdrawal_hold` giảm `amount`, `platform_revenue` tăng `fee`, `external` tăng `net`.
pub async fn approve(
    db: &PgPool,
    ctx: AdminCtx<'_>,
    withdrawal_id: Uuid,
    bank_txn_ref: &str,
) -> AppResult<Withdrawal> {
    let bank_ref = bank_txn_ref.trim();
    if bank_ref.is_empty() || bank_ref.chars().count() > 100 {
        return Err(AppError::bad_request(
            "bank_txn_ref_required",
            "Cần nhập mã giao dịch ngân hàng (tối đa 100 ký tự) khi duyệt lệnh rút.",
        ));
    }
    let mut tx = db.begin().await?;
    require_admin(&mut tx, ctx.admin_id).await?;
    let w = lock_pending(&mut tx, withdrawal_id).await?;

    let mut builder = TxBuilder::new(
        kinds::WITHDRAWAL_APPROVE,
        format!("withdrawal:approve:{withdrawal_id}"),
    )
    .reference("withdrawal", withdrawal_id)
    .debit(AccountKey::withdrawal_hold(w.user_id), w.amount);
    if w.fee > 0 {
        builder = builder.credit(AccountKey::platform_revenue(), w.fee);
    }
    if w.net > 0 {
        builder = builder.credit(AccountKey::external(), w.net);
    }
    let ledger_tx = builder.build()?;
    ledger::post(&mut tx, &ledger_tx).await?;

    let update = format!(
        "UPDATE withdrawals SET status = 'approved', bank_txn_ref = $2, reviewed_by = $3, \
         reviewed_at = now() WHERE id = $1 RETURNING {COLS}"
    );
    let updated = sqlx::query_as::<_, Withdrawal>(&update)
        .bind(withdrawal_id)
        .bind(bank_ref)
        .bind(ctx.admin_id)
        .fetch_one(&mut *tx)
        .await?;
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(ctx.admin_id),
            action: "withdrawal.approve",
            target_type: "withdrawal",
            target_id: Some(withdrawal_id.to_string()),
            before: Some(json!({ "status": "pending" })),
            after: Some(json!({
                "status": "approved", "bank_txn_ref": bank_ref,
                "amount": w.amount, "fee": w.fee, "net": w.net
            })),
            ip: Some(ctx.ip),
        },
    )
    .await?;
    notifications::create(
        &mut tx,
        w.user_id,
        "withdrawal_approved",
        &format!("Lệnh rút {} đã được chuyển khoản, bạn nhận {}.", format_vnd(w.amount), format_vnd(w.net)),
        Some(format!("withdrawal:{withdrawal_id}").as_str()),
    )
    .await?;
    tx.commit().await?;
    Ok(updated)
}

/// Admin từ chối: trả `amount` từ `withdrawal_hold` về `earnings_available`. Bắt buộc có lý do.
pub async fn reject(
    db: &PgPool,
    ctx: AdminCtx<'_>,
    withdrawal_id: Uuid,
    reason: &str,
) -> AppResult<Withdrawal> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(AppError::bad_request(
            "reason_required",
            "Cần nhập lý do từ chối (tối đa 500 ký tự).",
        ));
    }
    let mut tx = db.begin().await?;
    require_admin(&mut tx, ctx.admin_id).await?;
    let w = lock_pending(&mut tx, withdrawal_id).await?;

    let ledger_tx = TxBuilder::new(
        kinds::WITHDRAWAL_REJECT,
        format!("withdrawal:reject:{withdrawal_id}"),
    )
    .reference("withdrawal", withdrawal_id)
    .debit(AccountKey::withdrawal_hold(w.user_id), w.amount)
    .credit(AccountKey::earnings_available(w.user_id), w.amount)
    .build()?;
    ledger::post(&mut tx, &ledger_tx).await?;

    let update = format!(
        "UPDATE withdrawals SET status = 'rejected', reason = $2, reviewed_by = $3, \
         reviewed_at = now() WHERE id = $1 RETURNING {COLS}"
    );
    let updated = sqlx::query_as::<_, Withdrawal>(&update)
        .bind(withdrawal_id)
        .bind(reason)
        .bind(ctx.admin_id)
        .fetch_one(&mut *tx)
        .await?;
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(ctx.admin_id),
            action: "withdrawal.reject",
            target_type: "withdrawal",
            target_id: Some(withdrawal_id.to_string()),
            before: Some(json!({ "status": "pending" })),
            after: Some(json!({ "status": "rejected", "reason": reason, "amount": w.amount })),
            ip: Some(ctx.ip),
        },
    )
    .await?;
    notifications::create(
        &mut tx,
        w.user_id,
        "withdrawal_rejected",
        &format!("Lệnh rút {} bị từ chối: {reason}. Tiền đã trả về doanh thu của bạn.", format_vnd(w.amount)),
        Some(format!("withdrawal:{withdrawal_id}").as_str()),
    )
    .await?;
    tx.commit().await?;
    Ok(updated)
}
