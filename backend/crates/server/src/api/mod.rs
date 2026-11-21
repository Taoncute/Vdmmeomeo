use std::time::Duration;

use axum::extract::DefaultBodyLimit;
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
pub mod orders;
pub mod products;
pub mod review;
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
    // Cho phép multipart vượt mức mặc định 2MB của Axum; giới hạn thực tế lấy từ settings.max_upload_mb.
    let upload_route = Router::new()
        .route("/versions/:id/files", post(products::upload_version_file))
        .layer(DefaultBodyLimit::max(110 * 1024 * 1024));

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
        .route(
            "/withdrawals",
            get(withdrawals::list).post(withdrawals::create),
        )
        .route("/withdrawals/preview", post(withdrawals::preview))
        .route("/stall/rent", post(stall::rent))
        .route("/shops/me", patch(stall::patch_my_shop))
        .route("/notifications", get(notifications::list))
        // Phase 3: danh mục, sản phẩm, phiên bản, đánh giá
        .route("/categories", get(products::list_categories))
        .route(
            "/products",
            get(products::list_products).post(products::create_product),
        )
        .route(
            "/products/:id",
            get(products::get_product).patch(products::update_product),
        )
        .route(
            "/products/:id/reviews",
            get(products::list_reviews).post(products::create_review),
        )
        .route("/products/:id/versions", post(products::create_version))
        .route("/shops/:id", get(products::get_shop))
        .route("/seller/products", get(products::list_seller_products))
        .route("/versions/:id/submit", post(products::submit_version))
        // Phase 3: hàng chờ kiểm duyệt (reviewer/admin)
        .route("/review/queue", get(review::list_queue))
        .route("/review/versions/:id/claim", post(review::claim_version))
        .route(
            "/review/versions/:id/checklist",
            post(review::submit_checklist),
        )
        .route(
            "/review/versions/:id/decision",
            post(review::decide_version),
        )
        // Phase 3: đơn hàng, giao file signed URL, tranh chấp
        .route(
            "/orders",
            get(orders::list_orders).post(orders::create_order),
        )
        .route("/orders/:id", get(orders::get_order))
        .route("/orders/:id/confirm", post(orders::confirm_order))
        .route("/orders/:id/downloads", post(orders::request_download))
        .route("/orders/:id/disputes", post(orders::open_dispute))
        .route("/downloads/:token", get(orders::download_by_token))
        // Admin
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
        .route("/admin/disputes", get(admin::list_disputes))
        .route("/admin/disputes/:id/resolve", post(admin::resolve_dispute))
        .route(
            "/admin/products/:id/takedown",
            post(admin::takedown_product),
        )
        .route("/admin/audit-log", get(admin::audit_log))
        .merge(upload_route)
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
