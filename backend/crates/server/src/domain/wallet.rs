use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::AppResult;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct WalletBalances {
    /// Ví nạp: chỉ để mua hàng và thuê gian hàng, không rút được.
    pub spendable: i64,
    pub earnings_pending: i64,
    pub earnings_available: i64,
    /// Tiền đã khóa cho các lệnh rút đang chờ duyệt.
    pub withdrawal_hold: i64,
    pub seller_debt: i64,
}

pub async fn balances(conn: &mut PgConnection, user_id: Uuid) -> AppResult<WalletBalances> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT a.kind, b.balance FROM ledger_accounts a \
         JOIN account_balances b ON b.account_id = a.id WHERE a.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(conn)
    .await?;
    let mut w = WalletBalances::default();
    for (kind, balance) in rows {
        match kind.as_str() {
            "wallet_spendable" => w.spendable = balance,
            "earnings_pending" => w.earnings_pending = balance,
            "earnings_available" => w.earnings_available = balance,
            "withdrawal_hold" => w.withdrawal_hold = balance,
            "seller_debt" => w.seller_debt = balance,
            _ => {}
        }
    }
    Ok(w)
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct EntryDto {
    pub id: i64,
    pub amount: i64,
    pub account_kind: String,
    pub tx_kind: String,
    pub ref_type: Option<String>,
    pub ref_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Lịch sử biến động của các ví của người dùng, mới nhất trước. `before_id` để phân trang.
pub async fn entries(
    db: &PgPool,
    user_id: Uuid,
    before_id: Option<i64>,
    limit: i64,
) -> AppResult<Vec<EntryDto>> {
    let rows = sqlx::query_as::<_, EntryDto>(
        "SELECT e.id, e.amount, a.kind AS account_kind, t.kind AS tx_kind, \
                t.ref_type, t.ref_id, e.created_at \
         FROM ledger_entries e \
         JOIN ledger_accounts a ON a.id = e.account_id \
         JOIN ledger_transactions t ON t.id = e.tx_id \
         WHERE a.user_id = $1 AND ($2::bigint IS NULL OR e.id < $2) \
         ORDER BY e.id DESC LIMIT $3",
    )
    .bind(user_id)
    .bind(before_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}
