use chrono::{DateTime, Utc};
use ledger::{kinds, AccountKey, TxBuilder};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{notifications, require_admin, AdminCtx};
use crate::audit::{self, AuditEntry};
use crate::error::{AppError, AppResult};
use crate::util::format_vnd;

/// Trần kỹ thuật cho một lần nạp (chống nhập nhầm và tràn số). Tài liệu chưa quy định hạn mức.
pub const MAX_DEPOSIT: i64 = 1_000_000_000;

const COLS: &str = "id, user_id, amount, gateway, gateway_ref, order_code, transfer_code, \
                    status, confirmed_at, created_at";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Deposit {
    pub id: Uuid,
    pub user_id: Uuid,
    pub amount: i64,
    pub gateway: String,
    pub gateway_ref: Option<String>,
    pub order_code: i64,
    pub transfer_code: String,
    pub status: String,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct AdminDeposit {
    pub id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub amount: i64,
    pub gateway: String,
    pub order_code: i64,
    pub transfer_code: String,
    pub status: String,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

pub enum ConfirmSource<'a> {
    Webhook,
    Admin(AdminCtx<'a>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Confirmed { tx_id: Uuid },
    AlreadyConfirmed,
}

/// Tạo lệnh nạp ở trạng thái chờ. Chưa động tới ví.
pub async fn create(db: &PgPool, user_id: Uuid, amount: i64, gateway: &str) -> AppResult<Deposit> {
    if !(1..=MAX_DEPOSIT).contains(&amount) {
        return Err(AppError::bad_request(
            "invalid_amount",
            format!("Số tiền nạp phải từ 1đ đến {}.", format_vnd(MAX_DEPOSIT)),
        ));
    }
    let sql = format!(
        "WITH n AS (SELECT nextval('deposit_order_code_seq') AS oc) \
         INSERT INTO deposits (user_id, amount, gateway, order_code, transfer_code) \
         SELECT $1, $2, $3, oc, 'NAP' || oc::text FROM n RETURNING {COLS}"
    );
    let d = sqlx::query_as::<_, Deposit>(&sql)
        .bind(user_id)
        .bind(amount)
        .bind(gateway)
        .fetch_one(db)
        .await?;
    Ok(d)
}

pub async fn set_gateway_ref(db: &PgPool, deposit_id: Uuid, gateway_ref: &str) -> AppResult<()> {
    sqlx::query("UPDATE deposits SET gateway_ref = $2 WHERE id = $1")
        .bind(deposit_id)
        .bind(gateway_ref)
        .execute(db)
        .await?;
    Ok(())
}

/// Hủy lệnh nạp chưa trả tiền (ví dụ cổng thanh toán báo lỗi lúc tạo link).
pub async fn cancel_pending(db: &PgPool, deposit_id: Uuid) -> AppResult<()> {
    sqlx::query("UPDATE deposits SET status = 'cancelled' WHERE id = $1 AND status = 'pending'")
        .bind(deposit_id)
        .execute(db)
        .await?;
    Ok(())
}

pub async fn find_by_order_code(db: &PgPool, order_code: i64) -> AppResult<Option<Uuid>> {
    let id = sqlx::query_scalar::<_, Uuid>("SELECT id FROM deposits WHERE order_code = $1")
        .bind(order_code)
        .fetch_optional(db)
        .await?;
    Ok(id)
}

pub async fn list_for_user(db: &PgPool, user_id: Uuid, limit: i64) -> AppResult<Vec<Deposit>> {
    let sql =
        format!("SELECT {COLS} FROM deposits WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2");
    let rows = sqlx::query_as::<_, Deposit>(&sql)
        .bind(user_id)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

pub async fn list_admin(
    db: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> AppResult<Vec<AdminDeposit>> {
    let rows = sqlx::query_as::<_, AdminDeposit>(
        "SELECT d.id, d.user_id, u.email, d.amount, d.gateway, d.order_code, d.transfer_code, \
                d.status, d.confirmed_at, d.created_at \
         FROM deposits d JOIN users u ON u.id = d.user_id \
         WHERE ($1::text IS NULL OR d.status = $1) \
         ORDER BY d.created_at DESC LIMIT $2",
    )
    .bind(status)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Xác nhận một lệnh nạp và cộng tiền vào ví nạp (`wallet_spendable`).
///
/// An toàn khi gọi lặp hoặc đồng thời: hàng `deposits` bị khóa nên chỉ một lần được ghi sổ,
/// ngoài ra khóa idempotency `deposit:{id}` ở sổ cái chặn ghi trùng ở tầng thấp hơn.
/// `paid_amount` (nếu có, từ webhook) phải đúng bằng số tiền của lệnh nạp.
pub async fn confirm(
    db: &PgPool,
    deposit_id: Uuid,
    paid_amount: Option<i64>,
    source: ConfirmSource<'_>,
) -> AppResult<ConfirmOutcome> {
    let mut tx = db.begin().await?;
    if let ConfirmSource::Admin(ctx) = &source {
        require_admin(&mut tx, ctx.admin_id).await?;
    }
    let (user_id, amount, status) = sqlx::query_as::<_, (Uuid, i64, String)>(
        "SELECT user_id, amount, status FROM deposits WHERE id = $1 FOR UPDATE",
    )
    .bind(deposit_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy lệnh nạp."))?;

    match status.as_str() {
        "confirmed" => return Ok(ConfirmOutcome::AlreadyConfirmed),
        "pending" => {}
        _ => {
            return Err(AppError::conflict(
                "deposit_not_pending",
                "Lệnh nạp này đã bị hủy nên không thể xác nhận.",
            ))
        }
    }
    if let Some(paid) = paid_amount {
        if paid != amount {
            tracing::error!(%deposit_id, paid, amount, "số tiền thanh toán không khớp lệnh nạp");
            return Err(AppError::unprocessable(
                "amount_mismatch",
                format!(
                    "Số tiền thanh toán ({}) không khớp lệnh nạp ({}).",
                    format_vnd(paid),
                    format_vnd(amount)
                ),
            ));
        }
    }

    let ledger_tx = TxBuilder::new(kinds::DEPOSIT, format!("deposit:{deposit_id}"))
        .reference("deposit", deposit_id)
        .debit(AccountKey::external(), amount)
        .credit(AccountKey::wallet_spendable(user_id), amount)
        .build()?;
    let outcome = ledger::post(&mut tx, &ledger_tx).await?;

    let (confirmed_by, ip, via) = match &source {
        ConfirmSource::Webhook => (None, None, "webhook"),
        ConfirmSource::Admin(ctx) => (Some(ctx.admin_id), Some(ctx.ip), "admin"),
    };
    sqlx::query(
        "UPDATE deposits SET status = 'confirmed', confirmed_at = now(), confirmed_by = $2, \
         ledger_tx_id = $3 WHERE id = $1",
    )
    .bind(deposit_id)
    .bind(confirmed_by)
    .bind(outcome.tx_id())
    .execute(&mut *tx)
    .await?;
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: confirmed_by,
            action: "deposit.confirm",
            target_type: "deposit",
            target_id: Some(deposit_id.to_string()),
            before: Some(json!({ "status": "pending" })),
            after: Some(json!({ "status": "confirmed", "amount": amount, "via": via })),
            ip,
        },
    )
    .await?;
    notifications::create(
        &mut tx,
        user_id,
        "deposit_confirmed",
        &format!("Đã nạp {} vào ví.", format_vnd(amount)),
        Some(format!("deposit:{deposit_id}").as_str()),
    )
    .await?;
    tx.commit().await?;
    Ok(ConfirmOutcome::Confirmed {
        tx_id: outcome.tx_id(),
    })
}
