use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::Deserialize;

use crate::api::idempotency_key;
use crate::auth::AuthUser;
use crate::domain::stall::{self, Source};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct RentRequest {
    /// `spendable` (ví nạp) hoặc `earnings` (doanh thu rút được).
    pub source: String,
    /// Bắt buộc ở lần thuê đầu tiên; các lần gia hạn thì bỏ qua.
    pub shop_name: Option<String>,
}

pub async fn rent(
    State(state): State<AppState>,
    user: AuthUser,
    headers: HeaderMap,
    Json(req): Json<RentRequest>,
) -> AppResult<(StatusCode, Json<stall::RentResult>)> {
    let source = Source::parse(&req.source).ok_or_else(|| {
        AppError::bad_request("invalid_source", "source phải là spendable hoặc earnings.")
    })?;
    let idem = idempotency_key(&headers)?;
    let result = stall::rent(
        &state.db,
        user.id,
        source,
        req.shop_name.as_deref(),
        idem.as_deref(),
    )
    .await?;
    let status = if result.created_shop {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(result)))
}

#[derive(Debug, Deserialize)]
pub struct PatchShopRequest {
    pub bio: Option<String>,
    pub avatar_url: Option<String>,
}

pub async fn patch_my_shop(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<PatchShopRequest>,
) -> AppResult<Json<stall::Shop>> {
    let shop = stall::update_profile(
        &state.db,
        user.id,
        req.bio.as_deref(),
        req.avatar_url.as_deref(),
    )
    .await?;
    Ok(Json(shop))
}
