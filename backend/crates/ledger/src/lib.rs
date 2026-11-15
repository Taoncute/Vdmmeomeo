//! Lõi sổ cái: kiểu tiền, loại tài khoản, bộ dựng bút toán, phí rút và lớp ghi vào Postgres.
//!
//! Crate này không phụ thuộc HTTP. Quy tắc nghiệp vụ (ai được làm gì) nằm ở crate `server`;
//! ở đây chỉ bảo đảm sổ luôn cân bằng, bất biến và không ví nào âm.

pub mod account;
pub mod builder;
pub mod error;
pub mod fee;
pub mod store;

pub use account::{AccountKey, AccountKind};
pub use builder::{kinds, Entry, LedgerTx, TxBuilder};
pub use error::LedgerError;
pub use fee::{quote_withdrawal, withdraw_fee, WithdrawQuote};
pub use store::{post, PostOutcome};

/// Tiền luôn là số nguyên `i64` theo đơn vị đồng. Không dùng số thực.
pub type Money = i64;
