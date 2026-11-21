//! Quy trình duyệt thủ công bắt buộc mọi sản phẩm và mọi phiên bản:
//! `draft` -> `pending_review` -> `testing` -> `approved` | `rejected`.
//! Chỉ `reviewer` hoặc `admin` được duyệt; cấm tự duyệt sản phẩm của chính mình.
//! Danh mục rủi ro cao (`risk_level = 'high'`, như MMO Tool, Bot) có checklist riêng chặt hơn.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::notifications;
use super::products::{self, ProductFilePublic, ProductVersion};
use crate::audit::{self, AuditEntry};
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Serialize)]
pub struct QueueItem {
    pub version: ProductVersion,
    pub product_title: String,
    pub product_slug: String,
    pub category_slug: String,
    pub risk_level: String,
    pub seller_id: Uuid,
    pub files: Vec<ProductFilePublic>,
    pub auto_reports: Vec<Value>,
}

#[derive(Debug, sqlx::FromRow)]
struct QueueRow {
    id: Uuid,
    product_id: Uuid,
    version: String,
    changelog: String,
    status: String,
    reject_reason: Option<String>,
    commitment_accepted: bool,
    submitted_at: Option<DateTime<Utc>>,
    claimed_by: Option<Uuid>,
    claimed_at: Option<DateTime<Utc>>,
    reviewed_by: Option<Uuid>,
    reviewed_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    product_title: String,
    product_slug: String,
    category_slug: String,
    risk_level: String,
    seller_id: Uuid,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManualChecklist {
    pub runs_as_described: bool,
    pub no_malicious_behavior: bool,
    pub no_copyright_violation: bool,
    pub adequate_instructions: bool,
    /// Bắt buộc với danh mục rủi ro cao (`mmo-tool`, `bot-chat-discord`).
    #[serde(default)]
    pub no_account_theft: Option<bool>,
    #[serde(default)]
    pub no_tos_abuse: Option<bool>,
    #[serde(default)]
    pub sandbox_verified: Option<bool>,
    #[serde(default)]
    pub notes: String,
}

impl ManualChecklist {
    pub fn passed_for(&self, risk_level: &str) -> bool {
        let base = self.runs_as_described
            && self.no_malicious_behavior
            && self.no_copyright_violation
            && self.adequate_instructions;
        if risk_level == "high" {
            base && self.no_account_theft.unwrap_or(false)
                && self.no_tos_abuse.unwrap_or(false)
                && self.sandbox_verified.unwrap_or(false)
        } else {
            base
        }
    }
}

pub async fn list_queue(
    db: &PgPool,
    status_filter: Option<&str>,
    limit: i64,
) -> AppResult<Vec<QueueItem>> {
    let limit = limit.clamp(1, 200);
    let rows = sqlx::query_as::<_, QueueRow>(
        "SELECT v.id, v.product_id, v.version, v.changelog, v.status, v.reject_reason, \
                v.commitment_accepted, v.submitted_at, v.claimed_by, v.claimed_at, \
                v.reviewed_by, v.reviewed_at, v.created_at, \
                p.title AS product_title, p.slug AS product_slug, \
                c.slug AS category_slug, c.risk_level, s.owner_id AS seller_id \
         FROM product_versions v \
         JOIN products p ON p.id = v.product_id \
         JOIN categories c ON c.id = p.category_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE ($1::text IS NULL AND v.status IN ('pending_review', 'testing')) \
            OR ($1::text IS NOT NULL AND v.status = $1) \
         ORDER BY v.submitted_at ASC NULLS LAST, v.created_at ASC \
         LIMIT $2",
    )
    .bind(status_filter)
    .bind(limit)
    .fetch_all(db)
    .await?;

    let mut conn = db.acquire().await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let version = ProductVersion {
            id: r.id,
            product_id: r.product_id,
            version: r.version,
            changelog: r.changelog,
            status: r.status,
            reject_reason: r.reject_reason,
            commitment_accepted: r.commitment_accepted,
            submitted_at: r.submitted_at,
            claimed_by: r.claimed_by,
            claimed_at: r.claimed_at,
            reviewed_by: r.reviewed_by,
            reviewed_at: r.reviewed_at,
            created_at: r.created_at,
        };
        let files = sqlx::query_as::<_, ProductFilePublic>(
            "SELECT id, version_id, filename, size, mime, sha256, scan_status, created_at \
             FROM product_files WHERE version_id = $1 ORDER BY created_at ASC",
        )
        .bind(version.id)
        .fetch_all(&mut *conn)
        .await?;
        let auto_reports = products::version_auto_reports(&mut conn, version.id).await?;
        items.push(QueueItem {
            version,
            product_title: r.product_title,
            product_slug: r.product_slug,
            category_slug: r.category_slug,
            risk_level: r.risk_level,
            seller_id: r.seller_id,
            files,
            auto_reports,
        });
    }
    Ok(items)
}

/// Nhận test phiên bản (`pending_review` -> `testing`). Cấm người bán tự nhận test sản phẩm mình.
pub async fn claim_version(
    db: &PgPool,
    reviewer_id: Uuid,
    version_id: Uuid,
) -> AppResult<ProductVersion> {
    let mut tx = db.begin().await?;
    let row = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT v.status, s.owner_id \
         FROM product_versions v \
         JOIN products p ON p.id = v.product_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE v.id = $1 FOR UPDATE OF v",
    )
    .bind(version_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy phiên bản."))?;
    let (status, seller_id) = row;
    if seller_id == reviewer_id {
        return Err(AppError::forbidden(
            "Không được tự kiểm duyệt sản phẩm của chính mình.",
        ));
    }
    if status != "pending_review" {
        return Err(AppError::conflict(
            "invalid_transition",
            "Phiên bản không ở trạng thái chờ duyệt (pending_review).",
        ));
    }

    let updated = sqlx::query_as::<_, ProductVersion>(
        "UPDATE product_versions SET status = 'testing', claimed_by = $2, claimed_at = now() \
         WHERE id = $1 \
         RETURNING id, product_id, version, changelog, status, reject_reason, commitment_accepted, \
                   submitted_at, claimed_by, claimed_at, reviewed_by, reviewed_at, created_at",
    )
    .bind(version_id)
    .bind(reviewer_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(updated)
}

/// Ghi kết quả checklist test thủ công vào `review_reports`.
pub async fn submit_checklist(
    db: &PgPool,
    reviewer_id: Uuid,
    is_admin: bool,
    version_id: Uuid,
    checklist: ManualChecklist,
) -> AppResult<Value> {
    let mut tx = db.begin().await?;
    let row = sqlx::query_as::<_, (String, Option<Uuid>, Uuid, String)>(
        "SELECT v.status, v.claimed_by, s.owner_id, c.risk_level \
         FROM product_versions v \
         JOIN products p ON p.id = v.product_id \
         JOIN categories c ON c.id = p.category_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE v.id = $1 FOR UPDATE OF v",
    )
    .bind(version_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy phiên bản."))?;
    let (status, claimed_by, seller_id, risk_level) = row;
    if seller_id == reviewer_id {
        return Err(AppError::forbidden(
            "Không được tự kiểm duyệt sản phẩm của chính mình.",
        ));
    }
    if status != "testing" {
        return Err(AppError::conflict(
            "invalid_transition",
            "Cần nhận test (claim) phiên bản trước khi gửi checklist.",
        ));
    }
    if !is_admin && claimed_by != Some(reviewer_id) {
        return Err(AppError::forbidden(
            "Phiên bản đang được kiểm duyệt viên khác nhận test.",
        ));
    }

    let passed = checklist.passed_for(&risk_level);
    let result_json = json!({
        "passed": passed,
        "risk_level": risk_level,
    });
    let checklist_json = serde_json::to_value(&checklist)
        .map_err(|e| AppError::internal(format!("lỗi serialize checklist: {e}")))?;

    let report_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO review_reports (version_id, kind, result, checklist, reviewer_id) \
         VALUES ($1, 'manual', $2, $3, $4) RETURNING id",
    )
    .bind(version_id)
    .bind(&result_json)
    .bind(&checklist_json)
    .bind(reviewer_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(json!({
        "report_id": report_id,
        "passed": passed,
        "risk_level": risk_level,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

impl Decision {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approved" | "approve" => Some(Self::Approve),
            "rejected" | "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

pub struct DecisionCtx<'a> {
    pub reviewer_id: Uuid,
    pub is_admin: bool,
    pub ip: &'a str,
}

/// Quyết định duyệt (`approved`) hoặc từ chối (`rejected`).
/// - `approved`: bắt buộc đã có báo cáo checklist thủ công đạt (`passed = true`);
///   cập nhật `products.published_version_id` và chuyển `products.status = 'active'`.
/// - `rejected`: bắt buộc có lý do; không đụng tới `published_version_id` cũ đang bán.
pub async fn decide_version(
    db: &PgPool,
    ctx: DecisionCtx<'_>,
    version_id: Uuid,
    decision: Decision,
    reason: Option<&str>,
) -> AppResult<ProductVersion> {
    let mut tx = db.begin().await?;
    let row = sqlx::query_as::<_, (Uuid, String, Option<Uuid>, Uuid, String, String, String)>(
        "SELECT v.product_id, v.status, v.claimed_by, s.owner_id, p.status, p.title, v.version \
         FROM product_versions v \
         JOIN products p ON p.id = v.product_id \
         JOIN shops s ON s.id = p.shop_id \
         WHERE v.id = $1 FOR UPDATE OF v, p",
    )
    .bind(version_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::not_found("Không tìm thấy phiên bản."))?;
    let (product_id, status, claimed_by, seller_id, prod_status, prod_title, ver_label) = row;

    if seller_id == ctx.reviewer_id {
        return Err(AppError::forbidden(
            "Không được tự duyệt sản phẩm của chính mình.",
        ));
    }
    if status != "testing" {
        return Err(AppError::conflict(
            "invalid_transition",
            "Phiên bản phải ở trạng thái đang test (testing) trước khi ra quyết định.",
        ));
    }
    if !ctx.is_admin && claimed_by != Some(ctx.reviewer_id) {
        return Err(AppError::forbidden(
            "Phiên bản đang được kiểm duyệt viên khác nhận test.",
        ));
    }

    let updated = match decision {
        Decision::Approve => {
            if prod_status == "taken_down" {
                return Err(AppError::conflict(
                    "product_taken_down",
                    "Sản phẩm đang bị gỡ tạm thời, không thể duyệt lên sàn.",
                ));
            }
            let latest_passed = sqlx::query_scalar::<_, Option<bool>>(
                "SELECT (result->>'passed')::boolean FROM review_reports \
                 WHERE version_id = $1 AND kind = 'manual' \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(version_id)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
            if latest_passed != Some(true) {
                return Err(AppError::unprocessable(
                    "checklist_required",
                    "Cần hoàn thành đầy đủ checklist test thủ công (passed=true) trước khi duyệt.",
                ));
            }
            let ver = sqlx::query_as::<_, ProductVersion>(
                "UPDATE product_versions \
                 SET status = 'approved', reject_reason = NULL, reviewed_by = $2, reviewed_at = now() \
                 WHERE id = $1 \
                 RETURNING id, product_id, version, changelog, status, reject_reason, commitment_accepted, \
                           submitted_at, claimed_by, claimed_at, reviewed_by, reviewed_at, created_at",
            )
            .bind(version_id)
            .bind(ctx.reviewer_id)
            .fetch_one(&mut *tx)
            .await?;

            sqlx::query(
                "UPDATE products SET published_version_id = $2, status = 'active', updated_at = now() \
                 WHERE id = $1",
            )
            .bind(product_id)
            .bind(version_id)
            .execute(&mut *tx)
            .await?;

            audit::log(
                &mut tx,
                AuditEntry {
                    actor_id: Some(ctx.reviewer_id),
                    action: "product_version.approve",
                    target_type: "product_version",
                    target_id: Some(version_id.to_string()),
                    before: Some(json!({ "status": status })),
                    after: Some(json!({ "status": "approved", "product_id": product_id })),
                    ip: Some(ctx.ip),
                },
            )
            .await?;

            let _ = notifications::create(
                &mut tx,
                seller_id,
                "version_approved",
                &format!("Phiên bản {ver_label} của '{prod_title}' đã được duyệt và lên sàn."),
                Some(&format!("ver_approved:{version_id}")),
            )
            .await;

            ver
        }
        Decision::Reject => {
            let reason = reason
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .ok_or_else(|| {
                    AppError::bad_request(
                        "reason_required",
                        "Từ chối phiên bản bắt buộc phải nêu lý do.",
                    )
                })?;
            if reason.chars().count() > 1000 {
                return Err(AppError::bad_request(
                    "invalid_reason",
                    "Lý do từ chối dài tối đa 1000 ký tự.",
                ));
            }
            let ver = sqlx::query_as::<_, ProductVersion>(
                "UPDATE product_versions \
                 SET status = 'rejected', reject_reason = $2, reviewed_by = $3, reviewed_at = now() \
                 WHERE id = $1 \
                 RETURNING id, product_id, version, changelog, status, reject_reason, commitment_accepted, \
                           submitted_at, claimed_by, claimed_at, reviewed_by, reviewed_at, created_at",
            )
            .bind(version_id)
            .bind(reason)
            .bind(ctx.reviewer_id)
            .fetch_one(&mut *tx)
            .await?;

            audit::log(
                &mut tx,
                AuditEntry {
                    actor_id: Some(ctx.reviewer_id),
                    action: "product_version.reject",
                    target_type: "product_version",
                    target_id: Some(version_id.to_string()),
                    before: Some(json!({ "status": status })),
                    after: Some(json!({ "status": "rejected", "reason": reason })),
                    ip: Some(ctx.ip),
                },
            )
            .await?;

            let _ = notifications::create(
                &mut tx,
                seller_id,
                "version_rejected",
                &format!("Phiên bản {ver_label} của '{prod_title}' bị từ chối: {reason}"),
                None,
            )
            .await;

            ver
        }
    };

    tx.commit().await?;
    Ok(updated)
}
