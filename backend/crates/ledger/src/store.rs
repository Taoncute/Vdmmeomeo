//! Ghi bút toán vào Postgres. Mọi hàm nhận `&mut PgConnection` để người gọi tự quyết định
//! DB transaction (mỗi hàm nghiệp vụ một transaction). Nếu `post` trả lỗi thì người gọi phải hủy
//! transaction; đừng commit tiếp.

use std::collections::HashSet;

use sqlx::PgConnection;
use uuid::Uuid;

use crate::account::AccountKey;
use crate::builder::LedgerTx;
use crate::error::LedgerError;
use crate::Money;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostOutcome {
    /// Giao dịch mới được ghi.
    Created { tx_id: Uuid },
    /// `idempotency_key` đã dùng trước đó: không ghi lại, không đổi số dư.
    AlreadyPosted { tx_id: Uuid },
}

impl PostOutcome {
    pub fn tx_id(&self) -> Uuid {
        match self {
            PostOutcome::Created { tx_id } | PostOutcome::AlreadyPosted { tx_id } => *tx_id,
        }
    }

    pub fn is_new(&self) -> bool {
        matches!(self, PostOutcome::Created { .. })
    }
}

/// Lấy id tài khoản, tạo (cùng dòng `account_balances`) nếu chưa có.
pub async fn ensure_account(
    conn: &mut PgConnection,
    key: &AccountKey,
) -> Result<Uuid, LedgerError> {
    key.validate()?;
    let k = key.key();
    sqlx::query(
        "INSERT INTO ledger_accounts (key, kind, user_id, order_id) VALUES ($1, $2, $3, $4) \
         ON CONFLICT (key) DO NOTHING",
    )
    .bind(&k)
    .bind(key.kind.as_str())
    .bind(key.user_id)
    .bind(key.order_id)
    .execute(&mut *conn)
    .await?;
    let id = sqlx::query_scalar::<_, Uuid>("SELECT id FROM ledger_accounts WHERE key = $1")
        .bind(&k)
        .fetch_one(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO account_balances (account_id, allow_negative) VALUES ($1, $2) \
         ON CONFLICT (account_id) DO NOTHING",
    )
    .bind(id)
    .bind(key.kind.allows_negative())
    .execute(&mut *conn)
    .await?;
    Ok(id)
}

/// Ghi một giao dịch: kiểm idempotency, khóa số dư theo thứ tự id tăng dần, kiểm không âm,
/// ghi bút toán và cập nhật `account_balances`, tất cả trong transaction của người gọi.
pub async fn post(conn: &mut PgConnection, tx: &LedgerTx) -> Result<PostOutcome, LedgerError> {
    // Kiểm lại phòng thủ vì `LedgerTx` có thể được dựng tay, không qua `TxBuilder`.
    let sum: i128 = tx.entries.iter().map(|e| i128::from(e.amount)).sum();
    if sum != 0 {
        return Err(LedgerError::Unbalanced(sum));
    }
    if tx.entries.len() < 2 {
        return Err(LedgerError::TooFewEntries);
    }
    let mut seen = HashSet::new();
    for e in &tx.entries {
        if e.amount == 0 {
            return Err(LedgerError::InvalidAmount);
        }
        if !seen.insert(e.account) {
            return Err(LedgerError::DuplicateAccount);
        }
    }
    if tx.idempotency_key.trim().is_empty() {
        return Err(LedgerError::MissingIdempotencyKey);
    }

    // 1. Idempotency: nếu có giao dịch khác đang ghi cùng key, câu INSERT chờ cho tới khi nó commit.
    let inserted = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO ledger_transactions (kind, idempotency_key, ref_type, ref_id) \
         VALUES ($1, $2, $3, $4) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
    )
    .bind(&tx.kind)
    .bind(&tx.idempotency_key)
    .bind(&tx.ref_type)
    .bind(tx.ref_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(tx_id) = inserted else {
        let existing = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM ledger_transactions WHERE idempotency_key = $1",
        )
        .bind(&tx.idempotency_key)
        .fetch_one(&mut *conn)
        .await?;
        return Ok(PostOutcome::AlreadyPosted { tx_id: existing });
    };

    // 2. Tạo hết tài khoản trước khi khóa bất kỳ số dư nào (tránh deadlock do tạo tài khoản).
    let mut ids = Vec::with_capacity(tx.entries.len());
    for e in &tx.entries {
        ids.push(ensure_account(&mut *conn, &e.account).await?);
    }

    // 3. Khóa số dư theo thứ tự id tăng dần, tính số dư mới và kiểm không âm.
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.sort_by_key(|&i| ids[i]);
    let mut updates: Vec<(Uuid, Money)> = Vec::with_capacity(ids.len());
    for i in order {
        let (balance, allow_negative) = sqlx::query_as::<_, (i64, bool)>(
            "SELECT balance, allow_negative FROM account_balances WHERE account_id = $1 FOR UPDATE",
        )
        .bind(ids[i])
        .fetch_one(&mut *conn)
        .await?;
        let entry = &tx.entries[i];
        let new_balance = balance
            .checked_add(entry.amount)
            .ok_or(LedgerError::Overflow)?;
        if new_balance < 0 && !allow_negative {
            return Err(LedgerError::InsufficientFunds {
                account: entry.account,
                balance,
                needed: entry.amount.saturating_neg(),
            });
        }
        updates.push((ids[i], new_balance));
    }

    // 4. Ghi bút toán.
    for (entry, account_id) in tx.entries.iter().zip(&ids) {
        sqlx::query("INSERT INTO ledger_entries (tx_id, account_id, amount) VALUES ($1, $2, $3)")
            .bind(tx_id)
            .bind(*account_id)
            .bind(entry.amount)
            .execute(&mut *conn)
            .await?;
    }

    // 5. Cập nhật số dư.
    for (account_id, new_balance) in updates {
        sqlx::query(
            "UPDATE account_balances SET balance = $2, version = version + 1, updated_at = now() \
             WHERE account_id = $1",
        )
        .bind(account_id)
        .bind(new_balance)
        .execute(&mut *conn)
        .await?;
    }
    Ok(PostOutcome::Created { tx_id })
}

/// Số dư hiện tại của tài khoản (0 nếu tài khoản chưa tồn tại). Không khóa.
pub async fn balance_of(conn: &mut PgConnection, key: &AccountKey) -> Result<Money, LedgerError> {
    let balance = sqlx::query_scalar::<_, i64>(
        "SELECT b.balance FROM account_balances b \
         JOIN ledger_accounts a ON a.id = b.account_id WHERE a.key = $1",
    )
    .bind(key.key())
    .fetch_optional(conn)
    .await?;
    Ok(balance.unwrap_or(0))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub account_id: Uuid,
    pub key: String,
    pub balance: Money,
    pub entries_sum: Money,
}

/// Kết quả đối soát toàn sổ.
#[derive(Debug, Clone, Default)]
pub struct BooksReport {
    /// Tổng mọi bút toán toàn sổ (phải bằng 0).
    pub total: Money,
    /// Giao dịch có tổng bút toán khác 0.
    pub unbalanced_txs: Vec<Uuid>,
    /// Tài khoản có `balance` khác tổng `entries`.
    pub mismatches: Vec<Mismatch>,
    /// Tài khoản âm dù không được phép âm.
    pub negative_accounts: Vec<String>,
}

impl BooksReport {
    pub fn is_clean(&self) -> bool {
        self.total == 0
            && self.unbalanced_txs.is_empty()
            && self.mismatches.is_empty()
            && self.negative_accounts.is_empty()
    }
}

/// Đối soát: tổng toàn sổ, từng giao dịch, `balance` so với tổng `entries`, và ví âm.
pub async fn verify_books(conn: &mut PgConnection) -> Result<BooksReport, LedgerError> {
    let total =
        sqlx::query_scalar::<_, i64>("SELECT COALESCE(SUM(amount), 0)::bigint FROM ledger_entries")
            .fetch_one(&mut *conn)
            .await?;
    let unbalanced_txs = sqlx::query_scalar::<_, Uuid>(
        "SELECT tx_id FROM ledger_entries GROUP BY tx_id HAVING SUM(amount) <> 0",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mismatches = sqlx::query_as::<_, (Uuid, String, i64, i64)>(
        "SELECT b.account_id, a.key, b.balance, COALESCE(SUM(e.amount), 0)::bigint \
         FROM account_balances b \
         JOIN ledger_accounts a ON a.id = b.account_id \
         LEFT JOIN ledger_entries e ON e.account_id = b.account_id \
         GROUP BY b.account_id, a.key, b.balance \
         HAVING b.balance <> COALESCE(SUM(e.amount), 0)",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .map(|(account_id, key, balance, entries_sum)| Mismatch {
        account_id,
        key,
        balance,
        entries_sum,
    })
    .collect();
    let negative_accounts = sqlx::query_scalar::<_, String>(
        "SELECT a.key FROM account_balances b JOIN ledger_accounts a ON a.id = b.account_id \
         WHERE b.balance < 0 AND NOT b.allow_negative",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(BooksReport {
        total,
        unbalanced_txs,
        mismatches,
        negative_accounts,
    })
}
