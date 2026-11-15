use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::idempotency_key;
use crate::auth::AuthUser;
use crate::domain::withdrawals;
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct AmountRequest {
    pub amount: i64,
}

pub async fn preview(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AmountRequest>,
) -> AppResult<Json<withdrawals::Preview>> {
    Ok(Json(withdrawals::preview(&state.db, user.id, req.amount).await?))
}

pub async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<AmountRequest>,
) -> AppResult<(StatusCode, Json<withdrawals::Withdrawal>)> {
    let idem = idempotency_key(&headers)?;
    let w = withdrawals::create(&state.db, user.id, req.amount, idem.as_deref()).await?;
    Ok((StatusCode::CREATED, Json(w)))
}

pub async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let rows = withdrawals::list_for_user(&state.db, user.id, 50).await?;
    Ok(Json(json!({ "withdrawals": rows })))
}
