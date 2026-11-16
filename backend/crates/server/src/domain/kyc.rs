//! KYC cơ bản cho người bán: họ tên, ngân hàng, số tài khoản (mã hóa AES-256-GCM).
//! Người bán rút tiền chỉ khi KYC đã được admin duyệt.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::{notifications, require_admin, AdminCtx};
use crate::audit::{self, AuditEntry};
use crate::crypto::KycCrypto;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
pub struct KycStatus {
    pub status: String,
    pub full_name: String,
    pub bank_name: String,
    /// Chỉ hiện 4 số cuối.
    pub account_no_masked: String,
    pub reject_reason: Option<String>,
    pub submitted_at: DateTime<Utc>,
}

fn mask_account(account_no: &str) -> String {
    let n = account_no.chars().count();
    let tail: String = account_no.chars().skip(n.saturating_sub(4)).collect();
    format!("{}{}", "*".repeat(n.saturating_sub(4)), tail)
}

fn clean_text(label: &str, value: &str, max: usize) -> AppResult<String> {
    let v = value.trim();
    let n = v.chars().count();
    if !(2..=max).contains(&n) || v.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "invalid_kyc",
            format!("{label} phải dài từ 2 đến {max} ký tự."),
        ));
    }
    Ok(v.to_string())
}

/// Số tài khoản: chỉ chữ số (bỏ khoảng trắng), dài 6 đến 20.
fn clean_account_no(value: &str) -> AppResult<String> {
    let v: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if !(6..=20).contains(&v.len()) || !v.chars().all(|c| c.is_ascii_digit()) {
        return Err(AppError::bad_request(
            "invalid_kyc",
            "Số tài khoản chỉ gồm chữ số, dài từ 6 đến 20.",
        ));
    }
    Ok(v)
}

/// Gửi (hoặc gửi lại sau khi bị từ chối) thông tin KYC. KYC đã duyệt thì không sửa được qua API.
pub async fn submit(
    db: &PgPool,
    crypto: &KycCrypto,
    user_id: Uuid,
    full_name: &str,
    bank_name: &str,
    account_no: &str,
) -> AppResult<KycStatus> {
    let full_name = clean_text("Họ tên", full_name, 100)?;
    let bank_name = clean_text("Tên ngân hàng", bank_name, 100)?;
    let account_no = clean_account_no(account_no)?;
    let (enc, key_id) = crypto.encrypt(&account_no)?;

    let res = sqlx::query(
        "INSERT INTO seller_kyc (user_id, full_name, bank_name, account_no_enc, key_id) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (user_id) DO UPDATE SET \
            full_name = EXCLUDED.full_name, bank_name = EXCLUDED.bank_name, \
            account_no_enc = EXCLUDED.account_no_enc, key_id = EXCLUDED.key_id, \
            status = 'pending', reject_reason = NULL, reviewed_by = NULL, reviewed_at = NULL, \
            submitted_at = now() \
         WHERE seller_kyc.status <> 'approved'",
    )
    .bind(user_id)
    .bind(full_name)
    .bind(bank_name)
    .bind(enc)
    .bind(key_id)
    .execute(db)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::conflict(
            "kyc_approved_locked",
            "KYC đã được duyệt nên không tự sửa được. Hãy liên hệ quản trị viên nếu cần đổi tài khoản nhận tiền.",
        ));
    }
    get(db, crypto, user_id)
        .await?
        .ok_or_else(|| AppError::internal("KYC vừa lưu nhưng không đọc lại được"))
}

pub async fn get(db: &PgPool, crypto: &KycCrypto, user_id: Uuid) -> AppResult<Option<KycStatus>> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Vec<u8>,
            String,
            Option<String>,
            DateTime<Utc>,
        ),
    >(
        "SELECT status, full_name, bank_name, account_no_enc, key_id, reject_reason, submitted_at \
         FROM seller_kyc WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    let Some((status, full_name, bank_name, enc, key_id, reject_reason, submitted_at)) = row else {
        return Ok(None);
    };
    let account_no = crypto.decrypt(&key_id, &enc)?;
    Ok(Some(KycStatus {
        status,
        full_name,
        bank_name,
        account_no_masked: mask_account(&account_no),
        reject_reason,
        submitted_at,
    }))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminKycRow {
    pub user_id: Uuid,
    pub email: String,
    pub full_name: String,
    pub bank_name: String,
    pub account_no_enc: Vec<u8>,
    pub key_id: String,
    pub status: String,
    pub reject_reason: Option<String>,
    pub submitted_at: DateTime<Utc>,
}

pub async fn list_admin(
    db: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> AppResult<Vec<AdminKycRow>> {
    let rows = sqlx::query_as::<_, AdminKycRow>(
        "SELECT k.user_id, u.email, k.full_name, k.bank_name, k.account_no_enc, k.key_id, \
                k.status, k.reject_reason, k.submitted_at \
         FROM seller_kyc k JOIN users u ON u.id = k.user_id \
         WHERE ($1::text IS NULL OR k.status = $1) \
         ORDER BY k.submitted_at DESC LIMIT $2",
    )
    .bind(status)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

async fn review(
    db: &PgPool,
    ctx: AdminCtx<'_>,
    user_id: Uuid,
    approve: bool,
    reason: Option<&str>,
) -> AppResult<()> {
    let mut tx = db.begin().await?;
    require_admin(&mut tx, ctx.admin_id).await?;
    let new_status = if approve { "approved" } else { "rejected" };
    let res = sqlx::query(
        "UPDATE seller_kyc SET status = $2, reject_reason = $3, reviewed_by = $4, \
         reviewed_at = now() WHERE user_id = $1 AND status = 'pending'",
    )
    .bind(user_id)
    .bind(new_status)
    .bind(reason)
    .bind(ctx.admin_id)
    .execute(&mut *tx)
    .await?;
    if res.rows_affected() == 0 {
        return Err(AppError::conflict(
            "kyc_not_pending",
            "Không có hồ sơ KYC đang chờ duyệt của người dùng này.",
        ));
    }
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(ctx.admin_id),
            action: if approve { "kyc.approve" } else { "kyc.reject" },
            target_type: "seller_kyc",
            target_id: Some(user_id.to_string()),
            before: Some(json!({ "status": "pending" })),
            after: Some(json!({ "status": new_status, "reason": reason })),
            ip: Some(ctx.ip),
        },
    )
    .await?;
    let body = if approve {
        "KYC của bạn đã được duyệt. Bạn có thể tạo lệnh rút tiền.".to_string()
    } else {
        format!(
            "KYC của bạn bị từ chối: {}. Hãy sửa thông tin và gửi lại.",
            reason.unwrap_or("")
        )
    };
    notifications::create(&mut tx, user_id, "kyc_reviewed", &body, None).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn approve(db: &PgPool, ctx: AdminCtx<'_>, user_id: Uuid) -> AppResult<()> {
    review(db, ctx, user_id, true, None).await
}

pub async fn reject(db: &PgPool, ctx: AdminCtx<'_>, user_id: Uuid, reason: &str) -> AppResult<()> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(AppError::bad_request(
            "reason_required",
            "Cần nhập lý do từ chối (tối đa 500 ký tự).",
        ));
    }
    review(db, ctx, user_id, false, Some(reason)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn che_so_tai_khoan() {
        assert_eq!(mask_account("0123456789"), "******6789");
        assert_eq!(mask_account("123"), "123");
        assert_eq!(mask_account(""), "");
    }

    #[test]
    fn kiem_tra_dau_vao() {
        assert_eq!(clean_account_no(" 0123 4567 89 ").unwrap(), "0123456789");
        assert!(clean_account_no("12345").is_err());
        assert!(clean_account_no("12345a7890").is_err());
        assert!(clean_account_no(&"1".repeat(21)).is_err());
        assert!(clean_text("Họ tên", "A", 100).is_err());
        assert_eq!(
            clean_text("Họ tên", "  Nguyễn Văn A ", 100).unwrap(),
            "Nguyễn Văn A"
        );
    }
}
