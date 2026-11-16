//! Mật khẩu (argon2id), JWT truy cập, refresh token, vai trò và các extractor xác thực.

use std::convert::Infallible;
use std::net::SocketAddr;

use anyhow::anyhow;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::async_trait;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{header, Extensions, HeaderMap};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

// ---------- Vai trò ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Buyer,
    Seller,
    Reviewer,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Buyer => "buyer",
            Role::Seller => "seller",
            Role::Reviewer => "reviewer",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "buyer" => Some(Role::Buyer),
            "seller" => Some(Role::Seller),
            "reviewer" => Some(Role::Reviewer),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

/// Cấp vai trò cho người dùng (idempotent).
pub async fn grant_role(
    conn: &mut PgConnection,
    user_id: Uuid,
    role: Role,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) \
         SELECT $1, id FROM roles WHERE name = $2 \
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(role.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

// ---------- Mật khẩu ----------

fn argon2_with(m_kib: u32, t: u32) -> AppResult<Argon2<'static>> {
    let params = Params::new(m_kib, t, 1, None)
        .map_err(|e| AppError::Internal(anyhow!("tham số argon2 không hợp lệ: {e}")))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn hash_password(m_kib: u32, t: u32, password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = argon2_with(m_kib, t)?
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(anyhow!("không băm được mật khẩu: {e}")))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

pub async fn hash_password_async(m_kib: u32, t: u32, password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || hash_password(m_kib, t, &password))
        .await
        .map_err(|e| AppError::Internal(anyhow!("tác vụ băm mật khẩu lỗi: {e}")))?
}

pub async fn verify_password_async(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&password, &hash))
        .await
        .unwrap_or(false)
}

// ---------- JWT truy cập ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: i64,
    pub iat: i64,
}

pub fn issue_access_token(cfg: &Config, user_id: Uuid) -> AppResult<String> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now,
        exp: now + cfg.access_ttl_secs,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(cfg.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow!("không ký được JWT: {e}")))
}

pub fn decode_access_token(cfg: &Config, token: &str) -> AppResult<Claims> {
    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.leeway = 5;
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(cfg.jwt_secret.as_bytes()),
        &validation,
    )
    .map(|data| data.claims)
    .map_err(|_| {
        AppError::unauthorized("Phiên đăng nhập hết hạn hoặc không hợp lệ. Hãy đăng nhập lại.")
    })
}

// ---------- Refresh token ----------

/// Refresh token ngẫu nhiên 256 bit, mã hóa base64url. Chỉ lưu băm SHA-256 trong DB.
pub fn generate_refresh_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn hash_refresh_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

// ---------- Header, cookie, IP ----------

pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ")?;
    Some(token.trim().to_string())
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    for header_value in headers.get_all(header::COOKIE) {
        if let Ok(s) = header_value.to_str() {
            for part in s.split(';') {
                if let Some((k, v)) = part.trim().split_once('=') {
                    if k == name {
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    None
}

/// IP client. Chỉ tin header proxy khi `trust_proxy` bật (chạy sau Fly/Cloudflare).
pub fn resolve_ip(headers: &HeaderMap, ext: &Extensions, trust_proxy: bool) -> String {
    if trust_proxy {
        for name in ["fly-client-ip", "x-forwarded-for"] {
            if let Some(v) = headers.get(name).and_then(|v| v.to_str().ok()) {
                if let Some(first) = v.split(',').next() {
                    let first = first.trim();
                    if !first.is_empty() {
                        return first.to_string();
                    }
                }
            }
        }
    }
    ext.get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub struct ClientIp(pub String);

#[async_trait]
impl FromRequestParts<AppState> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Infallible> {
        Ok(ClientIp(resolve_ip(
            &parts.headers,
            &parts.extensions,
            state.config.trust_proxy,
        )))
    }
}

// ---------- Người dùng đã xác thực ----------

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub roles: Vec<Role>,
}

impl AuthUser {
    pub fn has_role(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }

    pub fn require_role(&self, role: Role) -> AppResult<()> {
        if self.has_role(role) {
            Ok(())
        } else {
            Err(AppError::forbidden(
                "Bạn không có quyền thực hiện thao tác này.",
            ))
        }
    }
}

/// Tải người dùng và vai trò từ DB. Trả lỗi 403 nếu tài khoản bị khóa.
pub async fn load_auth_user(db: &PgPool, id: Uuid) -> AppResult<Option<AuthUser>> {
    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, String)>(
        "SELECT id, email, display_name, avatar_url, status FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    let Some((id, email, display_name, avatar_url, status)) = row else {
        return Ok(None);
    };
    if status != "active" {
        return Err(AppError::forbidden(
            "Tài khoản đã bị khóa. Hãy liên hệ quản trị viên.",
        ));
    }
    let names = sqlx::query_scalar::<_, String>(
        "SELECT r.name FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
         WHERE ur.user_id = $1 ORDER BY r.id",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    let roles = names.iter().filter_map(|n| Role::parse(n)).collect();
    Ok(Some(AuthUser {
        id,
        email,
        display_name,
        avatar_url,
        roles,
    }))
}

#[async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let token = bearer_token(&parts.headers)
            .ok_or_else(|| AppError::unauthorized("Bạn chưa đăng nhập."))?;
        let claims = decode_access_token(&state.config, &token)?;
        let user_id = Uuid::parse_str(&claims.sub)
            .map_err(|_| AppError::unauthorized("Phiên đăng nhập không hợp lệ."))?;
        load_auth_user(&state.db, user_id)
            .await?
            .ok_or_else(|| AppError::unauthorized("Tài khoản không tồn tại."))
    }
}

/// Extractor chỉ cho quản trị viên.
pub struct AdminUser(pub AuthUser);

#[async_trait]
impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        let user = AuthUser::from_request_parts(parts, state).await?;
        user.require_role(Role::Admin)?;
        Ok(AdminUser(user))
    }
}
