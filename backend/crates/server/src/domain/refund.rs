//! Hoàn tiền nạp thừa: chỉ admin, bắt buộc có lý do, luôn ghi audit_log.
//! Bút toán: `wallet_spendable` của người dùng giảm, `external` tăng (tiền chuyển trả ra ngoài).

use ledger::{kinds, AccountKey, TxBuilder};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{idem_or_random, lock_user, notifications, post_or, require_admin, AdminCtx};
use crate::audit::{self, AuditEntry};
use crate::error::{AppError, AppResult};
use crate::util::format_vnd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefundOutcome {
    pub tx_id: Uuid,
    /// `true` nếu cùng Idempotency-Key đã xử lý trước đó (không trừ ví lần nữa).
    pub replayed: bool,
}

pub async fn refund_deposit(
    db: &PgPool,
    ctx: AdminCtx<'_>,
    user_id: Uuid,
    amount: i64,
    reason: &str,
    idem: Option<&str>,
) -> AppResult<RefundOutcome> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(AppError::bad_request(
            "reason_required",
            "Cần nhập lý do hoàn tiền (tối đa 500 ký tự).",
        ));
    }
    if amount <= 0 {
        return Err(AppError::bad_request(
            "invalid_amount",
            "Số tiền hoàn phải lớn hơn 0.",
        ));
    }
    let idem = idem_or_random(idem);
    let mut tx = db.begin().await?;
    require_admin(&mut tx, ctx.admin_id).await?;
    lock_user(&mut tx, user_id).await?;

    let before = ledger::store::balance_of(&mut tx, &AccountKey::wallet_spendable(user_id)).await?;
    let ledger_tx = TxBuilder::new(kinds::DEPOSIT_REFUND, format!("deposit_refund:{idem}"))
        .reference("user", user_id)
        .debit(AccountKey::wallet_spendable(user_id), amount)
        .credit(AccountKey::external(), amount)
        .build()?;
    let outcome = post_or(&mut tx, &ledger_tx, |balance| {
        AppError::unprocessable(
            "insufficient_funds",
            format!(
                "Ví của người dùng chỉ còn {}, không đủ để hoàn {}.",
                format_vnd(balance),
                format_vnd(amount)
            ),
        )
    })
    .await?;
    if !outcome.is_new() {
        return Ok(RefundOutcome {
            tx_id: outcome.tx_id(),
            replayed: true,
        });
    }
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(ctx.admin_id),
            action: "deposit.refund",
            target_type: "user",
            target_id: Some(user_id.to_string()),
            before: Some(json!({ "wallet_spendable": before })),
            after: Some(json!({
                "wallet_spendable": before - amount, "refunded": amount, "reason": reason
            })),
            ip: Some(ctx.ip),
        },
    )
    .await?;
    notifications::create(
        &mut tx,
        user_id,
        "deposit_refunded",
        &format!("Đã hoàn {} tiền nạp thừa về tài khoản ngân hàng của bạn.", format_vnd(amount)),
        Some(format!("deposit_refund:{idem}").as_str()),
    )
    .await?;
    tx.commit().await?;
    Ok(RefundOutcome {
        tx_id: outcome.tx_id(),
        replayed: false,
    })
}
