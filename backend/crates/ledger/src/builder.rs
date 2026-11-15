use std::collections::HashSet;

use uuid::Uuid;

use crate::account::AccountKey;
use crate::error::LedgerError;
use crate::Money;

/// Các loại giao dịch sổ cái (`ledger_transactions.kind`).
pub mod kinds {
    pub const DEPOSIT: &str = "deposit";
    pub const ESCROW_HOLD: &str = "escrow_hold";
    pub const ESCROW_COMPLETE: &str = "escrow_complete";
    pub const EARNINGS_RELEASE: &str = "earnings_release";
    pub const REFUND_FROM_ESCROW: &str = "refund_from_escrow";
    pub const REFUND_FROM_PENDING: &str = "refund_from_pending";
    pub const WITHDRAWAL_CREATE: &str = "withdrawal_create";
    pub const WITHDRAWAL_APPROVE: &str = "withdrawal_approve";
    pub const WITHDRAWAL_REJECT: &str = "withdrawal_reject";
    pub const STALL_RENT: &str = "stall_rent";
    pub const DEPOSIT_REFUND: &str = "deposit_refund";
    pub const SEED: &str = "seed";
}

/// Một bút toán: số dương cộng vào tài khoản, số âm trừ đi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub account: AccountKey,
    pub amount: Money,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerTx {
    pub kind: String,
    pub idempotency_key: String,
    pub ref_type: Option<String>,
    pub ref_id: Option<Uuid>,
    pub entries: Vec<Entry>,
}

/// Bộ dựng giao dịch. `build()` bảo đảm tổng các bút toán bằng 0.
pub struct TxBuilder {
    kind: String,
    idempotency_key: String,
    ref_type: Option<String>,
    ref_id: Option<Uuid>,
    entries: Vec<Entry>,
    invalid_amount: bool,
}

impl TxBuilder {
    pub fn new(kind: &str, idempotency_key: impl Into<String>) -> Self {
        Self {
            kind: kind.to_string(),
            idempotency_key: idempotency_key.into(),
            ref_type: None,
            ref_id: None,
            entries: Vec::new(),
            invalid_amount: false,
        }
    }

    pub fn reference(mut self, ref_type: &str, ref_id: Uuid) -> Self {
        self.ref_type = Some(ref_type.to_string());
        self.ref_id = Some(ref_id);
        self
    }

    /// Trừ `amount` (phải > 0) khỏi tài khoản.
    pub fn debit(mut self, account: AccountKey, amount: Money) -> Self {
        match amount.checked_neg() {
            Some(neg) if amount > 0 => self.entries.push(Entry {
                account,
                amount: neg,
            }),
            _ => self.invalid_amount = true,
        }
        self
    }

    /// Cộng `amount` (phải > 0) vào tài khoản.
    pub fn credit(mut self, account: AccountKey, amount: Money) -> Self {
        if amount > 0 {
            self.entries.push(Entry { account, amount });
        } else {
            self.invalid_amount = true;
        }
        self
    }

    pub fn build(self) -> Result<LedgerTx, LedgerError> {
        if self.invalid_amount {
            return Err(LedgerError::InvalidAmount);
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(LedgerError::MissingIdempotencyKey);
        }
        if self.entries.len() < 2 {
            return Err(LedgerError::TooFewEntries);
        }
        let mut seen = HashSet::new();
        let mut sum: i128 = 0;
        for e in &self.entries {
            e.account.validate()?;
            if e.amount == 0 {
                return Err(LedgerError::InvalidAmount);
            }
            if !seen.insert(e.account) {
                return Err(LedgerError::DuplicateAccount);
            }
            sum += i128::from(e.amount);
        }
        if sum != 0 {
            return Err(LedgerError::Unbalanced(sum));
        }
        Ok(LedgerTx {
            kind: self.kind,
            idempotency_key: self.idempotency_key,
            ref_type: self.ref_type,
            ref_id: self.ref_id,
            entries: self.entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn giao_dich_can_bang() {
        let u = Uuid::new_v4();
        let tx = TxBuilder::new(kinds::DEPOSIT, "deposit:1")
            .debit(AccountKey::external(), 1000)
            .credit(AccountKey::wallet_spendable(u), 1000)
            .build()
            .unwrap();
        assert_eq!(tx.entries.iter().map(|e| e.amount).sum::<i64>(), 0);
        assert_eq!(tx.entries.len(), 2);
    }

    #[test]
    fn tu_choi_giao_dich_sai() {
        let u = Uuid::new_v4();
        let w = AccountKey::wallet_spendable(u);
        // Lệch tổng.
        let r = TxBuilder::new("x", "k")
            .debit(AccountKey::external(), 100)
            .credit(w, 99)
            .build();
        assert!(matches!(r, Err(LedgerError::Unbalanced(-1))));
        // Chỉ một bút toán.
        let r = TxBuilder::new("x", "k").credit(w, 1).build();
        assert!(matches!(r, Err(LedgerError::TooFewEntries)));
        // Số tiền 0 hoặc âm.
        let r = TxBuilder::new("x", "k")
            .debit(AccountKey::external(), 0)
            .credit(w, 0)
            .build();
        assert!(matches!(r, Err(LedgerError::InvalidAmount)));
        let r = TxBuilder::new("x", "k")
            .debit(AccountKey::external(), -5)
            .credit(w, 5)
            .build();
        assert!(matches!(r, Err(LedgerError::InvalidAmount)));
        // Trùng tài khoản.
        let r = TxBuilder::new("x", "k").debit(w, 5).credit(w, 5).build();
        assert!(matches!(r, Err(LedgerError::DuplicateAccount)));
        // Thiếu idempotency key.
        let r = TxBuilder::new("x", "  ")
            .debit(AccountKey::external(), 5)
            .credit(w, 5)
            .build();
        assert!(matches!(r, Err(LedgerError::MissingIdempotencyKey)));
        // Sai phạm vi tài khoản.
        let bad = AccountKey {
            kind: crate::AccountKind::Escrow,
            user_id: Some(u),
            order_id: None,
        };
        let r = TxBuilder::new("x", "k")
            .debit(AccountKey::external(), 5)
            .credit(bad, 5)
            .build();
        assert!(matches!(r, Err(LedgerError::InvalidAccount(_))));
    }

    proptest! {
        #[test]
        fn chuyen_khoan_luon_can_bang(amount in 1i64..=i64::MAX) {
            let a = AccountKey::wallet_spendable(Uuid::new_v4());
            let b = AccountKey::earnings_available(Uuid::new_v4());
            let tx = TxBuilder::new("x", "k").debit(a, amount).credit(b, amount).build().unwrap();
            let sum: i128 = tx.entries.iter().map(|e| i128::from(e.amount)).sum();
            prop_assert_eq!(sum, 0);
        }

        #[test]
        fn ba_ben_van_can_bang_con_lech_thi_bi_tu_choi(
            x in 1i64..1_000_000_000,
            y in 1i64..1_000_000_000,
            skew in 1i64..1_000,
        ) {
            let src = AccountKey::earnings_available(Uuid::new_v4());
            let fee = AccountKey::platform_revenue();
            let out = AccountKey::external();
            let ok = TxBuilder::new("x", "k")
                .debit(src, x + y).credit(fee, x).credit(out, y).build();
            prop_assert!(ok.is_ok());
            let bad = TxBuilder::new("x", "k")
                .debit(src, x + y + skew).credit(fee, x).credit(out, y).build();
            prop_assert!(matches!(bad, Err(LedgerError::Unbalanced(_))));
        }
    }
}
