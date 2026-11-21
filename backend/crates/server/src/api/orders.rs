use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::api::idempotency_key;
use crate::auth::{AuthUser, ClientIp, Role};
use crate::domain::orders;
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateOrderRequest {
    pub product_id: Uuid,
}

pub async fn create_order(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<CreateOrderRequest>,
) -> AppResult<(StatusCode, Json<orders::CreateOrderResult>)> {
    let idem = idempotency_key(&headers)?;
    let res = orders::create_order(&state.db, user.id, req.product_id, idem.as_deref()).await?;
    let status = if res.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(res)))
}

pub async fn list_orders(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let items = orders::list_buyer_orders(&state.db, user.id).await?;
    Ok(Json(json!({ "orders": items })))
}

pub async fn get_order(
    State(state): State<AppState>,
    user: AuthUser,
    Path(order_id): Path<Uuid>,
) -> AppResult<Json<orders::Order>> {
    let order = orders::get_order(&state.db, user.id, user.has_role(Role::Admin), order_id).await?;
    Ok(Json(order))
}

pub async fn confirm_order(
    State(state): State<AppState>,
    user: AuthUser,
    Path(order_id): Path<Uuid>,
) -> AppResult<Json<orders::Order>> {
    let order = orders::confirm_order(&state.db, user.id, order_id).await?;
    Ok(Json(order))
}

#[derive(Debug, Default, Deserialize)]
pub struct DownloadRequest {
    pub file_id: Option<Uuid>,
}

pub async fn request_download(
    State(state): State<AppState>,
    user: AuthUser,
    ClientIp(ip): ClientIp,
    Path(order_id): Path<Uuid>,
    body: Option<Json<DownloadRequest>>,
) -> AppResult<Json<orders::DownloadResponse>> {
    let file_id = body.and_then(|b| b.0.file_id);
    let res = orders::request_download(
        &state.db,
        state.storage.as_ref(),
        user.id,
        order_id,
        file_id,
        &ip,
    )
    .await?;
    Ok(Json(res))
}

/// Phục vụ tệp từ signed URL cục bộ (`STORAGE_BACKEND=local`), luôn kiểm chữ ký HMAC và hạn dùng.
pub async fn download_by_token(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> AppResult<impl IntoResponse> {
    let resolved = state.storage.verify_local_token(&token)?;
    let bytes = state.storage.get(&resolved.object_key).await?;
    let safe_name = resolved.filename.replace(['"', '\\', '\r', '\n'], "_");
    let mut headers = HeaderMap::new();
    if let Ok(ct) = HeaderValue::from_str(&resolved.content_type) {
        headers.insert(header::CONTENT_TYPE, ct);
    }
    if let Ok(disp) = HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, disp);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((StatusCode::OK, headers, bytes))
}

#[derive(Debug, Deserialize)]
pub struct OpenDisputeRequest {
    pub reason: String,
    #[serde(default = "default_evidence")]
    pub evidence: Value,
}

fn default_evidence() -> Value {
    json!([])
}

pub async fn open_dispute(
    State(state): State<AppState>,
    user: AuthUser,
    Path(order_id): Path<Uuid>,
    Json(req): Json<OpenDisputeRequest>,
) -> AppResult<(StatusCode, Json<orders::Dispute>)> {
    let dispute =
        orders::open_dispute(&state.db, user.id, order_id, &req.reason, req.evidence).await?;
    Ok((StatusCode::CREATED, Json(dispute)))
}
