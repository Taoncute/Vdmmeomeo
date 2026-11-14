use std::collections::BTreeMap;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{AdminUser, ClientIp};
use crate::error::AppResult;
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
