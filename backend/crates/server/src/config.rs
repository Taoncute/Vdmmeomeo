use std::fmt::Display;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub app_env: String,
    pub database_url: String,
    pub bind_addr: String,
    pub jwt_secret: String,
    pub access_ttl_secs: i64,
    pub refresh_ttl_days: i64,
    pub cookie_secure: bool,
    pub cors_origins: Vec<String>,
    pub trust_proxy: bool,
    pub rate_limit_ip_per_min: u32,
    pub rate_limit_auth_per_min: u32,
    pub rate_limit_user_per_min: u32,
    pub argon2_m_kib: u32,
    pub argon2_t: u32,
    pub log_json: bool,
    /// `manual` (mặc định), `payos` hoặc `fake` (chỉ dev/test).
    pub payment_gateway: String,
    pub payos_client_id: String,
    pub payos_api_key: String,
    pub payos_checksum_key: String,
    pub payos_base_url: String,
    pub payos_return_url: String,
    pub payos_cancel_url: String,
    pub manual_bank_name: String,
    pub manual_account_no: String,
    pub manual_account_name: String,
    /// Khóa mã hóa KYC: `id:base64(32 byte)`, nhiều khóa cách nhau bằng dấu phẩy.
    pub kyc_keys: String,
    pub kyc_active_key: String,
    /// Phase 3: lưu trữ tệp (`local` | `s3`) và quét virus ClamAV.
    pub storage_backend: String,
    pub storage_local_dir: String,
    pub storage_download_secret: String,
    pub public_base_url: String,
    pub s3_endpoint: String,
    pub s3_region: String,
    pub s3_bucket: String,
    pub s3_access_key: String,
    pub s3_secret_key: String,
    pub clamd_addr: String,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: FromStr,
    T::Err: Display,
{
    match std::env::var(key) {
        Ok(v) => v
            .parse::<T>()
            .map_err(|e| anyhow::anyhow!("Biến môi trường {key} không hợp lệ: {e}")),
        Err(_) => Ok(default),
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let database_url = std::env::var("DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("Thiếu biến môi trường DATABASE_URL"))?;
        let jwt_secret = std::env::var("JWT_SECRET")
            .map_err(|_| anyhow::anyhow!("Thiếu biến môi trường JWT_SECRET (ít nhất 32 ký tự)"))?;
        if jwt_secret.len() < 32 {
            anyhow::bail!("JWT_SECRET phải dài ít nhất 32 ký tự");
        }
        let cors_origins = env_or("CORS_ORIGINS", "http://localhost:5173")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(Self {
            app_env: env_or("APP_ENV", "development"),
            database_url,
            bind_addr: env_or("BIND_ADDR", "0.0.0.0:8080"),
            jwt_secret,
            access_ttl_secs: env_parse("ACCESS_TTL_SECS", 900)?,
            refresh_ttl_days: env_parse("REFRESH_TTL_DAYS", 30)?,
            cookie_secure: env_parse("COOKIE_SECURE", false)?,
            cors_origins,
            trust_proxy: env_parse("TRUST_PROXY", false)?,
            rate_limit_ip_per_min: env_parse("RATE_LIMIT_IP_PER_MIN", 300)?,
            rate_limit_auth_per_min: env_parse("RATE_LIMIT_AUTH_PER_MIN", 20)?,
            rate_limit_user_per_min: env_parse("RATE_LIMIT_USER_PER_MIN", 600)?,
            argon2_m_kib: env_parse("ARGON2_M_KIB", 19456)?,
            argon2_t: env_parse("ARGON2_T", 2)?,
            log_json: env_parse("LOG_JSON", false)?,
            payment_gateway: env_or("PAYMENT_GATEWAY", "manual"),
            payos_client_id: env_or("PAYOS_CLIENT_ID", ""),
            payos_api_key: env_or("PAYOS_API_KEY", ""),
            payos_checksum_key: env_or("PAYOS_CHECKSUM_KEY", ""),
            payos_base_url: env_or("PAYOS_BASE_URL", "https://api-merchant.payos.vn"),
            payos_return_url: env_or("PAYOS_RETURN_URL", "http://localhost:5173/wallet"),
            payos_cancel_url: env_or("PAYOS_CANCEL_URL", "http://localhost:5173/wallet"),
            manual_bank_name: env_or("MANUAL_BANK_NAME", "Ngân hàng (chưa cấu hình)"),
            manual_account_no: env_or("MANUAL_ACCOUNT_NO", "0000000000"),
            manual_account_name: env_or("MANUAL_ACCOUNT_NAME", "CHU SAN (CHUA CAU HINH)"),
            kyc_keys: env_or("KYC_KEYS", ""),
            kyc_active_key: env_or("KYC_ACTIVE_KEY", ""),
            storage_backend: env_or("STORAGE_BACKEND", "local"),
            storage_local_dir: env_or("STORAGE_LOCAL_DIR", "./data/uploads"),
            storage_download_secret: env_or("STORAGE_DOWNLOAD_SECRET", ""),
            public_base_url: env_or("PUBLIC_BASE_URL", "http://localhost:8080"),
            s3_endpoint: env_or("S3_ENDPOINT", ""),
            s3_region: env_or("S3_REGION", "auto"),
            s3_bucket: env_or("S3_BUCKET", ""),
            s3_access_key: env_or("S3_ACCESS_KEY", ""),
            s3_secret_key: env_or("S3_SECRET_KEY", ""),
            clamd_addr: env_or("CLAMD_ADDR", ""),
        })
    }

    /// Cấu hình cho test: giới hạn tốc độ rất cao, argon2 nhẹ, thư mục lưu tệp tạm riêng từng test.
    pub fn for_tests() -> Self {
        let test_dir = std::env::temp_dir()
            .join(format!("cho-cong-dong-test-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .to_string();
        Self {
            app_env: "test".to_string(),
            database_url: String::new(),
            bind_addr: "127.0.0.1:0".to_string(),
            jwt_secret: "test-secret-test-secret-test-secret-0123".to_string(),
            access_ttl_secs: 900,
            refresh_ttl_days: 30,
            cookie_secure: false,
            cors_origins: vec!["http://localhost:5173".to_string()],
            trust_proxy: false,
            rate_limit_ip_per_min: 1_000_000,
            rate_limit_auth_per_min: 1_000_000,
            rate_limit_user_per_min: 1_000_000,
            argon2_m_kib: 8,
            argon2_t: 1,
            log_json: false,
            payment_gateway: "manual".to_string(),
            payos_client_id: "test-client".to_string(),
            payos_api_key: "test-key".to_string(),
            payos_checksum_key: crate::payments::payos::TEST_CHECKSUM_KEY.to_string(),
            payos_base_url: "http://127.0.0.1:1".to_string(),
            payos_return_url: "http://localhost/return".to_string(),
            payos_cancel_url: "http://localhost/cancel".to_string(),
            manual_bank_name: "Ngân hàng thử".to_string(),
            manual_account_no: "123456789".to_string(),
            manual_account_name: "CHU SAN THU".to_string(),
            kyc_keys: String::new(),
            kyc_active_key: String::new(),
            storage_backend: "local".to_string(),
            storage_local_dir: test_dir,
            storage_download_secret: "test-download-secret-0123456789abcdef".to_string(),
            public_base_url: "http://localhost:8080".to_string(),
            s3_endpoint: String::new(),
            s3_region: "auto".to_string(),
            s3_bucket: String::new(),
            s3_access_key: String::new(),
            s3_secret_key: String::new(),
            clamd_addr: String::new(),
        }
    }
}
