use crate::account::AccountKey;
use crate::Money;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("số tiền không hợp lệ")]
    InvalidAmount,
    #[error("tỷ lệ phí không hợp lệ: {0} (phải từ 0 đến 10000 basis point)")]
    InvalidFeeBps(i64),
    #[error("tràn số khi tính tiền")]
    Overflow,
    #[error("bút toán không cân bằng, tổng = {0}")]
    Unbalanced(i128),
    #[error("giao dịch phải có ít nhất 2 bút toán")]
    TooFewEntries,
    #[error("một tài khoản xuất hiện nhiều lần trong cùng giao dịch")]
    DuplicateAccount,
    #[error("tài khoản không hợp lệ: {0}")]
    InvalidAccount(String),
    #[error("thiếu idempotency_key")]
    MissingIdempotencyKey,
    #[error("không đủ số dư ở {account:?}: có {balance}, cần {needed}")]
    InsufficientFunds {
        account: AccountKey,
        balance: Money,
        needed: Money,
    },
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}
