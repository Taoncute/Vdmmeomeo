use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::domain::wallet;
use crate::error::AppResult;
use crate::state::AppState;

pub async fn get_wallet(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<wallet::WalletBalances>> {
    let mut conn = state.db.acquire().await?;
    Ok(Json(wallet::balances(&mut conn, user.id).await?))
}

#[derive(Debug, Deserialize)]
pub struct EntriesQuery {
    pub before_id: Option<i64>,
    pub limit: Option<i64>,
}

pub async fn get_entries(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<EntriesQuery>,
) -> AppResult<Json<Value>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let entries = wallet::entries(&state.db, user.id, q.before_id, limit).await?;
    let next = if i64::try_from(entries.len()).unwrap_or(0) == limit {
        entries.last().map(|e| e.id)
    } else {
        None
    };
    Ok(Json(json!({ "entries": entries, "next_before_id": next })))
}
