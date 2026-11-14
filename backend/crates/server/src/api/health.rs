use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn readyz(State(state): State<AppState>) -> Response {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (StatusCode::OK, Json(json!({ "status": "ready" }))).into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "readyz: không kết nối được DB");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "db_unavailable" })),
            )
                .into_response()
        }
    }
}
