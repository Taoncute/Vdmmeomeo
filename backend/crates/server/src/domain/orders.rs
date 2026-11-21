//! Đơn hàng (1 đơn = 1 sản phẩm), thanh toán qua escrow, xác nhận/tự hoàn tất,
//! giao file bằng signed URL có hạn và giới hạn lượt tải, tranh chấp và đánh giá.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{escrow, idem_or_random, lock_user, notifications, require_admin, AdminCtx};
use crate::audit::{self, AuditEntry};
use crate::error::{is_unique_violation, AppError, AppResult};
use crate::settings;
use crate::storage::{FileStorage, SignedDownload};

const DOWNLOAD_TTL_SECS: u64 = 120;

const ORDER_COLS: &str = "id, buyer_id, seller_id, shop_id, product_id, version_id, total, \
    status, pre_dispute_status, offer_id, paid_at, auto_complete_at, completed_at, \
    release_at, refunded_at, created_at";

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Order {
    pub id: Uuid,
    pub buyer_id: Uuid,
    pub seller_id: Uuid,
    pub shop_id: Uuid,
    pub product_id: Uuid,
    pub version_id: Uuid,
    pub total: i64,
    pub status: String,
    pub pre_dispute_status: Option<String>,
    pub offer_id: Option<Uuid>,
    pub paid_at: DateTime<Utc>,
    pub auto_complete_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub release_at: Option<DateTime<Utc>>,
    pub refunded_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateOrderResult {
    pub order: Order,
    pub replayed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadResponse {
    pub file_id: Uuid,
    pub filename: String,
    pub download: SignedDownload,
    pub downloads_used: i64,
    pub download_limit: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Dispute {
    pub id: Uuid,
    pub order_id: Uuid,
    pub opened_by: Uuid,
    pub reason: String,
    pub evidence: Value,
    pub status: String,
    pub resolution: Option<String>,
    pub resolution_note: Option<String>,
    pub resolved_by: Option<Uuid>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Review {
    pub id: Uuid,
    pub order_id: Uuid,
    pub product_id: Uuid,
    pub buyer_id: Uuid,
    pub rating: i16,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PublicReview {
    pub id: Uuid,
    pub product_id: Uuid,
    pub buyer_id: Uuid,
    pub buyer_name: String,
    pub rating: i16,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeResolution {
    Refund,
    Release,
}

impl DisputeResolution {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "refund" => Some(Self::Refund),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Refund => "refund",
            Self::Release => "release",
        }
    }
}

/// Tạo đơn hàng và trừ `wallet_spendable` -> `escrow` trong cùng một DB transaction.
/// Mỗi đơn 1 sản phẩm; mỗi người mua chỉ mua một sản phẩm một lần (trừ khi đơn cũ đã hoàn tiền).
pub async fn create_order(
    db: &PgPool,
    buyer_id: Uuid,
    product_id: Uuid,
    idem: Option<&str>,
) -> AppResult<CreateOrderResult> {
    let idem_key = idem_or_random(idem);
    let mut tx = db.begin().await?;
    lock_user(&mut tx, buyer_id).await?;

    let existing_sql =
        format!("SELECT {ORDER_COLS} FROM orders WHERE buyer_id = $1 AND idempotency_key = $2");
    if let Some(order) = sqlx::query_as::<_, Order>(&existing_sql)
        .bind(buyer_id)
        .bind(&idem_key)
        .fetch_optional(&mut *tx)
        .await?
    {
        return Ok(CreateOrderResult {
            order,
            replayed: true,
        });
    }

    let prod = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>, i64, String, Option<DateTime<Utc>>)>(
        "SELECT p.shop_id, s.owner_id, p.published_version_id, p.price, p.status, s.stall_until \
         FROM products p JOIN shops s ON s.id = p.shop_id \
         WHERE p.id = $1 FOR UPDATE OF p",
    )
    .bind(product_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy sản phẩm."))?;
    let (shop_id, seller_id, pub_ver, price, status, stall_until) = prod;

    if status != "active" || !stall_until.is_some_and(|u| u > Utc::now()) {
        return Err(AppError::unprocessable(
            "product_unavailable",
            "Sản phẩm hiện không mở bán hoặc gian hàng đã hết hạn.",
        ));
    }
    let version_id = pub_ver.ok_or_else(|| {
        AppError::unprocessable(
            "product_unavailable",
            "Sản phẩm chưa có phiên bản được duyệt.",
        )
    })?;
    if seller_id == buyer_id {
        return Err(AppError::bad_request(
            "cannot_buy_own_product",
            "Bạn không thể tự mua sản phẩm của chính mình.",
        ));
    }

    let already_bought = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM orders \
         WHERE buyer_id = $1 AND product_id = $2 AND status <> 'refunded')",
    )
    .bind(buyer_id)
    .bind(product_id)
    .fetch_one(&mut *tx)
    .await?;
    if already_bought {
        return Err(AppError::conflict(
            "already_purchased",
            "Bạn đã mua sản phẩm này rồi. Bạn có thể tải lại hoặc nhận bản cập nhật miễn phí trong Đơn của tôi.",
        ));
    }

    let cfg = settings::load(&mut tx).await?;
    let order_id = Uuid::new_v4();
    let insert_sql = format!(
        "INSERT INTO orders (id, buyer_id, seller_id, shop_id, product_id, version_id, total, \
                             status, idempotency_key, auto_complete_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'paid', $8, \
                 now() + ($9::bigint * 86400 * interval '1 second')) \
         RETURNING {ORDER_COLS}"
    );
    let order = sqlx::query_as::<_, Order>(&insert_sql)
        .bind(order_id)
        .bind(buyer_id)
        .bind(seller_id)
        .bind(shop_id)
        .bind(product_id)
        .bind(version_id)
        .bind(price)
        .bind(&idem_key)
        .bind(cfg.auto_complete_days)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::conflict("already_purchased", "Đơn hàng cho sản phẩm này đã tồn tại.")
            } else {
                AppError::from(e)
            }
        })?;

    sqlx::query(
        "INSERT INTO order_items (order_id, product_id, version_id, price) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(order.id)
    .bind(product_id)
    .bind(version_id)
    .bind(price)
    .execute(&mut *tx)
    .await?;

    // Ghi sổ cái `wallet_spendable -> escrow` trong cùng transaction.
    escrow::hold_in_tx(&mut tx, buyer_id, order.id, price).await?;

    sqlx::query("UPDATE products SET sales_count = sales_count + 1 WHERE id = $1")
        .bind(product_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;
    Ok(CreateOrderResult {
        order,
        replayed: false,
    })
}

pub async fn list_buyer_orders(db: &PgPool, buyer_id: Uuid) -> AppResult<Vec<Order>> {
    let sql = format!(
        "SELECT {ORDER_COLS} FROM orders WHERE buyer_id = $1 ORDER BY created_at DESC LIMIT 200"
    );
    let rows = sqlx::query_as::<_, Order>(&sql)
        .bind(buyer_id)
        .fetch_all(db)
        .await?;
    Ok(rows)
}

pub async fn get_order(
    db: &PgPool,
    user_id: Uuid,
    is_admin: bool,
    order_id: Uuid,
) -> AppResult<Order> {
    let sql = format!("SELECT {ORDER_COLS} FROM orders WHERE id = $1");
    let order = sqlx::query_as::<_, Order>(&sql)
        .bind(order_id)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| AppError::not_found("Không tìm thấy đơn hàng."))?;
    if !is_admin && order.buyer_id != user_id && order.seller_id != user_id {
        return Err(AppError::forbidden("Bạn không có quyền xem đơn hàng này."));
    }
    Ok(order)
}

async fn complete_order_in_tx(conn: &mut PgConnection, order: &Order) -> AppResult<Order> {
    let (_, release_at) =
        escrow::complete_in_tx(conn, order.seller_id, order.id, order.total).await?;
    let sql = format!(
        "UPDATE orders SET status = 'completed', completed_at = now(), \
         release_at = COALESCE($2, release_at) WHERE id = $1 RETURNING {ORDER_COLS}"
    );
    let updated = sqlx::query_as::<_, Order>(&sql)
        .bind(order.id)
        .bind(release_at)
        .fetch_one(conn)
        .await?;
    Ok(updated)
}

/// Người mua bấm xác nhận hoàn tất đơn (`paid` -> `completed`, `escrow` -> `earnings_pending`).
pub async fn confirm_order(db: &PgPool, buyer_id: Uuid, order_id: Uuid) -> AppResult<Order> {
    let mut tx = db.begin().await?;
    let sql = format!("SELECT {ORDER_COLS} FROM orders WHERE id = $1 FOR UPDATE");
    let order = sqlx::query_as::<_, Order>(&sql)
        .bind(order_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Không tìm thấy đơn hàng."))?;
    if order.buyer_id != buyer_id {
        return Err(AppError::forbidden(
            "Chỉ người mua của đơn hàng mới được xác nhận hoàn tất.",
        ));
    }
    if order.status == "completed" {
        return Ok(order);
    }
    if order.status != "paid" {
        return Err(AppError::conflict(
            "invalid_order_status",
            "Đơn hàng không ở trạng thái đã thanh toán (paid) để xác nhận.",
        ));
    }
    let updated = complete_order_in_tx(&mut tx, &order).await?;
    tx.commit().await?;
    Ok(updated)
}

/// Job nền: tự hoàn tất các đơn `paid` quá hạn `auto_complete_at` không có tranh chấp.
pub async fn auto_complete_due(db: &PgPool, limit: i64) -> AppResult<usize> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM orders WHERE status = 'paid' AND auto_complete_at <= now() \
         ORDER BY auto_complete_at ASC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(db)
    .await?;
    let mut completed = 0usize;
    for id in ids {
        let mut tx = db.begin().await?;
        let sql = format!("SELECT {ORDER_COLS} FROM orders WHERE id = $1 FOR UPDATE");
        let Some(order) = sqlx::query_as::<_, Order>(&sql)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            continue;
        };
        if order.status != "paid" || order.auto_complete_at > Utc::now() {
            continue;
        }
        complete_order_in_tx(&mut tx, &order).await?;
        tx.commit().await?;
        completed += 1;
    }
    Ok(completed)
}

/// Cấp signed URL có hạn 2 phút để người đã mua tải tệp, giới hạn `download_limit` lượt/đơn.
/// Người mua luôn tải được tệp của phiên bản mới nhất đã duyệt (cập nhật miễn phí) hoặc phiên bản lúc mua.
pub async fn request_download(
    db: &PgPool,
    storage: &dyn FileStorage,
    user_id: Uuid,
    order_id: Uuid,
    file_id: Option<Uuid>,
    ip: &str,
) -> AppResult<DownloadResponse> {
    let mut tx = db.begin().await?;
    let sql = format!("SELECT {ORDER_COLS} FROM orders WHERE id = $1 FOR UPDATE");
    let order = sqlx::query_as::<_, Order>(&sql)
        .bind(order_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Không tìm thấy đơn hàng."))?;

    if order.buyer_id != user_id {
        return Err(AppError::forbidden(
            "Bạn chưa mua sản phẩm này nên không được tải tệp.",
        ));
    }
    if order.status != "paid" && order.status != "completed" {
        return Err(AppError::forbidden(
            "Đơn hàng đang tranh chấp hoặc đã hoàn tiền nên không thể tải tệp.",
        ));
    }

    let cfg = settings::load(&mut tx).await?;
    let used =
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM download_logs WHERE order_id = $1")
            .bind(order.id)
            .fetch_one(&mut *tx)
            .await?;
    if used >= cfg.download_limit {
        return Err(AppError::unprocessable(
            "download_limit_exceeded",
            format!("Đơn hàng đã đạt giới hạn {} lượt tải.", cfg.download_limit),
        ));
    }

    // Cho phép tải tệp từ phiên bản hiện tại đang duyệt của sản phẩm hoặc phiên bản lúc mua.
    let published_ver = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT published_version_id FROM products WHERE id = $1",
    )
    .bind(order.product_id)
    .fetch_one(&mut *tx)
    .await?;
    let target_ver = published_ver.unwrap_or(order.version_id);

    let file_row = match file_id {
        Some(fid) => {
            sqlx::query_as::<_, (Uuid, String, String, String)>(
                "SELECT id, object_key, filename, mime FROM product_files \
                 WHERE id = $1 AND version_id IN ($2, $3)",
            )
            .bind(fid)
            .bind(target_ver)
            .bind(order.version_id)
            .fetch_optional(&mut *tx)
            .await?
        }
        None => {
            sqlx::query_as::<_, (Uuid, String, String, String)>(
                "SELECT id, object_key, filename, mime FROM product_files \
                 WHERE version_id = $1 ORDER BY created_at ASC LIMIT 1",
            )
            .bind(target_ver)
            .fetch_optional(&mut *tx)
            .await?
        }
    }
    .ok_or_else(|| AppError::not_found("Không tìm thấy tệp tải về cho đơn hàng này."))?;
    let (resolved_file_id, object_key, filename, mime) = file_row;

    let ip_hash = hex::encode(Sha256::digest(ip.as_bytes()));
    sqlx::query(
        "INSERT INTO download_logs (order_id, file_id, user_id, ip_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(order.id)
    .bind(resolved_file_id)
    .bind(user_id)
    .bind(&ip_hash)
    .execute(&mut *tx)
    .await?;

    let download = storage
        .presign_get(&object_key, &filename, &mime, DOWNLOAD_TTL_SECS)
        .await?;
    tx.commit().await?;

    Ok(DownloadResponse {
        file_id: resolved_file_id,
        filename,
        download,
        downloads_used: used + 1,
        download_limit: cfg.download_limit,
    })
}

/// Mở tranh chấp trong thời gian giữ tiền (khi đơn ở `paid` hoặc `completed` còn `earnings_holds = pending`).
/// Đóng băng đơn hàng và khoản giữ để job không tự chốt/nhả tiền trước khi admin phân xử.
pub async fn open_dispute(
    db: &PgPool,
    buyer_id: Uuid,
    order_id: Uuid,
    reason: &str,
    evidence: Value,
) -> AppResult<Dispute> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 2000 {
        return Err(AppError::bad_request(
            "invalid_reason",
            "Lý do khiếu nại phải dài từ 1 đến 2000 ký tự.",
        ));
    }

    let mut tx = db.begin().await?;
    let sql = format!("SELECT {ORDER_COLS} FROM orders WHERE id = $1 FOR UPDATE");
    let order = sqlx::query_as::<_, Order>(&sql)
        .bind(order_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| AppError::not_found("Không tìm thấy đơn hàng."))?;
    if order.buyer_id != buyer_id {
        return Err(AppError::forbidden(
            "Chỉ người mua của đơn hàng mới được mở khiếu nại.",
        ));
    }
    if order.status == "disputed" {
        return Err(AppError::conflict(
            "dispute_already_open",
            "Đơn hàng này đã có khiếu nại đang mở.",
        ));
    }
    if order.status == "refunded" {
        return Err(AppError::conflict(
            "order_refunded",
            "Đơn hàng đã được hoàn tiền.",
        ));
    }

    if order.status == "completed" {
        let hold_status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM earnings_holds WHERE order_ref = $1 FOR UPDATE",
        )
        .bind(order.id)
        .fetch_optional(&mut *tx)
        .await?;
        if hold_status.as_deref() != Some("pending") {
            return Err(AppError::conflict(
                "dispute_window_closed",
                "Tiền đã hết thời gian giữ và chuyển sang khả dụng, không mở tranh chấp tự động được.",
            ));
        }
        sqlx::query("UPDATE earnings_holds SET status = 'frozen' WHERE order_ref = $1")
            .bind(order.id)
            .execute(&mut *tx)
            .await?;
    }

    sqlx::query("UPDATE orders SET pre_dispute_status = $2, status = 'disputed' WHERE id = $1")
        .bind(order.id)
        .bind(&order.status)
        .execute(&mut *tx)
        .await?;

    let dispute = sqlx::query_as::<_, Dispute>(
        "INSERT INTO disputes (order_id, opened_by, reason, evidence) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id, order_id, opened_by, reason, evidence, status, resolution, \
                   resolution_note, resolved_by, resolved_at, created_at",
    )
    .bind(order.id)
    .bind(buyer_id)
    .bind(reason)
    .bind(&evidence)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::conflict("dispute_already_open", "Đơn hàng đã có tranh chấp.")
        } else {
            AppError::from(e)
        }
    })?;

    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(buyer_id),
            action: "dispute.open",
            target_type: "dispute",
            target_id: Some(dispute.id.to_string()),
            before: Some(json!({ "order_status": order.status })),
            after: Some(json!({ "order_id": order.id, "reason": reason })),
            ip: None,
        },
    )
    .await?;

    let _ = notifications::create(
        &mut tx,
        order.seller_id,
        "dispute_opened",
        &format!("Đơn hàng {} có khiếu nại mới từ người mua.", order.id),
        Some(&format!("dispute_open:{}", dispute.id)),
    )
    .await;

    tx.commit().await?;
    Ok(dispute)
}

pub async fn list_disputes_admin(
    db: &PgPool,
    status: Option<&str>,
    limit: i64,
) -> AppResult<Vec<Dispute>> {
    let rows = sqlx::query_as::<_, Dispute>(
        "SELECT id, order_id, opened_by, reason, evidence, status, resolution, \
                resolution_note, resolved_by, resolved_at, created_at \
         FROM disputes \
         WHERE ($1::text IS NULL OR status = $1) \
         ORDER BY created_at DESC LIMIT $2",
    )
    .bind(status)
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Admin phân xử tranh chấp:
/// - `refund`: hoàn tiền về `wallet_spendable` của người mua (từ `escrow` hoặc `earnings_pending`).
/// - `release`: trả về luồng người bán (từ `escrow` -> `earnings_pending`, hoặc mở khóa khoản giữ `frozen` -> `pending`).
pub async fn resolve_dispute(
    db: &PgPool,
    ctx: AdminCtx<'_>,
    dispute_id: Uuid,
    resolution: DisputeResolution,
    note: &str,
) -> AppResult<Dispute> {
    let note = note.trim();
    if note.is_empty() || note.chars().count() > 1000 {
        return Err(AppError::bad_request(
            "invalid_note",
            "Ghi chú phân xử phải dài từ 1 đến 1000 ký tự.",
        ));
    }

    let mut tx = db.begin().await?;
    require_admin(&mut tx, ctx.admin_id).await?;

    let dispute = sqlx::query_as::<_, Dispute>(
        "SELECT id, order_id, opened_by, reason, evidence, status, resolution, \
                resolution_note, resolved_by, resolved_at, created_at \
         FROM disputes WHERE id = $1 FOR UPDATE",
    )
    .bind(dispute_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy tranh chấp."))?;
    if dispute.status != "open" {
        return Err(AppError::conflict(
            "dispute_already_resolved",
            "Tranh chấp này đã được xử lý trước đó.",
        ));
    }

    let sql = format!("SELECT {ORDER_COLS} FROM orders WHERE id = $1 FOR UPDATE");
    let order = sqlx::query_as::<_, Order>(&sql)
        .bind(dispute.order_id)
        .fetch_one(&mut *tx)
        .await?;
    let pre = order.pre_dispute_status.as_deref().unwrap_or("paid");

    match resolution {
        DisputeResolution::Refund => {
            if pre == "paid" {
                escrow::refund_from_escrow_in_tx(&mut tx, order.buyer_id, order.id, order.total)
                    .await?;
            } else {
                escrow::refund_from_pending_in_tx(
                    &mut tx,
                    order.seller_id,
                    order.buyer_id,
                    order.id,
                    order.total,
                )
                .await?;
            }
            sqlx::query("UPDATE orders SET status = 'refunded', refunded_at = now() WHERE id = $1")
                .bind(order.id)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE products SET sales_count = GREATEST(0, sales_count - 1) WHERE id = $1",
            )
            .bind(order.product_id)
            .execute(&mut *tx)
            .await?;
        }
        DisputeResolution::Release => {
            if pre == "paid" {
                complete_order_in_tx(&mut tx, &order).await?;
            } else {
                sqlx::query(
                    "UPDATE earnings_holds SET status = 'pending' \
                     WHERE order_ref = $1 AND status = 'frozen'",
                )
                .bind(order.id)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE orders SET status = 'completed' WHERE id = $1")
                    .bind(order.id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }

    let updated = sqlx::query_as::<_, Dispute>(
        "UPDATE disputes \
         SET status = 'resolved', resolution = $2, resolution_note = $3, \
             resolved_by = $4, resolved_at = now() \
         WHERE id = $1 \
         RETURNING id, order_id, opened_by, reason, evidence, status, resolution, \
                   resolution_note, resolved_by, resolved_at, created_at",
    )
    .bind(dispute_id)
    .bind(resolution.as_str())
    .bind(note)
    .bind(ctx.admin_id)
    .fetch_one(&mut *tx)
    .await?;

    audit::log(
        &mut tx,
        AuditEntry {
            actor_id: Some(ctx.admin_id),
            action: "dispute.resolve",
            target_type: "dispute",
            target_id: Some(dispute_id.to_string()),
            before: Some(json!({ "status": "open", "pre_dispute_status": pre })),
            after: Some(json!({
                "status": "resolved",
                "resolution": resolution.as_str(),
                "note": note,
                "order_id": order.id,
            })),
            ip: Some(ctx.ip),
        },
    )
    .await?;

    let msg = match resolution {
        DisputeResolution::Refund => format!(
            "Tranh chấp đơn {} đã xử lý: hoàn tiền cho người mua.",
            order.id
        ),
        DisputeResolution::Release => format!(
            "Tranh chấp đơn {} đã xử lý: giữ quyền lợi cho người bán.",
            order.id
        ),
    };
    let _ = notifications::create(&mut tx, order.buyer_id, "dispute_resolved", &msg, None).await;
    let _ = notifications::create(&mut tx, order.seller_id, "dispute_resolved", &msg, None).await;

    tx.commit().await?;
    Ok(updated)
}

/// Đánh giá sản phẩm: chỉ người đã mua (`paid` hoặc `completed`) mới được đánh giá, mỗi đơn 1 lần.
pub async fn create_review(
    db: &PgPool,
    buyer_id: Uuid,
    product_id: Uuid,
    rating: i16,
    body: &str,
) -> AppResult<Review> {
    if !(1..=5).contains(&rating) {
        return Err(AppError::bad_request(
            "invalid_rating",
            "Điểm đánh giá phải từ 1 đến 5 sao.",
        ));
    }
    let body = body.trim();
    if body.chars().count() > 2000 {
        return Err(AppError::bad_request(
            "invalid_review_body",
            "Nội dung đánh giá dài tối đa 2000 ký tự.",
        ));
    }

    let mut tx = db.begin().await?;
    let order_row = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT id, shop_id FROM orders \
         WHERE buyer_id = $1 AND product_id = $2 AND status IN ('paid', 'completed') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(buyer_id)
    .bind(product_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::forbidden("Chỉ người đã mua sản phẩm này mới được viết đánh giá."))?;
    let (order_id, shop_id) = order_row;

    let review = sqlx::query_as::<_, Review>(
        "INSERT INTO reviews (order_id, product_id, buyer_id, rating, body) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, order_id, product_id, buyer_id, rating, body, created_at",
    )
    .bind(order_id)
    .bind(product_id)
    .bind(buyer_id)
    .bind(rating)
    .bind(body)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::conflict("already_reviewed", "Bạn đã đánh giá sản phẩm này rồi.")
        } else {
            AppError::from(e)
        }
    })?;

    // Cập nhật điểm trung bình của sản phẩm và gian hàng.
    sqlx::query(
        "UPDATE products SET \
            rating_avg = (SELECT avg(rating)::float8 FROM reviews WHERE product_id = $1), \
            rating_count = (SELECT count(*)::int FROM reviews WHERE product_id = $1) \
         WHERE id = $1",
    )
    .bind(product_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "UPDATE shops SET \
            rating_avg = (SELECT avg(r.rating)::float8 FROM reviews r \
                          JOIN products p ON p.id = r.product_id WHERE p.shop_id = $1) \
         WHERE id = $1",
    )
    .bind(shop_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(review)
}

pub async fn list_product_reviews(db: &PgPool, product_id: Uuid) -> AppResult<Vec<PublicReview>> {
    let rows = sqlx::query_as::<_, PublicReview>(
        "SELECT r.id, r.product_id, r.buyer_id, u.display_name AS buyer_name, \
                r.rating, r.body, r.created_at \
         FROM reviews r JOIN users u ON u.id = r.buyer_id \
         WHERE r.product_id = $1 ORDER BY r.created_at DESC LIMIT 100",
    )
    .bind(product_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}
