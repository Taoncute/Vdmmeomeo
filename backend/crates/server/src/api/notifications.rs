use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::domain::notifications;
use crate::error::AppResult;
use crate::state::AppState;

pub async fn list(State(state): State<AppState>, user: AuthUser) -> AppResult<Json<Value>> {
    let items = notifications::list(&state.db, user.id, 50).await?;
    Ok(Json(json!({ "notifications": items })))
}
