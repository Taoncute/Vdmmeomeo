//! Lõi sổ cái (khung ở phase 1, phase 2 bổ sung kiểu tài khoản, bút toán, phí).
//!
//! Crate này không phụ thuộc HTTP.

/// Tiền luôn là số nguyên `i64` theo đơn vị đồng. Không dùng số thực.
pub type Money = i64;
