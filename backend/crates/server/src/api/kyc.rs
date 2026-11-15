use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::domain::kyc;
use crate::error::AppResult;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct SubmitKycRequest {
    pub full_name: String,
    pub bank_name: String,
    pub account_no: String,
}

pub async fn submit(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<SubmitKycRequest>,
) -> AppResult<Json<kyc::KycStatus>> {
    let status = kyc::submit(
        &state.db,
        &state.crypto,
        user.id,
        &req.full_name,
        &req.bank_name,
        &req.account_no,
    )
    .await?;
    Ok(Json(status))
}

pub async fn get(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let status = kyc::get(&state.db, &state.crypto, user.id).await?;
    Ok(Json(json!({ "kyc": status })))
}
