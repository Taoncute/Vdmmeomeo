use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::{ClientIp, ReviewerUser, Role};
use crate::domain::review_queue::{self, Decision, DecisionCtx, ManualChecklist};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct QueueQuery {
    pub status: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_queue(
    State(state): State<AppState>,
    _reviewer: ReviewerUser,
    Query(q): Query<QueueQuery>,
) -> AppResult<Json<Value>> {
    if let Some(s) = q.status.as_deref() {
        if !matches!(s, "pending_review" | "testing" | "approved" | "rejected") {
            return Err(AppError::bad_request(
                "invalid_status",
                "status phải là pending_review, testing, approved hoặc rejected.",
            ));
        }
    }
    let items =
        review_queue::list_queue(&state.db, q.status.as_deref(), q.limit.unwrap_or(50)).await?;
    Ok(Json(json!({ "queue": items })))
}

pub async fn claim_version(
    State(state): State<AppState>,
    ReviewerUser(user): ReviewerUser,
    Path(version_id): Path<Uuid>,
) -> AppResult<Json<crate::domain::products::ProductVersion>> {
    let ver = review_queue::claim_version(&state.db, user.id, version_id).await?;
    Ok(Json(ver))
}

pub async fn submit_checklist(
    State(state): State<AppState>,
    ReviewerUser(user): ReviewerUser,
    Path(version_id): Path<Uuid>,
    Json(checklist): Json<ManualChecklist>,
) -> AppResult<Json<Value>> {
    let res = review_queue::submit_checklist(
        &state.db,
        user.id,
        user.has_role(Role::Admin),
        version_id,
        checklist,
    )
    .await?;
    Ok(Json(res))
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    pub decision: String,
    pub reason: Option<String>,
}

pub async fn decide_version(
    State(state): State<AppState>,
    ReviewerUser(user): ReviewerUser,
    ClientIp(ip): ClientIp,
    Path(version_id): Path<Uuid>,
    Json(req): Json<DecisionRequest>,
) -> AppResult<Json<crate::domain::products::ProductVersion>> {
    let decision = Decision::parse(&req.decision).ok_or_else(|| {
        AppError::bad_request(
            "invalid_decision",
            "decision phải là 'approved' hoặc 'rejected'.",
        )
    })?;
    let ctx = DecisionCtx {
        reviewer_id: user.id,
        is_admin: user.has_role(Role::Admin),
        ip: &ip,
    };
    let ver =
        review_queue::decide_version(&state.db, ctx, version_id, decision, req.reason.as_deref())
            .await?;
    Ok(Json(ver))
}
