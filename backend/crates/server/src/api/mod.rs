use std::time::Duration;

use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method};
use axum::middleware;
use axum::routing::{get, patch, post};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::error::{AppError, AppResult};
use crate::ratelimit::rate_limit_mw;
use crate::state::AppState;

pub mod admin;
pub mod auth;
pub mod deposits;
pub mod health;
pub mod kyc;
pub mod me;
pub mod notifications;
pub mod stall;
pub mod wallet;
pub mod withdrawals;

/// Đọc header `Idempotency-Key` (tối đa 64 ký tự: chữ, số, `-`, `_`).
pub fn idempotency_key(headers: &HeaderMap) -> AppResult<Option<String>> {
    let Some(raw) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let invalid = || {
        AppError::bad_request(
            "invalid_idempotency_key",
            "Idempotency-Key phải dài 1 đến 64 ký tự gồm chữ, số, '-' hoặc '_'.",
        )
    };
    let key = raw.to_str().map_err(|_| invalid())?.trim();
    let ok = !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(Some(key.to_string()))
    } else {
        Err(invalid())
    }
}

fn v1_routes() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/refresh", post(auth::refresh))
        .route("/auth/logout", post(auth::logout))
        .route("/me", get(me::get_me).patch(me::patch_me))
        .route("/me/kyc", get(kyc::get).post(kyc::submit))
        .route("/wallet", get(wallet::get_wallet))
        .route("/wallet/entries", get(wallet::get_entries))
        .route(
            "/deposits",
            get(deposits::list_deposits).post(deposits::create_deposit),
        )
        .route("/webhooks/payos", post(deposits::payment_webhook))
        .route("/withdrawals", get(withdrawals::list).post(withdrawals::create))
        .route("/withdrawals/preview", post(withdrawals::preview))
        .route("/stall/rent", post(stall::rent))
        .route("/shops/me", patch(stall::patch_my_shop))
        .route("/notifications", get(notifications::list))
        .route(
            "/admin/settings",
            get(admin::get_settings).patch(admin::patch_settings),
        )
        .route("/admin/deposits", get(admin::list_deposits))
        .route("/admin/deposits/:id/confirm", post(admin::confirm_deposit))
        .route("/admin/withdrawals", get(admin::list_withdrawals))
        .route(
            "/admin/withdrawals/:id/approve",
            post(admin::approve_withdrawal),
        )
        .route(
            "/admin/withdrawals/:id/reject",
            post(admin::reject_withdrawal),
        )
        .route("/admin/kyc", get(admin::list_kyc))
        .route("/admin/kyc/:id/approve", post(admin::approve_kyc))
        .route("/admin/kyc/:id/reject", post(admin::reject_kyc))
        .route(
            "/admin/users/:id/refund-deposit",
            post(admin::refund_deposit),
        )
        .route("/admin/audit-log", get(admin::audit_log))
}

pub fn router(state: AppState) -> Router {
    let origins: Vec<HeaderValue> = state
        .config
        .cors_origins
        .iter()
        .filter_map(|o| o.parse().ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-requested-with"),
            HeaderName::from_static("idempotency-key"),
        ])
        .allow_credentials(true)
        .max_age(Duration::from_secs(600));

    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/readyz", get(health::readyz))
        .nest("/api/v1", v1_routes())
        .layer(middleware::from_fn_with_state(state.clone(), rate_limit_mw))
        .layer(cors)
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        .with_state(state)
}
