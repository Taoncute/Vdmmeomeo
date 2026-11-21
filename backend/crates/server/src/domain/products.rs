//! Sản phẩm, phiên bản, tệp tải lên (upload vào bucket private + quét tự động),
//! danh mục, trang chi tiết công khai và gỡ tạm thời sản phẩm (admin takedown).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::stall::slugify;
use super::{escrow, notifications, require_admin, AdminCtx};
use crate::audit::{self, AuditEntry};
use crate::error::{is_unique_violation, AppError, AppResult};
use crate::scanner::FileScanner;
use crate::settings;
use crate::storage::FileStorage;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Category {
    pub id: i16,
    pub slug: String,
    pub name: String,
    pub icon: String,
    pub risk_level: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Product {
    pub id: Uuid,
    pub shop_id: Uuid,
    pub category_id: i16,
    pub title: String,
    pub slug: String,
    pub price: i64,
    pub description_md: String,
    pub requirements: String,
    pub status: String,
    pub published_version_id: Option<Uuid>,
    pub sales_count: i32,
    pub rating_avg: Option<f64>,
    pub rating_count: i32,
    pub takedown_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ProductVersion {
    pub id: Uuid,
    pub product_id: Uuid,
    pub version: String,
    pub changelog: String,
    pub status: String,
    pub reject_reason: Option<String>,
    pub commitment_accepted: bool,
    pub submitted_at: Option<DateTime<Utc>>,
    pub claimed_by: Option<Uuid>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<Uuid>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Thông tin tệp trả ra API: KHÔNG bao giờ chứa `object_key` hay đường dẫn bucket gốc.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ProductFilePublic {
    pub id: Uuid,
    pub version_id: Uuid,
    pub filename: String,
    pub size: i64,
    pub mime: String,
    pub sha256: String,
    pub scan_status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProductMedia {
    pub id: Uuid,
    pub product_id: Uuid,
    pub kind: String,
    pub url: String,
    pub position: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediaInput {
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub position: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProductInput {
    pub category_id: i16,
    pub title: String,
    pub price: i64,
    #[serde(default)]
    pub description_md: String,
    #[serde(default)]
    pub requirements: String,
    #[serde(default)]
    pub media: Vec<MediaInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProductInput {
    pub category_id: Option<i16>,
    pub title: Option<String>,
    pub price: Option<i64>,
    pub description_md: Option<String>,
    pub requirements: Option<String>,
    pub media: Option<Vec<MediaInput>>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PublicProductSummary {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
    pub price: i64,
    pub category_id: i16,
    pub category_slug: String,
    pub category_name: String,
    pub shop_id: Uuid,
    pub shop_name: String,
    pub shop_slug: String,
    pub sales_count: i32,
    pub rating_avg: Option<f64>,
    pub rating_count: i32,
    pub thumbnail_url: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ShopCard {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub slug: String,
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
    pub rating_avg: Option<f64>,
    pub product_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProductDetail {
    pub product: Product,
    pub category: Category,
    pub shop: ShopCard,
    pub published_version: ProductVersion,
    pub files: Vec<ProductFilePublic>,
    pub media: Vec<ProductMedia>,
    pub related: Vec<PublicProductSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicShopDetail {
    pub shop: ShopCard,
    pub products: Vec<PublicProductSummary>,
}

#[derive(Debug, Clone)]
pub struct UploadInput {
    pub version_id: Uuid,
    pub owner_id: Uuid,
    pub filename: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UploadResult {
    pub file: ProductFilePublic,
    pub scan_report_id: Uuid,
    pub suspicious_findings: Vec<String>,
}

/// Làm sạch cơ bản markdown khỏi thẻ HTML nguy hiểm (`<script>`, `<iframe>`, `javascript:`).
pub fn sanitize_markdown(input: &str) -> String {
    let mut out = input.trim().to_string();
    for bad in [
        "<script",
        "</script>",
        "<iframe",
        "</iframe>",
        "<object",
        "<embed",
        "javascript:",
    ] {
        let lower = out.to_ascii_lowercase();
        if lower.contains(bad) {
            out = strip_case_insensitive(&out, bad);
        }
    }
    out
}

fn strip_case_insensitive(text: &str, needle: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    let needle_len = needle.len();
    while !rest.is_empty() {
        let lower = rest.to_ascii_lowercase();
        if let Some(pos) = lower.find(needle) {
            result.push_str(&rest[..pos]);
            rest = &rest[pos + needle_len..];
        } else {
            result.push_str(rest);
            break;
        }
    }
    result
}

fn validate_title(title: &str) -> AppResult<String> {
    let t = title.trim();
    let len = t.chars().count();
    if !(3..=120).contains(&len) || t.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "invalid_title",
            "Tên sản phẩm phải dài từ 3 đến 120 ký tự.",
        ));
    }
    Ok(t.to_string())
}

fn validate_price(price: i64) -> AppResult<i64> {
    if !(1_000..=1_000_000_000).contains(&price) {
        return Err(AppError::bad_request(
            "invalid_price",
            "Giá sản phẩm phải từ 1.000đ đến 1.000.000.000đ.",
        ));
    }
    Ok(price)
}

fn validate_media(media: &[MediaInput]) -> AppResult<()> {
    if media.len() > 10 {
        return Err(AppError::bad_request(
            "too_many_media",
            "Tối đa 10 ảnh/video demo cho mỗi sản phẩm.",
        ));
    }
    for m in media {
        if m.kind != "image" && m.kind != "video" {
            return Err(AppError::bad_request(
                "invalid_media_kind",
                "kind của media phải là 'image' hoặc 'video'.",
            ));
        }
        let u = m.url.trim();
        if !u.starts_with("https://") || u.len() > 500 {
            return Err(AppError::bad_request(
                "invalid_media_url",
                "URL ảnh/video phải bắt đầu bằng https:// và dài tối đa 500 ký tự.",
            ));
        }
    }
    Ok(())
}

/// Kiểm tra đuôi tệp và MIME cơ bản theo danh sách cho phép trong `settings`.
fn validate_file_ext_and_mime(
    filename: &str,
    mime: &str,
    allowlist: &[String],
) -> AppResult<(String, String)> {
    let base = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .trim();
    if base.is_empty() || base.len() > 180 || base.chars().any(char::is_control) {
        return Err(AppError::bad_request(
            "invalid_filename",
            "Tên tệp không hợp lệ.",
        ));
    }
    let ext = base
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .ok_or_else(|| {
            AppError::bad_request("missing_extension", "Tệp phải có phần mở rộng (đuôi tệp).")
        })?;
    if !allowlist.iter().any(|a| a == &ext) {
        return Err(AppError::bad_request(
            "disallowed_file_type",
            format!(
                "Đuôi tệp '.{ext}' không được phép. Các đuôi được phép: {}.",
                allowlist.join(", ")
            ),
        ));
    }
    let mime_clean = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let mime_ok = if mime_clean.is_empty() || mime_clean == "application/octet-stream" {
        true
    } else {
        match ext.as_str() {
            "zip" => matches!(
                mime_clean.as_str(),
                "application/zip" | "application/x-zip-compressed"
            ),
            "rar" => matches!(
                mime_clean.as_str(),
                "application/vnd.rar" | "application/x-rar-compressed"
            ),
            "7z" => mime_clean == "application/x-7z-compressed",
            "py" => matches!(
                mime_clean.as_str(),
                "text/x-python" | "text/plain" | "application/x-python-code"
            ),
            "js" => matches!(
                mime_clean.as_str(),
                "application/javascript" | "text/javascript" | "text/plain"
            ),
            "json" => matches!(mime_clean.as_str(), "application/json" | "text/plain"),
            "pdf" => mime_clean == "application/pdf",
            "md" => matches!(mime_clean.as_str(), "text/markdown" | "text/plain"),
            "txt" => mime_clean == "text/plain",
            _ => true,
        }
    };
    if !mime_ok {
        return Err(AppError::bad_request(
            "mime_mismatch",
            format!("Kiểu MIME '{mime_clean}' không khớp với đuôi tệp '.{ext}'."),
        ));
    }
    let effective_mime = if mime_clean.is_empty() {
        "application/octet-stream".to_string()
    } else {
        mime_clean
    };
    Ok((base.to_string(), effective_mime))
}

async fn unique_product_slug(conn: &mut PgConnection, title: &str) -> AppResult<String> {
    let base = slugify(title);
    let mut candidate = base.clone();
    for n in 2..200 {
        let taken =
            sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM products WHERE slug = $1)")
                .bind(&candidate)
                .fetch_one(&mut *conn)
                .await?;
        if !taken {
            return Ok(candidate);
        }
        candidate = format!("{base}-{n}");
    }
    Ok(format!("{base}-{}", &Uuid::new_v4().to_string()[..8]))
}

async fn require_active_shop(conn: &mut PgConnection, owner_id: Uuid) -> AppResult<Uuid> {
    let row = sqlx::query_as::<_, (Uuid, Option<DateTime<Utc>>)>(
        "SELECT id, stall_until FROM shops WHERE owner_id = $1",
    )
    .bind(owner_id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| {
        AppError::forbidden("Bạn chưa có gian hàng. Hãy thuê gian hàng trước khi đăng sản phẩm.")
    })?;
    let (shop_id, stall_until) = row;
    let active = stall_until.is_some_and(|until| until > Utc::now());
    if !active {
        return Err(AppError::forbidden(
            "Gian hàng đã hết hạn thuê. Hãy gia hạn để tiếp tục quản lý sản phẩm.",
        ));
    }
    Ok(shop_id)
}

pub async fn list_categories(db: &PgPool) -> AppResult<Vec<Category>> {
    let rows = sqlx::query_as::<_, Category>(
        "SELECT id, slug, name, icon, risk_level FROM categories ORDER BY id ASC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn create_product(
    db: &PgPool,
    owner_id: Uuid,
    input: CreateProductInput,
) -> AppResult<Product> {
    let title = validate_title(&input.title)?;
    let price = validate_price(input.price)?;
    validate_media(&input.media)?;
    let description_md = sanitize_markdown(&input.description_md);
    let requirements = input.requirements.trim().to_string();
    if description_md.chars().count() > 20_000 || requirements.chars().count() > 4_000 {
        return Err(AppError::bad_request(
            "validation_error",
            "Mô tả tối đa 20.000 ký tự, yêu cầu hệ thống tối đa 4.000 ký tự.",
        ));
    }

    let mut tx = db.begin().await?;
    let shop_id = require_active_shop(&mut tx, owner_id).await?;
    let cat_exists =
        sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM categories WHERE id = $1)")
            .bind(input.category_id)
            .fetch_one(&mut *tx)
            .await?;
    if !cat_exists {
        return Err(AppError::bad_request(
            "invalid_category",
            "Danh mục không tồn tại.",
        ));
    }
    let slug = unique_product_slug(&mut tx, &title).await?;
    let product = sqlx::query_as::<_, Product>(
        "INSERT INTO products (shop_id, category_id, title, slug, price, description_md, requirements) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, shop_id, category_id, title, slug, price, description_md, requirements, \
                   status, published_version_id, sales_count, rating_avg, rating_count, \
                   takedown_reason, created_at, updated_at",
    )
    .bind(shop_id)
    .bind(input.category_id)
    .bind(&title)
    .bind(&slug)
    .bind(price)
    .bind(&description_md)
    .bind(&requirements)
    .fetch_one(&mut *tx)
    .await?;

    for m in &input.media {
        sqlx::query(
            "INSERT INTO product_media (product_id, kind, url, position) VALUES ($1, $2, $3, $4)",
        )
        .bind(product.id)
        .bind(&m.kind)
        .bind(m.url.trim())
        .bind(m.position)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(product)
}

pub async fn update_product(
    db: &PgPool,
    owner_id: Uuid,
    product_id: Uuid,
    input: UpdateProductInput,
) -> AppResult<Product> {
    let mut tx = db.begin().await?;
    let shop_id = require_active_shop(&mut tx, owner_id).await?;
    let existing = sqlx::query_as::<_, Product>(
        "SELECT id, shop_id, category_id, title, slug, price, description_md, requirements, \
                status, published_version_id, sales_count, rating_avg, rating_count, \
                takedown_reason, created_at, updated_at \
         FROM products WHERE id = $1 AND shop_id = $2 FOR UPDATE",
    )
    .bind(product_id)
    .bind(shop_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy sản phẩm trong gian hàng của bạn."))?;

    let title = match input.title {
        Some(t) => validate_title(&t)?,
        None => existing.title,
    };
    let price = match input.price {
        Some(p) => validate_price(p)?,
        None => existing.price,
    };
    let category_id = match input.category_id {
        Some(c) => {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM categories WHERE id = $1)",
            )
            .bind(c)
            .fetch_one(&mut *tx)
            .await?;
            if !exists {
                return Err(AppError::bad_request(
                    "invalid_category",
                    "Danh mục không tồn tại.",
                ));
            }
            c
        }
        None => existing.category_id,
    };
    let description_md = match input.description_md {
        Some(d) => {
            let clean = sanitize_markdown(&d);
            if clean.chars().count() > 20_000 {
                return Err(AppError::bad_request(
                    "validation_error",
                    "Mô tả tối đa 20.000 ký tự.",
                ));
            }
            clean
        }
        None => existing.description_md,
    };
    let requirements = match input.requirements {
        Some(r) => {
            let clean = r.trim().to_string();
            if clean.chars().count() > 4_000 {
                return Err(AppError::bad_request(
                    "validation_error",
                    "Yêu cầu hệ thống tối đa 4.000 ký tự.",
                ));
            }
            clean
        }
        None => existing.requirements,
    };

    if let Some(media) = &input.media {
        validate_media(media)?;
        sqlx::query("DELETE FROM product_media WHERE product_id = $1")
            .bind(product_id)
            .execute(&mut *tx)
            .await?;
        for m in media {
            sqlx::query(
                "INSERT INTO product_media (product_id, kind, url, position) VALUES ($1, $2, $3, $4)",
            )
            .bind(product_id)
            .bind(&m.kind)
            .bind(m.url.trim())
            .bind(m.position)
            .execute(&mut *tx)
            .await?;
        }
    }

    let updated = sqlx::query_as::<_, Product>(
        "UPDATE products SET category_id = $2, title = $3, price = $4, \
         description_md = $5, requirements = $6, updated_at = now() \
         WHERE id = $1 \
         RETURNING id, shop_id, category_id, title, slug, price, description_md, requirements, \
                   status, published_version_id, sales_count, rating_avg, rating_count, \
                   takedown_reason, created_at, updated_at",
    )
    .bind(product_id)
    .bind(category_id)
    .bind(&title)
    .bind(price)
    .bind(&description_md)
    .bind(&requirements)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(updated)
}

pub async fn list_seller_products(db: &PgPool, owner_id: Uuid) -> AppResult<Vec<Product>> {
    let rows = sqlx::query_as::<_, Product>(
        "SELECT p.id, p.shop_id, p.category_id, p.title, p.slug, p.price, p.description_md, \
                p.requirements, p.status, p.published_version_id, p.sales_count, p.rating_avg, \
                p.rating_count, p.takedown_reason, p.created_at, p.updated_at \
         FROM products p JOIN shops s ON s.id = p.shop_id \
         WHERE s.owner_id = $1 ORDER BY p.created_at DESC",
    )
    .bind(owner_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

pub async fn create_version(
    db: &PgPool,
    owner_id: Uuid,
    product_id: Uuid,
    version: &str,
    changelog: &str,
) -> AppResult<ProductVersion> {
    let ver = version.trim();
    if ver.is_empty()
        || ver.len() > 32
        || !ver
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(AppError::bad_request(
            "invalid_version",
            "Số phiên bản chỉ gồm chữ, số, '.', '-', '_' và dài tối đa 32 ký tự.",
        ));
    }
    let log = changelog.trim();
    if log.chars().count() > 5_000 {
        return Err(AppError::bad_request(
            "validation_error",
            "Changelog dài tối đa 5.000 ký tự.",
        ));
    }

    let mut tx = db.begin().await?;
    let shop_id = require_active_shop(&mut tx, owner_id).await?;
    let owned = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM products WHERE id = $1 AND shop_id = $2)",
    )
    .bind(product_id)
    .bind(shop_id)
    .fetch_one(&mut *tx)
    .await?;
    if !owned {
        return Err(AppError::not_found(
            "Không tìm thấy sản phẩm trong gian hàng của bạn.",
        ));
    }

    let created = sqlx::query_as::<_, ProductVersion>(
        "INSERT INTO product_versions (product_id, version, changelog) \
         VALUES ($1, $2, $3) \
         RETURNING id, product_id, version, changelog, status, reject_reason, commitment_accepted, \
                   submitted_at, claimed_by, claimed_at, reviewed_by, reviewed_at, created_at",
    )
    .bind(product_id)
    .bind(ver)
    .bind(log)
    .fetch_one(&mut *tx)
    .await;

    let row = match created {
        Ok(r) => r,
        Err(e) if is_unique_violation(&e) => {
            return Err(AppError::conflict(
                "version_exists",
                format!("Phiên bản '{ver}' đã tồn tại cho sản phẩm này."),
            ));
        }
        Err(e) => return Err(e.into()),
    };
    tx.commit().await?;
    Ok(row)
}

/// Upload tệp cho phiên bản ở trạng thái `draft` hoặc `rejected`:
/// kiểm tra đuôi/MIME, giới hạn dung lượng theo `settings`, tính SHA-256, lưu lên bucket private,
/// chạy quét tự động (`Scanner`) và lưu kết quả vào `review_reports`.
pub async fn upload_version_file(
    db: &PgPool,
    storage: &dyn FileStorage,
    scanner: &dyn FileScanner,
    input: UploadInput,
) -> AppResult<UploadResult> {
    if input.data.is_empty() {
        return Err(AppError::bad_request(
            "empty_file",
            "Tệp tải lên không được rỗng.",
        ));
    }

    let mut conn = db.acquire().await?;
    let cfg = settings::load(&mut conn).await?;
    let max_bytes = (cfg.max_upload_mb as usize).saturating_mul(1024 * 1024);
    if input.data.len() > max_bytes {
        return Err(AppError::bad_request(
            "file_too_large",
            format!("Tệp vượt quá giới hạn {} MB.", cfg.max_upload_mb),
        ));
    }
    let (filename, mime) =
        validate_file_ext_and_mime(&input.filename, &input.mime, &cfg.file_ext_allowlist)?;

    // Kiểm tra quyền sở hữu và trạng thái phiên bản trước khi ghi lên storage.
    let ver_info = sqlx::query_as::<_, (Uuid, String, Option<DateTime<Utc>>)>(
        "SELECT v.product_id, v.status, s.stall_until \
         FROM product_versions v \
         JOIN products p ON p.id = v.product_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE v.id = $1 AND s.owner_id = $2",
    )
    .bind(input.version_id)
    .bind(input.owner_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy phiên bản thuộc gian hàng của bạn."))?;
    let (product_id, status, stall_until) = ver_info;
    if !stall_until.is_some_and(|u| u > Utc::now()) {
        return Err(AppError::forbidden(
            "Gian hàng đã hết hạn thuê. Hãy gia hạn trước khi tải tệp.",
        ));
    }
    if status != "draft" && status != "rejected" {
        return Err(AppError::conflict(
            "version_locked",
            "Chỉ được tải tệp khi phiên bản đang ở trạng thái nháp hoặc bị từ chối.",
        ));
    }
    drop(conn);

    let sha256 = hex::encode(Sha256::digest(&input.data));
    let scan = scanner.scan(&filename, &input.data).await?;
    let object_key = format!(
        "products/{product_id}/versions/{}/{}_{filename}",
        input.version_id,
        Uuid::new_v4()
    );
    storage.put(&object_key, &mime, &input.data).await?;

    let mut tx = db.begin().await?;
    let file = sqlx::query_as::<_, ProductFilePublic>(
        "INSERT INTO product_files (version_id, object_key, filename, size, mime, sha256, scan_status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, version_id, filename, size, mime, sha256, scan_status, created_at",
    )
    .bind(input.version_id)
    .bind(&object_key)
    .bind(&filename)
    .bind(input.data.len() as i64)
    .bind(&mime)
    .bind(&sha256)
    .bind(&scan.status)
    .fetch_one(&mut *tx)
    .await?;

    let report_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO review_reports (version_id, kind, result) VALUES ($1, 'auto', $2) RETURNING id",
    )
    .bind(input.version_id)
    .bind(json!({
        "file_id": file.id,
        "filename": file.filename,
        "sha256": file.sha256,
        "scan_status": scan.status,
        "clamav_checked": scan.clamav_checked,
        "clamav_result": scan.clamav_result,
        "suspicious_findings": scan.suspicious_findings,
    }))
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(UploadResult {
        file,
        scan_report_id: report_id,
        suspicious_findings: scan.suspicious_findings,
    })
}

/// Nộp phiên bản vào hàng chờ duyệt (`draft`/`rejected` -> `pending_review`).
pub async fn submit_version(
    db: &PgPool,
    owner_id: Uuid,
    version_id: Uuid,
    commitment_accepted: bool,
) -> AppResult<ProductVersion> {
    if !commitment_accepted {
        return Err(AppError::bad_request(
            "commitment_required",
            "Bạn phải cam kết sản phẩm không chứa mã độc hoặc hành vi đánh cắp dữ liệu.",
        ));
    }
    let mut tx = db.begin().await?;
    let shop_id = require_active_shop(&mut tx, owner_id).await?;
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT v.status, p.status \
         FROM product_versions v JOIN products p ON p.id = v.product_id \
         WHERE v.id = $1 AND p.shop_id = $2 FOR UPDATE OF v",
    )
    .bind(version_id)
    .bind(shop_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy phiên bản trong gian hàng của bạn."))?;
    let (ver_status, prod_status) = row;
    if prod_status == "taken_down" {
        return Err(AppError::forbidden(
            "Sản phẩm đã bị gỡ bởi quản trị viên, không thể nộp duyệt.",
        ));
    }
    if ver_status != "draft" && ver_status != "rejected" {
        return Err(AppError::conflict(
            "invalid_transition",
            "Chỉ phiên bản nháp hoặc bị từ chối mới nộp duyệt được.",
        ));
    }

    let (file_count, infected_count) = sqlx::query_as::<_, (i64, i64)>(
        "SELECT count(*), count(*) FILTER (WHERE scan_status = 'infected') \
         FROM product_files WHERE version_id = $1",
    )
    .bind(version_id)
    .fetch_one(&mut *tx)
    .await?;
    if file_count == 0 {
        return Err(AppError::bad_request(
            "no_files",
            "Cần tải lên ít nhất một tệp trước khi nộp duyệt.",
        ));
    }
    if infected_count > 0 {
        return Err(AppError::unprocessable(
            "infected_file",
            "Phiên bản có tệp bị phát hiện nhiễm mã độc, không thể nộp duyệt.",
        ));
    }

    let updated = sqlx::query_as::<_, ProductVersion>(
        "UPDATE product_versions \
         SET status = 'pending_review', commitment_accepted = true, reject_reason = NULL, \
             submitted_at = now(), claimed_by = NULL, claimed_at = NULL \
         WHERE id = $1 \
         RETURNING id, product_id, version, changelog, status, reject_reason, commitment_accepted, \
                   submitted_at, claimed_by, claimed_at, reviewed_by, reviewed_at, created_at",
    )
    .bind(version_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(updated)
}

#[derive(Debug, Clone, Default)]
pub struct CatalogQuery {
    pub cat: Option<String>,
    pub q: Option<String>,
    pub sort: Option<String>,
    pub cursor: Option<DateTime<Utc>>,
    pub limit: i64,
}

pub async fn list_public_products(
    db: &PgPool,
    query: CatalogQuery,
) -> AppResult<Vec<PublicProductSummary>> {
    let limit = query.limit.clamp(1, 50);
    let q_like = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));
    let order_by = match query.sort.as_deref() {
        Some("price_asc") => "p.price ASC, p.created_at DESC",
        Some("price_desc") => "p.price DESC, p.created_at DESC",
        Some("popular") => "p.sales_count DESC, p.created_at DESC",
        Some("rating") => "COALESCE(p.rating_avg, 0) DESC, p.created_at DESC",
        _ => "p.created_at DESC",
    };
    let sql = format!(
        "SELECT p.id, p.slug, p.title, p.price, p.category_id, \
                c.slug AS category_slug, c.name AS category_name, \
                s.id AS shop_id, s.name AS shop_name, s.slug AS shop_slug, \
                p.sales_count, p.rating_avg, p.rating_count, \
                (SELECT m.url FROM product_media m WHERE m.product_id = p.id \
                 ORDER BY m.position ASC, m.created_at ASC LIMIT 1) AS thumbnail_url, \
                p.created_at \
         FROM products p \
         JOIN categories c ON c.id = p.category_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE p.status = 'active' \
           AND p.published_version_id IS NOT NULL \
           AND s.stall_until > now() \
           AND ($1::text IS NULL OR c.slug = $1) \
           AND ($2::text IS NULL OR p.title ILIKE $2 OR p.description_md ILIKE $2) \
           AND ($3::timestamptz IS NULL OR p.created_at < $3) \
         ORDER BY {order_by} \
         LIMIT $4"
    );
    let rows = sqlx::query_as::<_, PublicProductSummary>(&sql)
        .bind(query.cat.as_deref())
        .bind(q_like.as_deref())
        .bind(query.cursor)
        .bind(limit)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

async fn load_shop_card(conn: &mut PgConnection, shop_id: Uuid) -> AppResult<ShopCard> {
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<f64>,
        ),
    >(
        "SELECT id, owner_id, name, slug, bio, avatar_url, rating_avg FROM shops WHERE id = $1",
    )
    .bind(shop_id)
    .fetch_one(&mut *conn)
    .await?;
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM products WHERE shop_id = $1 AND status = 'active' AND published_version_id IS NOT NULL",
    )
    .bind(shop_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(ShopCard {
        id: row.0,
        owner_id: row.1,
        name: row.2,
        slug: row.3,
        bio: row.4,
        avatar_url: row.5,
        rating_avg: row.6,
        product_count: count,
    })
}

pub async fn get_public_product_detail(db: &PgPool, id_or_slug: &str) -> AppResult<ProductDetail> {
    let mut conn = db.acquire().await?;
    let product = if let Ok(uuid) = Uuid::parse_str(id_or_slug) {
        sqlx::query_as::<_, Product>(
            "SELECT p.id, p.shop_id, p.category_id, p.title, p.slug, p.price, p.description_md, \
                    p.requirements, p.status, p.published_version_id, p.sales_count, p.rating_avg, \
                    p.rating_count, p.takedown_reason, p.created_at, p.updated_at \
             FROM products p JOIN shops s ON s.id = p.shop_id \
             WHERE p.id = $1 AND p.status = 'active' AND p.published_version_id IS NOT NULL \
               AND s.stall_until > now()",
        )
        .bind(uuid)
        .fetch_optional(&mut *conn)
        .await?
    } else {
        sqlx::query_as::<_, Product>(
            "SELECT p.id, p.shop_id, p.category_id, p.title, p.slug, p.price, p.description_md, \
                    p.requirements, p.status, p.published_version_id, p.sales_count, p.rating_avg, \
                    p.rating_count, p.takedown_reason, p.created_at, p.updated_at \
             FROM products p JOIN shops s ON s.id = p.shop_id \
             WHERE p.slug = $1 AND p.status = 'active' AND p.published_version_id IS NOT NULL \
               AND s.stall_until > now()",
        )
        .bind(id_or_slug)
        .fetch_optional(&mut *conn)
        .await?
    }
    .ok_or_else(|| AppError::not_found("Không tìm thấy sản phẩm."))?;

    let pub_ver_id = product
        .published_version_id
        .ok_or_else(|| AppError::not_found("Sản phẩm chưa có phiên bản được duyệt."))?;
    let category = sqlx::query_as::<_, Category>(
        "SELECT id, slug, name, icon, risk_level FROM categories WHERE id = $1",
    )
    .bind(product.category_id)
    .fetch_one(&mut *conn)
    .await?;
    let shop = load_shop_card(&mut conn, product.shop_id).await?;
    let published_version = sqlx::query_as::<_, ProductVersion>(
        "SELECT id, product_id, version, changelog, status, reject_reason, commitment_accepted, \
                submitted_at, claimed_by, claimed_at, reviewed_by, reviewed_at, created_at \
         FROM product_versions WHERE id = $1",
    )
    .bind(pub_ver_id)
    .fetch_one(&mut *conn)
    .await?;
    let files = sqlx::query_as::<_, ProductFilePublic>(
        "SELECT id, version_id, filename, size, mime, sha256, scan_status, created_at \
         FROM product_files WHERE version_id = $1 ORDER BY created_at ASC",
    )
    .bind(pub_ver_id)
    .fetch_all(&mut *conn)
    .await?;
    let media = sqlx::query_as::<_, ProductMedia>(
        "SELECT id, product_id, kind, url, position FROM product_media \
         WHERE product_id = $1 ORDER BY position ASC, created_at ASC",
    )
    .bind(product.id)
    .fetch_all(&mut *conn)
    .await?;
    let related = sqlx::query_as::<_, PublicProductSummary>(
        "SELECT p.id, p.slug, p.title, p.price, p.category_id, \
                c.slug AS category_slug, c.name AS category_name, \
                s.id AS shop_id, s.name AS shop_name, s.slug AS shop_slug, \
                p.sales_count, p.rating_avg, p.rating_count, \
                (SELECT m.url FROM product_media m WHERE m.product_id = p.id \
                 ORDER BY m.position ASC, m.created_at ASC LIMIT 1) AS thumbnail_url, \
                p.created_at \
         FROM products p \
         JOIN categories c ON c.id = p.category_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE p.category_id = $1 AND p.id <> $2 \
           AND p.status = 'active' AND p.published_version_id IS NOT NULL \
           AND s.stall_until > now() \
         ORDER BY p.sales_count DESC, p.created_at DESC LIMIT 6",
    )
    .bind(product.category_id)
    .bind(product.id)
    .fetch_all(&mut *conn)
    .await?;

    Ok(ProductDetail {
        product,
        category,
        shop,
        published_version,
        files,
        media,
        related,
    })
}

pub async fn get_public_shop(db: &PgPool, id_or_slug: &str) -> AppResult<PublicShopDetail> {
    let mut conn = db.acquire().await?;
    let shop_id = if let Ok(uuid) = Uuid::parse_str(id_or_slug) {
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM shops WHERE id = $1 AND stall_until > now()")
            .bind(uuid)
            .fetch_optional(&mut *conn)
            .await?
    } else {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM shops WHERE slug = $1 AND stall_until > now()",
        )
        .bind(id_or_slug)
        .fetch_optional(&mut *conn)
        .await?
    }
    .ok_or_else(|| AppError::not_found("Không tìm thấy gian hàng đang hoạt động."))?;

    let shop = load_shop_card(&mut conn, shop_id).await?;
    let products = sqlx::query_as::<_, PublicProductSummary>(
        "SELECT p.id, p.slug, p.title, p.price, p.category_id, \
                c.slug AS category_slug, c.name AS category_name, \
                s.id AS shop_id, s.name AS shop_name, s.slug AS shop_slug, \
                p.sales_count, p.rating_avg, p.rating_count, \
                (SELECT m.url FROM product_media m WHERE m.product_id = p.id \
                 ORDER BY m.position ASC, m.created_at ASC LIMIT 1) AS thumbnail_url, \
                p.created_at \
         FROM products p \
         JOIN categories c ON c.id = p.category_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE p.shop_id = $1 AND p.status = 'active' AND p.published_version_id IS NOT NULL \
         ORDER BY p.created_at DESC LIMIT 50",
    )
    .bind(shop_id)
    .fetch_all(&mut *conn)
    .await?;

    Ok(PublicShopDetail { shop, products })
}

#[derive(Debug, Clone, Serialize)]
pub struct TakedownResult {
    pub product_id: Uuid,
    pub refunded_orders: usize,
}

/// Admin gỡ tạm thời sản phẩm; tùy chọn hoàn tiền các đơn còn ở escrow / `earnings_pending`.
pub async fn takedown_product(
    db: &PgPool,
    ctx: AdminCtx<'_>,
    product_id: Uuid,
    reason: &str,
    refund_eligible: bool,
) -> AppResult<TakedownResult> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        return Err(AppError::bad_request(
            "invalid_reason",
            "Lý do gỡ sản phẩm phải dài từ 1 đến 500 ký tự.",
        ));
    }

    let mut tx = db.begin().await?;
    require_admin(&mut tx, ctx.admin_id).await?;
    let (old_status, seller_id, title) = sqlx::query_as::<_, (String, Uuid, String)>(
        "SELECT p.status, s.owner_id, p.title \
         FROM products p JOIN shops s ON s.id = p.shop_id \
         WHERE p.id = $1 FOR UPDATE OF p",
    )
    .bind(product_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy sản phẩm."))?;

    sqlx::query(
        "UPDATE products SET status = 'taken_down', takedown_reason = $2, updated_at = now() \
         WHERE id = $1",
    )
    .bind(product_id)
    .bind(reason)
    .execute(&mut *tx)
    .await?;

    let mut refunded_orders = 0usize;
    if refund_eligible {
        let orders = sqlx::query_as::<_, (Uuid, Uuid, i64, String)>(
            "SELECT id, buyer_id, total, status FROM orders \
             WHERE product_id = $1 AND status IN ('paid', 'completed') FOR UPDATE",
        )
        .bind(product_id)
        .fetch_all(&mut *tx)
        .await?;
        for (order_id, buyer_id, total, status) in orders {
            let refunded = if status == "paid" {
                escrow::refund_from_escrow_in_tx(&mut tx, buyer_id, order_id, total)
                    .await
                    .is_ok()
            } else {
                escrow::refund_from_pending_in_tx(&mut tx, seller_id, buyer_id, order_id, total)
                    .await
                    .is_ok()
            };
            if refunded {
                sqlx::query(
                    "UPDATE orders SET status = 'refunded', refunded_at = now() WHERE id = $1",
                )
                .bind(order_id)
                .execute(&mut *tx)
                .await?;
                refunded_orders += 1;
            }
        }
        if refunded_orders > 0 {
            sqlx::query(
                "UPDATE products SET sales_count = GREATEST(0, sales_count - $2) WHERE id = $1",
            )
            .bind(product_id)
            .bind(refunded_orders as i32)
            .execute(&mut *tx)
            .await?;
        }
    }

    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(ctx.admin_id),
            action: "product.takedown",
            target_type: "product",
            target_id: Some(product_id.to_string()),
            before: Some(json!({ "status": old_status })),
            after: Some(json!({
                "status": "taken_down",
                "reason": reason,
                "refunded_orders": refunded_orders,
            })),
            ip: Some(ctx.ip),
        },
    )
    .await?;

    let _ = notifications::create(
        &mut tx,
        seller_id,
        "product_takedown",
        &format!("Sản phẩm '{title}' đã bị gỡ tạm thời. Lý do: {reason}"),
        None,
    )
    .await;

    tx.commit().await?;
    Ok(TakedownResult {
        product_id,
        refunded_orders,
    })
}

/// Kiểm tra nhanh `sanitize_markdown` và `validate_file_ext_and_mime`.
/// Helper trả JSON thô cho các báo cáo tự động của phiên bản (dùng trong review queue).
pub async fn version_auto_reports(
    conn: &mut PgConnection,
    version_id: Uuid,
) -> AppResult<Vec<Value>> {
    let rows = sqlx::query_scalar::<_, Value>(
        "SELECT result FROM review_reports WHERE version_id = $1 AND kind = 'auto' \
         ORDER BY created_at DESC",
    )
    .bind(version_id)
    .fetch_all(conn)
    .await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loc_markdown_nguy_hiem() {
        let s = sanitize_markdown("Xin chào <SCRIPT>alert(1)</script> [link](javascript:alert(1))");
        assert!(!s.to_ascii_lowercase().contains("<script"));
        assert!(!s.to_ascii_lowercase().contains("javascript:"));
    }

    #[test]
    fn kiem_tra_duoi_tep_va_mime() {
        let allow: Vec<String> = ["zip", "py", "txt"].into_iter().map(String::from).collect();
        assert!(validate_file_ext_and_mime("tool.zip", "application/zip", &allow).is_ok());
        assert!(
            validate_file_ext_and_mime("virus.exe", "application/octet-stream", &allow).is_err()
        );
        assert!(validate_file_ext_and_mime("fake.zip", "text/html", &allow).is_err());
    }
}
