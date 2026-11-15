use uuid::Uuid;

use crate::error::LedgerError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AccountKind {
    WalletSpendable,
    EarningsPending,
    EarningsAvailable,
    Escrow,
    WithdrawalHold,
    PlatformRevenue,
    SellerDebt,
    External,
}

/// Tài khoản thuộc về ai: một người dùng, một đơn hàng, hay toàn hệ thống.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    User,
    Order,
    Global,
}

impl AccountKind {
    pub const ALL: [AccountKind; 8] = [
        AccountKind::WalletSpendable,
        AccountKind::EarningsPending,
        AccountKind::EarningsAvailable,
        AccountKind::Escrow,
        AccountKind::WithdrawalHold,
        AccountKind::PlatformRevenue,
        AccountKind::SellerDebt,
        AccountKind::External,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AccountKind::WalletSpendable => "wallet_spendable",
            AccountKind::EarningsPending => "earnings_pending",
            AccountKind::EarningsAvailable => "earnings_available",
            AccountKind::Escrow => "escrow",
            AccountKind::WithdrawalHold => "withdrawal_hold",
            AccountKind::PlatformRevenue => "platform_revenue",
            AccountKind::SellerDebt => "seller_debt",
            AccountKind::External => "external",
        }
    }

    pub fn parse(s: &str) -> Option<AccountKind> {
        AccountKind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Chỉ tài khoản `external` (phía ngoài hệ thống) được phép âm.
    pub fn allows_negative(self) -> bool {
        matches!(self, AccountKind::External)
    }

    fn scope(self) -> Scope {
        match self {
            AccountKind::WalletSpendable
            | AccountKind::EarningsPending
            | AccountKind::EarningsAvailable
            | AccountKind::WithdrawalHold
            | AccountKind::SellerDebt => Scope::User,
            AccountKind::Escrow => Scope::Order,
            AccountKind::PlatformRevenue | AccountKind::External => Scope::Global,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AccountKey {
    pub kind: AccountKind,
    pub user_id: Option<Uuid>,
    pub order_id: Option<Uuid>,
}

impl AccountKey {
    fn per_user(kind: AccountKind, user_id: Uuid) -> Self {
        Self {
            kind,
            user_id: Some(user_id),
            order_id: None,
        }
    }

    pub fn wallet_spendable(user_id: Uuid) -> Self {
        Self::per_user(AccountKind::WalletSpendable, user_id)
    }

    pub fn earnings_pending(user_id: Uuid) -> Self {
        Self::per_user(AccountKind::EarningsPending, user_id)
    }

    pub fn earnings_available(user_id: Uuid) -> Self {
        Self::per_user(AccountKind::EarningsAvailable, user_id)
    }

    pub fn withdrawal_hold(user_id: Uuid) -> Self {
        Self::per_user(AccountKind::WithdrawalHold, user_id)
    }

    pub fn seller_debt(user_id: Uuid) -> Self {
        Self::per_user(AccountKind::SellerDebt, user_id)
    }

    pub fn escrow(order_id: Uuid) -> Self {
        Self {
            kind: AccountKind::Escrow,
            user_id: None,
            order_id: Some(order_id),
        }
    }

    pub fn platform_revenue() -> Self {
        Self {
            kind: AccountKind::PlatformRevenue,
            user_id: None,
            order_id: None,
        }
    }

    pub fn external() -> Self {
        Self {
            kind: AccountKind::External,
            user_id: None,
            order_id: None,
        }
    }

    /// Kiểm tra khóa có đúng phạm vi của loại tài khoản (người dùng / đơn hàng / toàn cục).
    pub fn validate(&self) -> Result<(), LedgerError> {
        let ok = match self.kind.scope() {
            Scope::User => self.user_id.is_some() && self.order_id.is_none(),
            Scope::Order => self.order_id.is_some() && self.user_id.is_none(),
            Scope::Global => self.user_id.is_none() && self.order_id.is_none(),
        };
        if ok {
            Ok(())
        } else {
            Err(LedgerError::InvalidAccount(format!(
                "{} có phạm vi sai: {:?}",
                self.kind.as_str(),
                self
            )))
        }
    }

    /// Khóa chuỗi duy nhất của tài khoản, dùng làm `ledger_accounts.key`.
    pub fn key(&self) -> String {
        match (self.kind.scope(), self.user_id, self.order_id) {
            (Scope::User, Some(u), _) => format!("{}:{}", self.kind.as_str(), u),
            (Scope::Order, _, Some(o)) => format!("{}:{}", self.kind.as_str(), o),
            _ => self.kind.as_str().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn khoa_va_pham_vi() {
        let u = Uuid::new_v4();
        let o = Uuid::new_v4();
        assert_eq!(
            AccountKey::wallet_spendable(u).key(),
            format!("wallet_spendable:{u}")
        );
        assert_eq!(AccountKey::escrow(o).key(), format!("escrow:{o}"));
        assert_eq!(AccountKey::platform_revenue().key(), "platform_revenue");
        assert_eq!(AccountKey::external().key(), "external");
        for k in AccountKind::ALL {
            assert_eq!(AccountKind::parse(k.as_str()), Some(k));
        }
        let bad = AccountKey {
            kind: AccountKind::Escrow,
            user_id: Some(u),
            order_id: None,
        };
        assert!(bad.validate().is_err());
        let bad2 = AccountKey {
            kind: AccountKind::WalletSpendable,
            user_id: None,
            order_id: None,
        };
        assert!(bad2.validate().is_err());
        assert!(AccountKey::external().validate().is_ok());
        assert!(AccountKind::External.allows_negative());
        assert!(!AccountKind::WalletSpendable.allows_negative());
    }
}
