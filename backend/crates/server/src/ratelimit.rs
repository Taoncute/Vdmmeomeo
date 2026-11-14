use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::auth::{bearer_token, decode_access_token, resolve_ip};
use crate::error::AppError;
use crate::state::AppState;

/// Giới hạn tốc độ theo cửa sổ cố định, lưu trong bộ nhớ của tiến trình.
pub struct RateLimiter {
    limit: u32,
    window: Duration,
    buckets: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self {
            limit,
            window,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, key: &str) -> bool {
        self.check_at(key, Instant::now())
    }

    pub fn check_at(&self, key: &str, now: Instant) -> bool {
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 50_000 {
            let window = self.window;
            map.retain(|_, v| now.duration_since(v.0) < window);
        }
        let entry = map.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.limit {
            return false;
        }
        entry.1 += 1;
        true
    }
}

pub async fn rate_limit_mw(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let ip = resolve_ip(req.headers(), req.extensions(), state.config.trust_proxy);
    let is_auth = req.uri().path().starts_with("/api/v1/auth/");
    let allowed = if is_auth {
        state.auth_limiter.check(&ip)
    } else {
        state.ip_limiter.check(&ip)
    };
    if !allowed {
        return AppError::too_many_requests().into_response();
    }
    if let Some(token) = bearer_token(req.headers()) {
        if let Ok(claims) = decode_access_token(&state.config, &token) {
            if !state.user_limiter.check(&claims.sub) {
                return AppError::too_many_requests().into_response();
            }
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gioi_han_theo_cua_so() {
        let rl = RateLimiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(rl.check_at("a", t0));
        assert!(rl.check_at("a", t0));
        assert!(rl.check_at("a", t0));
        assert!(!rl.check_at("a", t0));
        // Khóa khác không bị ảnh hưởng.
        assert!(rl.check_at("b", t0));
        // Sang cửa sổ mới thì được lại.
        assert!(rl.check_at("a", t0 + Duration::from_secs(61)));
    }
}
