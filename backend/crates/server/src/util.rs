/// Định dạng tiền Việt: `99000` -> `99.000đ`.
pub fn format_vnd(amount: i64) -> String {
    let digits = amount.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    let mut out: String = grouped.chars().rev().collect();
    if amount < 0 {
        out.insert(0, '-');
    }
    format!("{out}đ")
}

#[cfg(test)]
mod tests {
    use super::format_vnd;

    #[test]
    fn dinh_dang_tien() {
        assert_eq!(format_vnd(0), "0đ");
        assert_eq!(format_vnd(999), "999đ");
        assert_eq!(format_vnd(99_000), "99.000đ");
        assert_eq!(format_vnd(1_000_000), "1.000.000đ");
        assert_eq!(format_vnd(-1_234_567), "-1.234.567đ");
    }
}
