use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use crate::config::Config;
use crate::ratelimit::RateLimiter;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub config: Arc<Config>,
    pub ip_limiter: Arc<RateLimiter>,
    pub auth_limiter: Arc<RateLimiter>,
    pub user_limiter: Arc<RateLimiter>,
    /// Băm giả để đăng nhập email không tồn tại tốn thời gian tương đương email có thật.
    pub dummy_hash: Arc<String>,
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> anyhow::Result<Self> {
        let window = Duration::from_secs(60);
        let dummy_hash = crate::auth::hash_password(
            config.argon2_m_kib,
            config.argon2_t,
            "mat-khau-gia-de-chong-do-thoi-gian",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Self {
            db,
            ip_limiter: Arc::new(RateLimiter::new(config.rate_limit_ip_per_min, window)),
            auth_limiter: Arc::new(RateLimiter::new(config.rate_limit_auth_per_min, window)),
            user_limiter: Arc::new(RateLimiter::new(config.rate_limit_user_per_min, window)),
            dummy_hash: Arc::new(dummy_hash),
            config: Arc::new(config),
        })
    }
}
