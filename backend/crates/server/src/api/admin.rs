use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::api::idempotency_key;
use crate::auth::{AdminUser, ClientIp};
use crate::domain::deposits::{self, ConfirmOutcome, ConfirmSource};
use crate::domain::{kyc, refund, withdrawals, AdminCtx};
use crate::error::{AppError, AppResult};
use crate::settings;
use crate::state::AppState;

pub async fn get_settings(
    State(state): State<AppState>,
    _admin: AdminUser,
) -> AppResult<Json<Value>> {
    let mut conn = state.db.acquire().await?;
    let all = settings::load_all(&mut conn).await?;
    Ok(Json(json!({ "settings": all })))
}

#[derive(Debug, Deserialize)]
pub struct PatchSettingsRequest {
    pub settings: BTreeMap<String, Value>,
}

pub async fn patch_settings(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Json(req): Json<PatchSettingsRequest>,
) -> AppResult<Json<Value>> {
    let all = settings::update(&state.db, admin.id, &ip, req.settings).await?;
    Ok(Json(json!({ "settings": all })))
}

// ---------- Phase 2: nạp, rút, KYC, hoàn tiền, audit_log ----------

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

fn list_limit(q: &ListQuery) -> i64 {
    q.limit.unwrap_or(100).clamp(1, 500)
}

fn list_status<'a>(q: &'a ListQuery, allowed: &[&str]) -> AppResult<Option<&'a str>> {
    match q.status.as_deref() {
        None => Ok(None),
        Some(s) if allowed.contains(&s) => Ok(Some(s)),
        Some(_) => Err(AppError::bad_request(
            "invalid_status",
            format!("status phải là một trong: {}", allowed.join(", ")),
        )),
    }
}

pub async fn list_deposits(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let status = list_status(&q, &["pending", "confirmed", "cancelled"])?;
    let rows = deposits::list_admin(&state.db, status, list_limit(&q)).await?;
    Ok(Json(json!({ "deposits": rows })))
}

pub async fn confirm_deposit(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    let outcome = deposits::confirm(&state.db, id, None, ConfirmSource::Admin(ctx)).await?;
    let result = match outcome {
        ConfirmOutcome::Confirmed { .. } => "confirmed",
        ConfirmOutcome::AlreadyConfirmed => "already_confirmed",
    };
    Ok(Json(json!({ "result": result })))
}

pub async fn list_withdrawals(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let status = list_status(&q, &["pending", "approved", "rejected"])?;
    let rows = withdrawals::list_admin(&state.db, status, list_limit(&q)).await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        // Admin cần số tài khoản đầy đủ để chuyển khoản, nên giải mã ở đây.
        let kyc = match (
            &r.kyc_full_name,
            &r.kyc_bank_name,
            &r.kyc_account_enc,
            &r.kyc_key_id,
        ) {
            (Some(full_name), Some(bank_name), Some(enc), Some(key_id)) => {
                let account_no = state.crypto.decrypt(key_id, enc)?;
                Some(json!({
                    "full_name": full_name, "bank_name": bank_name, "account_no": account_no
                }))
            }
            _ => None,
        };
        items.push(json!({
            "id": r.id, "user_id": r.user_id, "email": r.email, "amount": r.amount,
            "fee": r.fee, "net": r.net, "status": r.status, "bank_txn_ref": r.bank_txn_ref,
            "reason": r.reason, "created_at": r.created_at, "kyc": kyc
        }));
    }
    Ok(Json(json!({ "withdrawals": items })))
}

#[derive(Debug, Deserialize)]
pub struct ApproveWithdrawalRequest {
    pub bank_txn_ref: String,
}

#[derive(Debug, Deserialize)]
pub struct ReasonRequest {
    pub reason: String,
}

pub async fn approve_withdrawal(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(id): Path<Uuid>,
    Json(req): Json<ApproveWithdrawalRequest>,
) -> AppResult<Json<withdrawals::Withdrawal>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    Ok(Json(
        withdrawals::approve(&state.db, ctx, id, &req.bank_txn_ref).await?,
    ))
}

pub async fn reject_withdrawal(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> AppResult<Json<withdrawals::Withdrawal>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    Ok(Json(
        withdrawals::reject(&state.db, ctx, id, &req.reason).await?,
    ))
}

pub async fn list_kyc(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let status = list_status(&q, &["pending", "approved", "rejected"])?;
    let rows = kyc::list_admin(&state.db, status, list_limit(&q)).await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let account_no = state.crypto.decrypt(&r.key_id, &r.account_no_enc)?;
        items.push(json!({
            "user_id": r.user_id, "email": r.email, "full_name": r.full_name,
            "bank_name": r.bank_name, "account_no": account_no, "status": r.status,
            "reject_reason": r.reject_reason, "submitted_at": r.submitted_at
        }));
    }
    Ok(Json(json!({ "kyc": items })))
}

pub async fn approve_kyc(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(user_id): Path<Uuid>,
) -> AppResult<Json<Value>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    kyc::approve(&state.db, ctx, user_id).await?;
    Ok(Json(json!({ "status": "approved" })))
}

pub async fn reject_kyc(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(user_id): Path<Uuid>,
    Json(req): Json<ReasonRequest>,
) -> AppResult<Json<Value>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    kyc::reject(&state.db, ctx, user_id, &req.reason).await?;
    Ok(Json(json!({ "status": "rejected" })))
}

#[derive(Debug, Deserialize)]
pub struct RefundDepositRequest {
    pub amount: i64,
    pub reason: String,
}

pub async fn refund_deposit(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(req): Json<RefundDepositRequest>,
) -> AppResult<Json<Value>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    let idem = idempotency_key(&headers)?;
    let outcome = refund::refund_deposit(
        &state.db,
        ctx,
        user_id,
        req.amount,
        &req.reason,
        idem.as_deref(),
    )
    .await?;
    Ok(Json(
        json!({ "tx_id": outcome.tx_id, "replayed": outcome.replayed }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub before_id: Option<i64>,
    pub action: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AuditRow {
    pub id: i64,
    pub actor_id: Option<Uuid>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

pub async fn audit_log(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(q): Query<AuditQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, actor_id, action, target_type, target_id, before, after, ip, created_at \
         FROM audit_log \
         WHERE ($1::bigint IS NULL OR id < $1) AND ($2::text IS NULL OR action = $2) \
         ORDER BY id DESC LIMIT $3",
    )
    .bind(q.before_id)
    .bind(q.action.as_deref())
    .bind(limit)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "audit_log": rows })))
}

// ---------- Phase 3: tranh chấp và gỡ sản phẩm ----------

pub async fn list_disputes(
    State(state): State<AppState>,
    _admin: AdminUser,
    Query(q): Query<ListQuery>,
) -> AppResult<Json<Value>> {
    let status = list_status(&q, &["open", "resolved"])?;
    let rows =
        crate::domain::orders::list_disputes_admin(&state.db, status, list_limit(&q)).await?;
    Ok(Json(json!({ "disputes": rows })))
}

#[derive(Debug, Deserialize)]
pub struct ResolveDisputeRequest {
    pub resolution: String,
    pub note: String,
}

pub async fn resolve_dispute(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(dispute_id): Path<Uuid>,
    Json(req): Json<ResolveDisputeRequest>,
) -> AppResult<Json<crate::domain::orders::Dispute>> {
    let res =
        crate::domain::orders::DisputeResolution::parse(&req.resolution).ok_or_else(|| {
            AppError::bad_request(
                "invalid_resolution",
                "resolution phải là 'refund' hoặc 'release'.",
            )
        })?;
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    let dispute =
        crate::domain::orders::resolve_dispute(&state.db, ctx, dispute_id, res, &req.note).await?;
    Ok(Json(dispute))
}

#[derive(Debug, Deserialize)]
pub struct TakedownRequest {
    pub reason: String,
    #[serde(default)]
    pub refund_eligible: bool,
}

pub async fn takedown_product(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    ClientIp(ip): ClientIp,
    Path(product_id): Path<Uuid>,
    Json(req): Json<TakedownRequest>,
) -> AppResult<Json<crate::domain::products::TakedownResult>> {
    let ctx = AdminCtx {
        admin_id: admin.id,
        ip: &ip,
    };
    let res = crate::domain::products::takedown_product(
        &state.db,
        ctx,
        product_id,
        &req.reason,
        req.refund_eligible,
    )
    .await?;
    Ok(Json(res))
}
