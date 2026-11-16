use crate::error::LedgerError;
use crate::Money;

/// Phí rút: `fee = (amount × fee_bps + 5000) / 10000`, toàn bằng số nguyên.
/// Tính bằng `i128` nên không tràn với mọi `amount` là `i64` hợp lệ.
pub fn withdraw_fee(amount: Money, fee_bps: i64) -> Result<Money, LedgerError> {
    if amount < 0 {
        return Err(LedgerError::InvalidAmount);
    }
    if !(0..=10_000).contains(&fee_bps) {
        return Err(LedgerError::InvalidFeeBps(fee_bps));
    }
    let fee = (i128::from(amount) * i128::from(fee_bps) + 5_000) / 10_000;
    Money::try_from(fee).map_err(|_| LedgerError::Overflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithdrawQuote {
    pub amount: Money,
    pub fee: Money,
    pub net: Money,
}

pub fn quote_withdrawal(amount: Money, fee_bps: i64) -> Result<WithdrawQuote, LedgerError> {
    let fee = withdraw_fee(amount, fee_bps)?;
    let net = amount.checked_sub(fee).ok_or(LedgerError::Overflow)?;
    Ok(WithdrawQuote { amount, fee, net })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn quote(amount: i64) -> WithdrawQuote {
        quote_withdrawal(amount, 1000).expect("quote")
    }

    #[test]
    fn ca_co_dinh() {
        assert_eq!(
            (quote(1_000_000).fee, quote(1_000_000).net),
            (100_000, 900_000)
        );
        // Lẻ đúng nửa đồng thì làm tròn lên.
        assert_eq!(
            (quote(1_000_005).fee, quote(1_000_005).net),
            (100_001, 900_004)
        );
        // Dưới nửa thì làm tròn xuống.
        assert_eq!(
            (quote(1_000_004).fee, quote(1_000_004).net),
            (100_000, 900_004)
        );
        assert_eq!((quote(0).fee, quote(0).net), (0, 0));
        assert_eq!((quote(4).fee, quote(4).net), (0, 4));
        assert_eq!((quote(5).fee, quote(5).net), (1, 4));
        assert_eq!(
            (quote(1_500_000).fee, quote(1_500_000).net),
            (150_000, 1_350_000)
        );
    }

    #[test]
    fn so_rat_lon_khong_tran() {
        let q = quote(i64::MAX);
        assert_eq!(q.fee, 922_337_203_685_477_581);
        assert_eq!(q.net, 8_301_034_833_169_298_226);
        assert_eq!(q.fee + q.net, i64::MAX);
        let q = quote(i64::MAX - 1);
        assert_eq!(q.fee, 922_337_203_685_477_581);
        assert_eq!(q.net, 8_301_034_833_169_298_225);
        // Phí 100% cũng không tràn.
        let all = quote_withdrawal(i64::MAX, 10_000).unwrap();
        assert_eq!((all.fee, all.net), (i64::MAX, 0));
        let none = quote_withdrawal(i64::MAX, 0).unwrap();
        assert_eq!((none.fee, none.net), (0, i64::MAX));
    }

    #[test]
    fn dau_vao_khong_hop_le() {
        assert!(matches!(
            withdraw_fee(-1, 1000),
            Err(LedgerError::InvalidAmount)
        ));
        assert!(matches!(
            withdraw_fee(100, -1),
            Err(LedgerError::InvalidFeeBps(-1))
        ));
        assert!(matches!(
            withdraw_fee(100, 10_001),
            Err(LedgerError::InvalidFeeBps(10_001))
        ));
    }

    proptest! {
        #[test]
        fn phi_cong_thuc_nhan_bang_so_tien(amount in 0i64..=i64::MAX, bps in 0i64..=10_000) {
            let q = quote_withdrawal(amount, bps).unwrap();
            prop_assert_eq!(q.fee + q.net, amount);
            prop_assert!(q.fee >= 0 && q.fee <= amount);
            prop_assert!(q.net >= 0 && q.net <= amount);
        }

        #[test]
        fn phi_dung_cong_thuc(amount in 0i64..=i64::MAX, bps in 0i64..=10_000) {
            let expected = (i128::from(amount) * i128::from(bps) + 5_000) / 10_000;
            prop_assert_eq!(i128::from(withdraw_fee(amount, bps).unwrap()), expected);
        }

        #[test]
        fn phi_khong_giam_khi_so_tien_tang(
            amount in 0i64..(i64::MAX / 2),
            delta in 0i64..1_000_000,
            bps in 0i64..=10_000,
        ) {
            let a = withdraw_fee(amount, bps).unwrap();
            let b = withdraw_fee(amount + delta, bps).unwrap();
            prop_assert!(a <= b);
        }
    }
}
