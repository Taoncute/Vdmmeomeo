#![allow(dead_code)]

use axum::body::Body;
use axum::http::{header, HeaderMap, Method, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use server::config::Config;
use server::state::AppState;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "mat-khau-manh-123";

pub fn test_state(pool: PgPool) -> AppState {
    AppState::new(pool, Config::for_tests()).expect("tạo AppState")
}

pub fn test_app(pool: PgPool) -> (Router, AppState) {
    let state = test_state(pool);
    (server::api::router(state.clone()), state)
}

pub struct Resp {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
}

pub async fn send(app: &Router, req: Request<Body>) -> Resp {
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("đọc body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Resp {
        status,
        headers,
        body,
    }
}

pub fn request(
    method: Method,
    uri: &str,
    body: Option<Value>,
    token: Option<&str>,
    extra: &[(&str, &str)],
) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .expect("request"),
        None => b.body(Body::empty()).expect("request"),
    }
}

pub async fn get(app: &Router, uri: &str, token: Option<&str>) -> Resp {
    send(app, request(Method::GET, uri, None, token, &[])).await
}

pub async fn post(app: &Router, uri: &str, body: Value, token: Option<&str>) -> Resp {
    send(app, request(Method::POST, uri, Some(body), token, &[])).await
}

pub async fn post_with(
    app: &Router,
    uri: &str,
    body: Value,
    token: Option<&str>,
    extra: &[(&str, &str)],
) -> Resp {
    send(app, request(Method::POST, uri, Some(body), token, extra)).await
}

pub async fn patch(app: &Router, uri: &str, body: Value, token: Option<&str>) -> Resp {
    send(app, request(Method::PATCH, uri, Some(body), token, &[])).await
}

/// Lấy giá trị cookie `refresh_token` từ các header Set-Cookie.
pub fn refresh_cookie_from(headers: &HeaderMap) -> Option<String> {
    for v in headers.get_all(header::SET_COOKIE) {
        let s = v.to_str().ok()?;
        if let Some(rest) = s.strip_prefix("refresh_token=") {
            return rest.split(';').next().map(|x| x.to_string());
        }
    }
    None
}

pub struct Session {
    pub access: String,
    pub refresh: String,
    pub user_id: Uuid,
}

pub async fn register(app: &Router, email: &str) -> Session {
    let r = post(
        app,
        "/api/v1/auth/register",
        json!({ "email": email, "password": PASSWORD, "display_name": "Người thử" }),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "đăng ký lỗi: {:?}", r.body);
    Session {
        access: r.body["access_token"].as_str().expect("access").to_string(),
        refresh: refresh_cookie_from(&r.headers).expect("cookie refresh"),
        user_id: Uuid::parse_str(r.body["user"]["id"].as_str().expect("id")).expect("uuid"),
    }
}

pub async fn login(app: &Router, email: &str, password: &str) -> Resp {
    post(
        app,
        "/api/v1/auth/login",
        json!({ "email": email, "password": password }),
        None,
    )
    .await
}

pub async fn refresh_with(app: &Router, refresh: &str) -> Resp {
    let cookie = format!("refresh_token={refresh}");
    send(
        app,
        request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            None,
            &[("x-requested-with", "test"), ("cookie", cookie.as_str())],
        ),
    )
    .await
}

pub async fn grant_role(pool: &PgPool, user_id: Uuid, role: &str) {
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) SELECT $1, id FROM roles WHERE name = $2 \
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await
    .expect("cấp vai trò");
}

/// Đăng ký người dùng mới rồi cấp vai admin.
pub async fn register_admin(app: &Router, pool: &PgPool, email: &str) -> Session {
    let s = register(app, email).await;
    grant_role(pool, s.user_id, "admin").await;
    s
}
