//! Nghiệp vụ tiền: mỗi hàm công khai mở đúng một DB transaction, ghi sổ cái, rồi commit.
//! Khóa luôn lấy theo thứ tự: hàng người dùng (nếu có) -> hàng nghiệp vụ -> số dư tài khoản (id tăng dần).

use ledger::{LedgerError, LedgerTx, PostOutcome};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::{AppError, AppResult};

pub mod deposits;
pub mod escrow;
pub mod kyc;
pub mod notifications;
pub mod orders;
pub mod products;
pub mod refund;
pub mod review_queue;
pub mod stall;
pub mod wallet;
pub mod withdrawals;

/// Ngữ cảnh của một thao tác quản trị: ai làm, từ IP nào (để ghi audit_log).
#[derive(Debug, Clone, Copy)]
pub struct AdminCtx<'a> {
    pub admin_id: Uuid,
    pub ip: &'a str,
}

/// Chặn người không phải admin ngay ở tầng nghiệp vụ (không chỉ ở tầng HTTP).
pub async fn require_admin(conn: &mut PgConnection, user_id: Uuid) -> AppResult<()> {
    let is_admin = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
         WHERE ur.user_id = $1 AND r.name = 'admin')",
    )
    .bind(user_id)
    .fetch_one(conn)
    .await?;
    if is_admin {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "Chỉ quản trị viên được thực hiện thao tác này.",
        ))
    }
}

/// Khóa hàng người dùng để các thao tác tiền của cùng một người chạy tuần tự.
pub async fn lock_user(conn: &mut PgConnection, user_id: Uuid) -> AppResult<()> {
    let found = sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(user_id)
        .fetch_optional(conn)
        .await?;
    match found {
        Some(_) => Ok(()),
        None => Err(AppError::not_found("Không tìm thấy người dùng.")),
    }
}

/// Ghi giao dịch sổ cái; nếu thiếu số dư thì đổi lỗi chung thành lỗi nghiệp vụ cụ thể.
/// `on_insufficient` nhận số dư hiện có của tài khoản bị thiếu.
pub async fn post_or(
    conn: &mut PgConnection,
    tx: &LedgerTx,
    on_insufficient: impl FnOnce(i64) -> AppError,
) -> AppResult<PostOutcome> {
    match ledger::post(conn, tx).await {
        Ok(outcome) => Ok(outcome),
        Err(LedgerError::InsufficientFunds { balance, .. }) => Err(on_insufficient(balance)),
        Err(e) => Err(e.into()),
    }
}

/// Dùng Idempotency-Key của client, nếu không có thì sinh khóa ngẫu nhiên (không chống gửi lặp).
pub fn idem_or_random(idem: Option<&str>) -> String {
    match idem {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => Uuid::new_v4().to_string(),
    }
}
