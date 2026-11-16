//! Thuê gian hàng: trả từ ví nạp (`spendable`) hoặc doanh thu rút được (`earnings`).
//! Lần thuê đầu tạo gian hàng (bắt buộc `shop_name`) và cấp vai `seller`; các lần sau là gia hạn.

use chrono::{DateTime, Duration, Utc};
use ledger::{kinds, AccountKey, TxBuilder};
use serde::Serialize;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::{idem_or_random, lock_user, notifications, post_or};
use crate::audit::{self, AuditEntry};
use crate::auth::{grant_role, Role};
use crate::error::{is_unique_violation, AppError, AppResult};
use crate::settings;
use crate::util::format_vnd;

const SHOP_COLS: &str = "id, owner_id, name, slug, bio, avatar_url, stall_until, created_at";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Shop {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub slug: String,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub stall_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Spendable,
    Earnings,
}

impl Source {
    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "spendable" => Some(Source::Spendable),
            "earnings" => Some(Source::Earnings),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Spendable => "spendable",
            Source::Earnings => "earnings",
        }
    }

    fn account(self, user_id: Uuid) -> AccountKey {
        match self {
            Source::Spendable => AccountKey::wallet_spendable(user_id),
            Source::Earnings => AccountKey::earnings_available(user_id),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Source::Spendable => "Ví nạp",
            Source::Earnings => "Doanh thu",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RentResult {
    pub shop: Shop,
    pub rental_id: Uuid,
    pub created_shop: bool,
    /// `true` nếu cùng Idempotency-Key đã xử lý trước đó (không trừ tiền lần nữa).
    pub replayed: bool,
}

/// `Cửa Hàng Đẹp` -> `cua-hang-dep`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for c in name
        .nfd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
    {
        let c = if c == 'đ' || c == 'Đ' { 'd' } else { c };
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "shop".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Tên gian hàng: bỏ khoảng trắng hai đầu, dài 3 đến 40 ký tự, không có ký tự điều khiển.
fn validate_shop_name(name: Option<&str>) -> AppResult<String> {
    let name = name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            AppError::bad_request(
                "shop_name_required",
                "Lần thuê đầu tiên cần đặt tên gian hàng (shop_name).",
            )
        })?;
    let len = name.chars().count();
    if !(3..=40).contains(&len) || name.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "invalid_shop_name",
            "Tên gian hàng phải dài từ 3 đến 40 ký tự.",
        ));
    }
    Ok(name.to_string())
}

async fn unique_slug(conn: &mut PgConnection, name: &str) -> AppResult<String> {
    let base = slugify(name);
    let mut candidate = base.clone();
    for n in 2..200 {
        let taken =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM shops WHERE slug = $1)")
                .bind(&candidate)
                .fetch_one(&mut *conn)
                .await?;
        if !taken {
            return Ok(candidate);
        }
        candidate = format!("{base}-{n}");
    }
    Err(AppError::conflict(
        "slug_unavailable",
        "Không tạo được đường dẫn cho gian hàng. Hãy đặt tên khác.",
    ))
}

async fn load_shop(conn: &mut PgConnection, shop_id: Uuid) -> AppResult<Shop> {
    let sql = format!("SELECT {SHOP_COLS} FROM shops WHERE id = $1");
    let shop = sqlx::query_as::<_, Shop>(&sql)
        .bind(shop_id)
        .fetch_one(conn)
        .await?;
    Ok(shop)
}

pub async fn shop_of_owner(db: &PgPool, owner_id: Uuid) -> AppResult<Option<Shop>> {
    let sql = format!("SELECT {SHOP_COLS} FROM shops WHERE owner_id = $1");
    let shop = sqlx::query_as::<_, Shop>(&sql)
        .bind(owner_id)
        .fetch_optional(db)
        .await?;
    Ok(shop)
}

async fn find_rental_by_key(conn: &mut PgConnection, key: &str) -> AppResult<Option<(Uuid, Uuid)>> {
    let row = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT r.id, r.shop_id FROM stall_rentals r \
         JOIN ledger_transactions t ON t.id = r.ledger_tx_id WHERE t.idempotency_key = $1",
    )
    .bind(key)
    .fetch_optional(conn)
    .await?;
    Ok(row)
}

/// Thuê hoặc gia hạn gian hàng. Giá và số ngày lấy từ `settings` tại thời điểm thuê.
/// Thanh toán từ đúng nguồn đã chọn, không tự chuyển sang nguồn khác khi thiếu tiền.
/// Gia hạn khi còn hạn thì cộng dồn từ `stall_until`; hết hạn thì tính từ bây giờ.
pub async fn rent(
    db: &PgPool,
    user_id: Uuid,
    source: Source,
    shop_name: Option<&str>,
    idem: Option<&str>,
) -> AppResult<RentResult> {
    let key = format!("stall:rent:{user_id}:{}", idem_or_random(idem));
    let mut tx = db.begin().await?;
    lock_user(&mut tx, user_id).await?;

    if let Some((rental_id, shop_id)) = find_rental_by_key(&mut tx, &key).await? {
        let shop = load_shop(&mut tx, shop_id).await?;
        return Ok(RentResult {
            shop,
            rental_id,
            created_shop: false,
            replayed: true,
        });
    }

    let cfg = settings::load(&mut tx).await?;
    let sql = format!("SELECT {SHOP_COLS} FROM shops WHERE owner_id = $1 FOR UPDATE");
    let existing = sqlx::query_as::<_, Shop>(&sql)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let (shop_id, created_shop) = match existing {
        Some(shop) => (shop.id, false),
        None => {
            let name = validate_shop_name(shop_name)?;
            let slug = unique_slug(&mut tx, &name).await?;
            let inserted = sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO shops (owner_id, name, name_key, slug) VALUES ($1, $2, $3, $4) \
                 RETURNING id",
            )
            .bind(user_id)
            .bind(&name)
            .bind(name.to_lowercase())
            .bind(slug)
            .fetch_one(&mut *tx)
            .await;
            match inserted {
                Ok(id) => (id, true),
                Err(e) if is_unique_violation(&e) => {
                    return Err(AppError::conflict(
                        "shop_name_taken",
                        "Tên gian hàng đã có người dùng. Hãy chọn tên khác.",
                    ));
                }
                Err(e) => return Err(e.into()),
            }
        }
    };

    let price = cfg.stall_price;
    let ledger_tx = TxBuilder::new(kinds::STALL_RENT, key.clone())
        .reference("shop", shop_id)
        .debit(source.account(user_id), price)
        .credit(AccountKey::platform_revenue(), price)
        .build()?;
    let outcome = post_or(&mut tx, &ledger_tx, |balance| {
        AppError::unprocessable(
            "insufficient_funds_source",
            format!(
                "{} không đủ: cần {}, hiện có {}. Hãy chọn nguồn khác hoặc nạp thêm.",
                source.label(),
                format_vnd(price),
                format_vnd(balance)
            ),
        )
    })
    .await?;
    if !outcome.is_new() {
        // Hiếm: một yêu cầu cùng khóa đã ghi sổ xong giữa chừng. Trả kết quả đã có.
        let (rental_id, existing_shop) = find_rental_by_key(&mut tx, &key)
            .await?
            .ok_or_else(|| AppError::internal("giao dịch thuê đã có nhưng không thấy lượt thuê"))?;
        let shop = load_shop(&mut tx, existing_shop).await?;
        return Ok(RentResult {
            shop,
            rental_id,
            created_shop: false,
            replayed: true,
        });
    }

    let ends_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        "UPDATE shops SET stall_until = GREATEST(now(), COALESCE(stall_until, now())) \
            + (($2::bigint * 86400)::double precision * interval '1 second') \
         WHERE id = $1 RETURNING stall_until",
    )
    .bind(shop_id)
    .bind(cfg.stall_days)
    .fetch_one(&mut *tx)
    .await?;
    let starts_at = ends_at - Duration::seconds(cfg.stall_days * 86_400);
    let days = i32::try_from(cfg.stall_days)
        .map_err(|_| AppError::internal("stall_days vượt giới hạn"))?;
    let rental_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO stall_rentals (shop_id, ledger_tx_id, source, price, days, starts_at, ends_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(shop_id)
    .bind(outcome.tx_id())
    .bind(source.as_str())
    .bind(price)
    .bind(days)
    .bind(starts_at)
    .bind(ends_at)
    .fetch_one(&mut *tx)
    .await?;
    grant_role(&mut tx, user_id, Role::Seller).await?;
    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(user_id),
            action: "stall.rent",
            target_type: "shop",
            target_id: Some(shop_id.to_string()),
            before: None,
            after: Some(json!({
                "source": source.as_str(), "price": price, "days": cfg.stall_days,
                "stall_until": ends_at, "created_shop": created_shop
            })),
            ip: None,
        },
    )
    .await?;
    notifications::create(
        &mut tx,
        user_id,
        "stall_rented",
        &format!(
            "Đã thuê gian hàng {} ngày, hạn đến {}.",
            cfg.stall_days,
            notifications::vn_date(ends_at)
        ),
        Some(format!("rent:{rental_id}").as_str()),
    )
    .await?;
    let shop = load_shop(&mut tx, shop_id).await?;
    tx.commit().await?;
    Ok(RentResult {
        shop,
        rental_id,
        created_shop,
        replayed: false,
    })
}

/// Sửa mô tả và logo của gian hàng (`PATCH /shops/me`). Chuỗi rỗng nghĩa là xóa.
pub async fn update_profile(
    db: &PgPool,
    owner_id: Uuid,
    bio: Option<&str>,
    avatar_url: Option<&str>,
) -> AppResult<Shop> {
    let shop = shop_of_owner(db, owner_id)
        .await?
        .ok_or_else(|| AppError::not_found("Bạn chưa có gian hàng. Hãy thuê gian hàng trước."))?;
    let mut new_bio = shop.bio.clone();
    let mut new_avatar = shop.avatar_url.clone();
    if let Some(b) = bio {
        let b = b.trim();
        if b.chars().count() > 1000 {
            return Err(AppError::bad_request(
                "validation_error",
                "Mô tả gian hàng dài tối đa 1000 ký tự.",
            ));
        }
        new_bio = if b.is_empty() {
            None
        } else {
            Some(b.to_string())
        };
    }
    if let Some(a) = avatar_url {
        let a = a.trim();
        if a.is_empty() {
            new_avatar = None;
        } else if !a.starts_with("https://") || a.len() > 500 {
            return Err(AppError::bad_request(
                "validation_error",
                "Logo phải là đường dẫn https dài tối đa 500 ký tự.",
            ));
        } else {
            new_avatar = Some(a.to_string());
        }
    }
    let sql =
        format!("UPDATE shops SET bio = $2, avatar_url = $3 WHERE id = $1 RETURNING {SHOP_COLS}");
    let updated = sqlx::query_as::<_, Shop>(&sql)
        .bind(shop.id)
        .bind(new_bio)
        .bind(new_avatar)
        .fetch_one(db)
        .await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tao_slug() {
        assert_eq!(slugify("Cửa Hàng Đẹp"), "cua-hang-dep");
        assert_eq!(slugify("  Shop--A  "), "shop-a");
        assert_eq!(slugify("Đồ Chơi Số!"), "do-choi-so");
        assert_eq!(slugify("***"), "shop");
        assert_eq!(slugify("Plugin Pro 2025"), "plugin-pro-2025");
    }

    #[test]
    fn kiem_tra_ten_shop() {
        assert!(validate_shop_name(None).is_err());
        assert!(validate_shop_name(Some("   ")).is_err());
        assert!(validate_shop_name(Some("ab")).is_err());
        assert!(validate_shop_name(Some(&"a".repeat(41))).is_err());
        assert_eq!(validate_shop_name(Some("  Abc ")).unwrap(), "Abc");
        assert!(validate_shop_name(Some(&"a".repeat(40))).is_ok());
        // Đếm theo ký tự, không theo byte: 3 ký tự tiếng Việt vẫn hợp lệ.
        assert!(validate_shop_name(Some("Đẹp")).is_ok());
    }
}
