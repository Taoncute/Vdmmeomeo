use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;
use validator::Validate;

use crate::auth::{
    cookie_value, generate_refresh_token, grant_role, hash_password_async, hash_refresh_token,
    issue_access_token, load_auth_user, verify_password_async, AuthUser, Role,
};
use crate::config::Config;
use crate::error::{is_unique_violation, AppError, AppResult};
use crate::state::AppState;

pub const REFRESH_COOKIE: &str = "refresh_token";
const COOKIE_PATH: &str = "/api/v1/auth";

#[derive(Debug, Deserialize, Validate)]
pub struct RegisterRequest {
    #[validate(email)]
    #[validate(length(max = 254))]
    pub email: String,
    #[validate(length(min = 8, max = 128))]
    pub password: String,
    #[validate(length(min = 1, max = 60))]
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct UserDto {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub roles: Vec<&'static str>,
}

impl From<&AuthUser> for UserDto {
    fn from(u: &AuthUser) -> Self {
        UserDto {
            id: u.id,
            email: u.email.clone(),
            display_name: u.display_name.clone(),
            avatar_url: u.avatar_url.clone(),
            roles: u.roles.iter().map(|r| r.as_str()).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct AuthResponse {
    access_token: String,
    token_type: &'static str,
    expires_in: i64,
    user: UserDto,
}

pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn refresh_cookie(cfg: &Config, token: &str) -> String {
    format!(
        "{REFRESH_COOKIE}={token}; Path={COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age={}{}",
        cfg.refresh_ttl_days * 86_400,
        if cfg.cookie_secure { "; Secure" } else { "" }
    )
}

fn clear_cookie(cfg: &Config) -> String {
    format!(
        "{REFRESH_COOKIE}=; Path={COOKIE_PATH}; HttpOnly; SameSite=Lax; Max-Age=0{}",
        if cfg.cookie_secure { "; Secure" } else { "" }
    )
}

/// Tạo refresh token mới (cùng một "họ" token) và access token.
async fn create_session(
    state: &AppState,
    conn: &mut PgConnection,
    user_id: Uuid,
    family_id: Uuid,
) -> AppResult<(String, String)> {
    let refresh = generate_refresh_token();
    let expires_at = Utc::now() + Duration::days(state.config.refresh_ttl_days);
    sqlx::query(
        "INSERT INTO refresh_tokens (user_id, family_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(user_id)
    .bind(family_id)
    .bind(hash_refresh_token(&refresh))
    .bind(expires_at)
    .execute(conn)
    .await?;
    let access = issue_access_token(&state.config, user_id)?;
    Ok((access, refresh))
}

async fn session_response(
    state: &AppState,
    status: StatusCode,
    user_id: Uuid,
    access: String,
    refresh: String,
) -> AppResult<Response> {
    let user = load_auth_user(&state.db, user_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("Tài khoản không tồn tại."))?;
    let body = AuthResponse {
        access_token: access,
        token_type: "Bearer",
        expires_in: state.config.access_ttl_secs,
        user: UserDto::from(&user),
    };
    Ok((
        status,
        [(header::SET_COOKIE, refresh_cookie(&state.config, &refresh))],
        Json(body),
    )
        .into_response())
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> AppResult<Response> {
    let req = RegisterRequest {
        email: normalize_email(&req.email),
        password: req.password,
        display_name: req.display_name.trim().to_string(),
    };
    req.validate()?;

    let hash = hash_password_async(
        state.config.argon2_m_kib,
        state.config.argon2_t,
        req.password.clone(),
    )
    .await?;

    let mut tx = state.db.begin().await?;
    let inserted = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO users (email, password_hash, display_name) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(&req.email)
    .bind(hash)
    .bind(&req.display_name)
    .fetch_one(&mut *tx)
    .await;
    let user_id = match inserted {
        Ok(id) => id,
        Err(e) if is_unique_violation(&e) => {
            return Err(AppError::conflict(
                "email_taken",
                "Email này đã được đăng ký. Hãy đăng nhập hoặc dùng email khác.",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    grant_role(&mut tx, user_id, Role::Buyer).await?;
    let (access, refresh) = create_session(&state, &mut tx, user_id, Uuid::new_v4()).await?;
    tx.commit().await?;
    session_response(&state, StatusCode::CREATED, user_id, access, refresh).await
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Response> {
    let email = normalize_email(&req.email);
    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, password_hash, status FROM users WHERE email = $1",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await?;
    let invalid = || {
        AppError::api(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "Email hoặc mật khẩu không đúng.",
        )
    };
    let Some((user_id, hash, status)) = row else {
        // Vẫn băm một lần để thời gian phản hồi không lộ email nào đã đăng ký.
        let _ = verify_password_async(req.password, state.dummy_hash.to_string()).await;
        return Err(invalid());
    };
    if !verify_password_async(req.password, hash).await {
        return Err(invalid());
    }
    if status != "active" {
        return Err(AppError::api(
            StatusCode::FORBIDDEN,
            "account_banned",
            "Tài khoản đã bị khóa. Hãy liên hệ quản trị viên.",
        ));
    }
    let mut tx = state.db.begin().await?;
    let (access, refresh) = create_session(&state, &mut tx, user_id, Uuid::new_v4()).await?;
    tx.commit().await?;
    session_response(&state, StatusCode::OK, user_id, access, refresh).await
}

/// Xoay vòng refresh token. Dùng lại token đã bị thu hồi nghĩa là có thể đã bị đánh cắp,
/// nên thu hồi cả họ token (người dùng phải đăng nhập lại).
pub async fn refresh(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if !headers.contains_key("x-requested-with") {
        return Err(AppError::forbidden(
            "Thiếu header X-Requested-With (chống CSRF) cho /auth/refresh.",
        ));
    }
    let token = cookie_value(&headers, REFRESH_COOKIE)
        .ok_or_else(|| AppError::unauthorized("Thiếu refresh token. Hãy đăng nhập lại."))?;
    let hash = hash_refresh_token(&token);

    let mut tx = state.db.begin().await?;
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            Uuid,
            chrono::DateTime<Utc>,
            Option<chrono::DateTime<Utc>>,
        ),
    >(
        "SELECT id, user_id, family_id, expires_at, revoked_at FROM refresh_tokens \
         WHERE token_hash = $1 FOR UPDATE",
    )
    .bind(hash)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((id, user_id, family_id, expires_at, revoked_at)) = row else {
        return Err(AppError::unauthorized(
            "Refresh token không hợp lệ. Hãy đăng nhập lại.",
        ));
    };
    if revoked_at.is_some() {
        sqlx::query(
            "UPDATE refresh_tokens SET revoked_at = now() \
             WHERE family_id = $1 AND revoked_at IS NULL",
        )
        .bind(family_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        tracing::warn!(%user_id, %family_id, "phát hiện dùng lại refresh token, đã thu hồi cả họ token");
        return Err(AppError::api(
            StatusCode::UNAUTHORIZED,
            "refresh_reused",
            "Phiên đăng nhập không còn an toàn nên đã bị đóng. Hãy đăng nhập lại.",
        ));
    }
    if expires_at <= Utc::now() {
        return Err(AppError::unauthorized(
            "Phiên đăng nhập đã hết hạn. Hãy đăng nhập lại.",
        ));
    }
    let status = sqlx::query_scalar::<_, String>("SELECT status FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    if status != "active" {
        return Err(AppError::forbidden("Tài khoản đã bị khóa."));
    }
    sqlx::query("UPDATE refresh_tokens SET revoked_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let (access, new_refresh) = create_session(&state, &mut tx, user_id, family_id).await?;
    tx.commit().await?;
    session_response(&state, StatusCode::OK, user_id, access, new_refresh).await
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> AppResult<Response> {
    if let Some(token) = cookie_value(&headers, REFRESH_COOKIE) {
        sqlx::query(
            "UPDATE refresh_tokens SET revoked_at = now() \
             WHERE revoked_at IS NULL AND family_id = \
             (SELECT family_id FROM refresh_tokens WHERE token_hash = $1)",
        )
        .bind(hash_refresh_token(&token))
        .execute(&state.db)
        .await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, clear_cookie(&state.config))],
    )
        .into_response())
}
