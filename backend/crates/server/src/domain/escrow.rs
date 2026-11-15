//! Escrow và doanh thu người bán (dùng ở phase 3 khi có đơn hàng):
//! `wallet_spendable` -> `escrow` -> `earnings_pending` -> `earnings_available`,
//! cùng hai đường hoàn tiền về ví người mua.

use ledger::{kinds, AccountKey, LedgerError, PostOutcome, TxBuilder};
use sqlx::PgPool;
use uuid::Uuid;

use super::{lock_user, post_or};
use crate::error::{AppError, AppResult};
use crate::settings;
use crate::util::format_vnd;

/// Người mua trả tiền vào escrow của đơn hàng.
pub async fn hold(
    db: &PgPool,
    buyer_id: Uuid,
    order_id: Uuid,
    amount: i64,
) -> AppResult<PostOutcome> {
    let tx_def = TxBuilder::new(kinds::ESCROW_HOLD, format!("escrow:hold:{order_id}"))
        .reference("order", order_id)
        .debit(AccountKey::wallet_spendable(buyer_id), amount)
        .credit(AccountKey::escrow(order_id), amount)
        .build()?;
    let mut tx = db.begin().await?;
    lock_user(&mut tx, buyer_id).await?;
    let outcome = post_or(&mut tx, &tx_def, |balance| {
        AppError::unprocessable(
            "insufficient_funds",
            format!(
                "Ví không đủ để thanh toán: cần {}, hiện có {}.",
                format_vnd(amount),
                format_vnd(balance)
            ),
        )
    })
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Đơn hoàn tất: tiền từ escrow sang `earnings_pending` của người bán và bị giữ `hold_days` ngày.
pub async fn complete(
    db: &PgPool,
    seller_id: Uuid,
    order_id: Uuid,
    amount: i64,
) -> AppResult<PostOutcome> {
    let tx_def = TxBuilder::new(kinds::ESCROW_COMPLETE, format!("escrow:complete:{order_id}"))
        .reference("order", order_id)
        .debit(AccountKey::escrow(order_id), amount)
        .credit(AccountKey::earnings_pending(seller_id), amount)
        .build()?;
    let mut tx = db.begin().await?;
    let cfg = settings::load(&mut tx).await?;
    let outcome = post_or(&mut tx, &tx_def, |_| {
        AppError::conflict(
            "escrow_empty",
            "Escrow của đơn hàng này không còn đủ tiền (đã hoàn tiền hoặc đã chốt).",
        )
    })
    .await?;
    if outcome.is_new() {
        sqlx::query(
            "INSERT INTO earnings_holds (seller_id, order_ref, amount, release_at) \
             VALUES ($1, $2, $3, now() + ($4::bigint * 86400 * interval '1 second'))",
        )
        .bind(seller_id)
        .bind(order_id)
        .bind(amount)
        .bind(cfg.hold_days)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    Released,
    /// Khoản giữ đã được nhả hoặc đã hoàn lại trước đó.
    NotPending,
}

/// Nhả một khoản giữ: `earnings_pending` -> `earnings_available`.
pub async fn release_hold(db: &PgPool, hold_id: Uuid) -> AppResult<ReleaseOutcome> {
    let mut tx = db.begin().await?;
    let row = sqlx::query_as::<_, (Uuid, i64, String)>(
        "SELECT seller_id, amount, status FROM earnings_holds WHERE id = $1 FOR UPDATE",
    )
    .bind(hold_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy khoản giữ doanh thu."))?;
    let (seller_id, amount, status) = row;
    if status != "pending" {
        return Ok(ReleaseOutcome::NotPending);
    }
    let tx_def = TxBuilder::new(kinds::EARNINGS_RELEASE, format!("escrow:release:{hold_id}"))
        .reference("earnings_hold", hold_id)
        .debit(AccountKey::earnings_pending(seller_id), amount)
        .credit(AccountKey::earnings_available(seller_id), amount)
        .build()?;
    ledger::post(&mut tx, &tx_def).await?;
    sqlx::query("UPDATE earnings_holds SET status = 'released', released_at = now() WHERE id = $1")
        .bind(hold_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ReleaseOutcome::Released)
}

/// Job: nhả mọi khoản giữ đã đến hạn. Trả về số khoản đã nhả.
pub async fn release_due(db: &PgPool, limit: i64) -> AppResult<usize> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM earnings_holds WHERE status = 'pending' AND release_at <= now() \
         ORDER BY release_at LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut released = 0usize;
    for id in ids {
        if release_hold(db, id).await? == ReleaseOutcome::Released {
            released += 1;
        }
    }
    Ok(released)
}

/// Hoàn tiền khi tiền còn ở escrow: `escrow` -> `wallet_spendable` của người mua.
pub async fn refund_from_escrow(
    db: &PgPool,
    buyer_id: Uuid,
    order_id: Uuid,
    amount: i64,
) -> AppResult<PostOutcome> {
    let tx_def = TxBuilder::new(kinds::REFUND_FROM_ESCROW, format!("escrow:refund:{order_id}"))
        .reference("order", order_id)
        .debit(AccountKey::escrow(order_id), amount)
        .credit(AccountKey::wallet_spendable(buyer_id), amount)
        .build()?;
    let mut tx = db.begin().await?;
    let outcome = post_or(&mut tx, &tx_def, |_| {
        AppError::conflict(
            "refund_not_possible",
            "Tiền không còn ở escrow của đơn này nên không hoàn tự động được.",
        )
    })
    .await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Hoàn tiền khi tiền đang ở `earnings_pending`: về ví người mua và đánh dấu khoản giữ đã hoàn.
/// Tiền đã sang `earnings_available` thì từ chối: phải xử lý bằng tay (công nợ người bán, phase sau).
pub async fn refund_from_pending(
    db: &PgPool,
    seller_id: Uuid,
    buyer_id: Uuid,
    order_id: Uuid,
    amount: i64,
) -> AppResult<PostOutcome> {
    let mut tx = db.begin().await?;
    let (hold_id, hold_amount, status) = sqlx::query_as::<_, (Uuid, i64, String)>(
        "SELECT id, amount, status FROM earnings_holds WHERE order_ref = $1 FOR UPDATE",
    )
    .bind(order_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        AppError::conflict(
            "hold_not_found",
            "Đơn hàng này chưa có khoản doanh thu đang giữ.",
        )
    })?;
    if status != "pending" {
        return Err(AppError::conflict(
            "refund_after_release",
            "Tiền đã chuyển sang doanh thu rút được (hoặc đã hoàn), không hoàn tự động được.",
        ));
    }
    if hold_amount != amount {
        return Err(AppError::bad_request(
            "amount_mismatch",
            "Số tiền hoàn phải bằng đúng khoản doanh thu đang giữ của đơn.",
        ));
    }
    let tx_def = TxBuilder::new(
        kinds::REFUND_FROM_PENDING,
        format!("escrow:refund_pending:{order_id}"),
    )
    .reference("order", order_id)
    .debit(AccountKey::earnings_pending(seller_id), amount)
    .credit(AccountKey::wallet_spendable(buyer_id), amount)
    .build()?;
    let outcome = match ledger::post(&mut tx, &tx_def).await {
        Ok(o) => o,
        Err(LedgerError::InsufficientFunds { .. }) => {
            return Err(AppError::conflict(
                "refund_not_possible",
                "Doanh thu đang giữ của người bán không đủ để hoàn.",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    sqlx::query("UPDATE earnings_holds SET status = 'refunded' WHERE id = $1")
        .bind(hold_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(outcome)
}
