use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use crate::config::Config;
use crate::crypto::KycCrypto;
use crate::payments::fake::FakeGateway;
use crate::payments::manual::ManualGateway;
use crate::payments::payos::PayOsGateway;
use crate::payments::PaymentGateway;
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
    pub gateway: Arc<dyn PaymentGateway>,
    pub crypto: Arc<KycCrypto>,
}

fn build_gateway(config: &Config) -> anyhow::Result<Arc<dyn PaymentGateway>> {
    match config.payment_gateway.as_str() {
        "manual" => Ok(Arc::new(ManualGateway::from_config(config))),
        "payos" => Ok(Arc::new(PayOsGateway::from_config(config)?)),
        "fake" => {
            if config.app_env == "production" {
                anyhow::bail!("PAYMENT_GATEWAY=fake không được dùng ở production");
            }
            Ok(Arc::new(FakeGateway::new("fake-secret")))
        }
        other => anyhow::bail!("PAYMENT_GATEWAY không hợp lệ: {other} (manual | payos | fake)"),
    }
}

impl AppState {
    pub fn new(db: PgPool, config: Config) -> anyhow::Result<Self> {
        let gateway = build_gateway(&config)?;
        Self::with_gateway(db, config, gateway)
    }

    /// Dùng cổng thanh toán chỉ định (test thay adapter).
    pub fn with_gateway(
        db: PgPool,
        config: Config,
        gateway: Arc<dyn PaymentGateway>,
    ) -> anyhow::Result<Self> {
        let window = Duration::from_secs(60);
        let dummy_hash = crate::auth::hash_password(
            config.argon2_m_kib,
            config.argon2_t,
            "mat-khau-gia-de-chong-do-thoi-gian",
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let crypto = Arc::new(KycCrypto::from_config(&config)?);
        Ok(Self {
            db,
            ip_limiter: Arc::new(RateLimiter::new(config.rate_limit_ip_per_min, window)),
            auth_limiter: Arc::new(RateLimiter::new(config.rate_limit_auth_per_min, window)),
            user_limiter: Arc::new(RateLimiter::new(config.rate_limit_user_per_min, window)),
            dummy_hash: Arc::new(dummy_hash),
            gateway,
            crypto,
            config: Arc::new(config),
        })
    }
}
