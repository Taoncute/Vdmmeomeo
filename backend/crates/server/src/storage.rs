//! Lưu trữ tệp sản phẩm trên bucket private (S3/R2) hoặc thư mục cục bộ (dev/test).
//! Không bao giờ lộ đường dẫn gốc; người mua chỉ nhận signed URL có hạn ngắn (mặc định 120 giây).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{AppError, AppResult};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize)]
pub struct SignedDownload {
    pub url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ResolvedToken {
    pub object_key: String,
    pub filename: String,
    pub content_type: String,
}

#[async_trait]
pub trait FileStorage: Send + Sync {
    async fn put(&self, object_key: &str, content_type: &str, data: &[u8]) -> AppResult<()>;
    async fn get(&self, object_key: &str) -> AppResult<Vec<u8>>;
    async fn delete(&self, object_key: &str) -> AppResult<()>;
    async fn presign_get(
        &self,
        object_key: &str,
        filename: &str,
        content_type: &str,
        ttl_secs: u64,
    ) -> AppResult<SignedDownload>;

    /// Giải mã và xác thực token tải cục bộ (chỉ dùng cho adapter `local`).
    fn verify_local_token(&self, _token: &str) -> AppResult<ResolvedToken> {
        Err(AppError::not_found(
            "Đường dẫn tải trực tiếp không bật cho bộ lưu trữ này.",
        ))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TokenPayload {
    k: String,
    f: String,
    c: String,
    exp: i64,
}

fn sign_bytes(secret: &[u8], data: &[u8]) -> AppResult<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|e| AppError::internal(format!("khóa HMAC không hợp lệ: {e}")))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn verify_sig(secret: &[u8], data: &[u8], sig: &[u8]) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(data);
    mac.verify_slice(sig).is_ok()
}

/// Kiểm tra object_key chỉ chứa ký tự an toàn, không cho phép leo thư mục (`..`).
fn sanitize_key(object_key: &str) -> AppResult<&str> {
    if object_key.is_empty()
        || object_key.starts_with('/')
        || object_key.contains("..")
        || object_key.contains('\\')
    {
        return Err(AppError::bad_request(
            "invalid_object_key",
            "Khóa tệp lưu trữ không hợp lệ.",
        ));
    }
    Ok(object_key)
}

/// Lưu tệp trên đĩa cục bộ và phát hành URL ký bằng HMAC-SHA256.
pub struct LocalStorage {
    base_dir: PathBuf,
    public_base_url: String,
    signing_secret: Vec<u8>,
}

impl LocalStorage {
    pub fn new(base_dir: PathBuf, public_base_url: String, signing_secret: String) -> Self {
        Self {
            base_dir,
            public_base_url: public_base_url.trim_end_matches('/').to_string(),
            signing_secret: signing_secret.into_bytes(),
        }
    }

    fn path_for(&self, object_key: &str) -> AppResult<PathBuf> {
        let key = sanitize_key(object_key)?;
        Ok(self.base_dir.join(key))
    }

    /// Tạo token tải đã ký (dùng trong test để kiểm tra hạn hoặc giả mạo).
    pub fn sign_token_with_exp(
        &self,
        object_key: &str,
        filename: &str,
        content_type: &str,
        exp: i64,
    ) -> AppResult<String> {
        let payload = TokenPayload {
            k: object_key.to_string(),
            f: filename.to_string(),
            c: content_type.to_string(),
            exp,
        };
        let encoded =
            URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&payload).map_err(|e| {
                    AppError::internal(format!("không mã hóa được token tải: {e}"))
                })?);
        let sig = sign_bytes(&self.signing_secret, encoded.as_bytes())?;
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig);
        Ok(format!("{encoded}.{sig_b64}"))
    }
}

#[async_trait]
impl FileStorage for LocalStorage {
    async fn put(&self, object_key: &str, _content_type: &str, data: &[u8]) -> AppResult<()> {
        let path = self.path_for(object_key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::internal(format!("không tạo được thư mục lưu tệp: {e}")))?;
        }
        tokio::fs::write(&path, data)
            .await
            .map_err(|e| AppError::internal(format!("không ghi được tệp: {e}")))?;
        Ok(())
    }

    async fn get(&self, object_key: &str) -> AppResult<Vec<u8>> {
        let path = self.path_for(object_key)?;
        tokio::fs::read(&path)
            .await
            .map_err(|_| AppError::not_found("Không tìm thấy tệp trong kho lưu trữ."))
    }

    async fn delete(&self, object_key: &str) -> AppResult<()> {
        let path = self.path_for(object_key)?;
        if Path::new(&path).exists() {
            let _ = tokio::fs::remove_file(&path).await;
        }
        Ok(())
    }

    async fn presign_get(
        &self,
        object_key: &str,
        filename: &str,
        content_type: &str,
        ttl_secs: u64,
    ) -> AppResult<SignedDownload> {
        sanitize_key(object_key)?;
        let expires_at = Utc::now() + Duration::seconds(ttl_secs as i64);
        let token =
            self.sign_token_with_exp(object_key, filename, content_type, expires_at.timestamp())?;
        Ok(SignedDownload {
            url: format!("{}/api/v1/downloads/{token}", self.public_base_url),
            expires_at,
        })
    }

    fn verify_local_token(&self, token: &str) -> AppResult<ResolvedToken> {
        let Some((encoded, sig_b64)) = token.split_once('.') else {
            return Err(AppError::unauthorized("Liên kết tải không hợp lệ."));
        };
        let sig = URL_SAFE_NO_PAD
            .decode(sig_b64)
            .map_err(|_| AppError::unauthorized("Liên kết tải không hợp lệ."))?;
        if !verify_sig(&self.signing_secret, encoded.as_bytes(), &sig) {
            return Err(AppError::unauthorized("Chữ ký liên kết tải không hợp lệ."));
        }
        let raw = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| AppError::unauthorized("Liên kết tải không hợp lệ."))?;
        let payload: TokenPayload = serde_json::from_slice(&raw)
            .map_err(|_| AppError::unauthorized("Liên kết tải không hợp lệ."))?;
        if Utc::now().timestamp() > payload.exp {
            return Err(AppError::unauthorized(
                "Liên kết tải đã hết hạn. Hãy yêu cầu liên kết mới.",
            ));
        }
        sanitize_key(&payload.k)?;
        Ok(ResolvedToken {
            object_key: payload.k,
            filename: payload.f,
            content_type: payload.c,
        })
    }
}

/// Adapter S3-compatible (Cloudflare R2 / MinIO / S3) dùng AWS SigV4 thuần Rust.
pub struct S3Storage {
    client: reqwest::Client,
    endpoint: String,
    host: String,
    region: String,
    bucket: String,
    access_key: String,
    secret_key: String,
}

impl S3Storage {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        if cfg.s3_endpoint.is_empty()
            || cfg.s3_bucket.is_empty()
            || cfg.s3_access_key.is_empty()
            || cfg.s3_secret_key.is_empty()
        {
            anyhow::bail!(
                "STORAGE_BACKEND=s3 yêu cầu S3_ENDPOINT, S3_BUCKET, S3_ACCESS_KEY, S3_SECRET_KEY"
            );
        }
        let endpoint = cfg.s3_endpoint.trim_end_matches('/').to_string();
        let host = endpoint
            .strip_prefix("https://")
            .or_else(|| endpoint.strip_prefix("http://"))
            .unwrap_or(&endpoint)
            .split('/')
            .next()
            .unwrap_or("")
            .to_string();
        if host.is_empty() {
            anyhow::bail!("S3_ENDPOINT không hợp lệ");
        }
        Ok(Self {
            client: reqwest::Client::new(),
            endpoint,
            host,
            region: cfg.s3_region.clone(),
            bucket: cfg.s3_bucket.clone(),
            access_key: cfg.s3_access_key.clone(),
            secret_key: cfg.s3_secret_key.clone(),
        })
    }

    fn signing_key(&self, date_stamp: &str) -> AppResult<Vec<u8>> {
        let k_date = sign_bytes(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date_stamp.as_bytes(),
        )?;
        let k_region = sign_bytes(&k_date, self.region.as_bytes())?;
        let k_service = sign_bytes(&k_region, b"s3")?;
        sign_bytes(&k_service, b"aws4_request")
    }

    fn canonical_uri(&self, object_key: &str) -> String {
        let mut out = format!("/{}", pct_encode(&self.bucket));
        for seg in object_key.split('/') {
            out.push('/');
            out.push_str(&pct_encode(seg));
        }
        out
    }

    fn auth_headers(
        &self,
        method: &str,
        object_key: &str,
        payload_hash: &str,
        now: DateTime<Utc>,
    ) -> AppResult<(String, String, String)> {
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let canonical_uri = self.canonical_uri(object_key);
        let canonical_headers = format!(
            "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            self.host, payload_hash, amz_date
        );
        let signed_headers = "host;x-amz-content-sha256;x-amz-date";
        let canonical_req = format!(
            "{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical_req.as_bytes()))
        );
        let key = self.signing_key(&date_stamp)?;
        let sig = hex::encode(sign_bytes(&key, string_to_sign.as_bytes())?);
        let auth = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={sig}",
            self.access_key
        );
        Ok((
            amz_date,
            auth,
            format!("{}{}", self.endpoint, canonical_uri),
        ))
    }
}

fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[async_trait]
impl FileStorage for S3Storage {
    async fn put(&self, object_key: &str, content_type: &str, data: &[u8]) -> AppResult<()> {
        sanitize_key(object_key)?;
        let payload_hash = hex::encode(Sha256::digest(data));
        let (amz_date, auth, url) =
            self.auth_headers("PUT", object_key, &payload_hash, Utc::now())?;
        let resp = self
            .client
            .put(&url)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .header("authorization", auth)
            .header("content-type", content_type)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| AppError::internal(format!("lỗi tải tệp lên S3/R2: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::internal(format!(
                "S3/R2 từ chối PUT: {}",
                resp.status()
            )));
        }
        Ok(())
    }

    async fn get(&self, object_key: &str) -> AppResult<Vec<u8>> {
        sanitize_key(object_key)?;
        let payload_hash = hex::encode(Sha256::digest(b""));
        let (amz_date, auth, url) =
            self.auth_headers("GET", object_key, &payload_hash, Utc::now())?;
        let resp = self
            .client
            .get(&url)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .header("authorization", auth)
            .send()
            .await
            .map_err(|e| AppError::internal(format!("lỗi đọc tệp từ S3/R2: {e}")))?;
        if !resp.status().is_success() {
            return Err(AppError::not_found("Không tìm thấy tệp trên kho lưu trữ."));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| AppError::internal(format!("lỗi đọc nội dung tệp: {e}")))
    }

    async fn delete(&self, object_key: &str) -> AppResult<()> {
        sanitize_key(object_key)?;
        let payload_hash = hex::encode(Sha256::digest(b""));
        let (amz_date, auth, url) =
            self.auth_headers("DELETE", object_key, &payload_hash, Utc::now())?;
        let _ = self
            .client
            .delete(&url)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .header("authorization", auth)
            .send()
            .await;
        Ok(())
    }

    async fn presign_get(
        &self,
        object_key: &str,
        filename: &str,
        _content_type: &str,
        ttl_secs: u64,
    ) -> AppResult<SignedDownload> {
        sanitize_key(object_key)?;
        let now = Utc::now();
        let expires_at = now + Duration::seconds(ttl_secs as i64);
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();
        let scope = format!("{date_stamp}/{}/s3/aws4_request", self.region);
        let credential = format!("{}/{scope}", self.access_key);
        let disposition = format!(
            "attachment; filename=\"{}\"",
            filename.replace(['"', '\\', '\r', '\n'], "_")
        );
        let mut params = [
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", credential),
            ("X-Amz-Date", amz_date.clone()),
            ("X-Amz-Expires", ttl_secs.to_string()),
            ("X-Amz-SignedHeaders", "host".to_string()),
            ("response-content-disposition", disposition),
        ];
        params.sort_by(|a, b| a.0.cmp(b.0));
        let canonical_query = params
            .iter()
            .map(|(k, v)| format!("{}={}", pct_encode(k), pct_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        let canonical_uri = self.canonical_uri(object_key);
        let canonical_req = format!(
            "GET\n{canonical_uri}\n{canonical_query}\nhost:{}\n\nhost\nUNSIGNED-PAYLOAD",
            self.host
        );
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex::encode(Sha256::digest(canonical_req.as_bytes()))
        );
        let key = self.signing_key(&date_stamp)?;
        let sig = hex::encode(sign_bytes(&key, string_to_sign.as_bytes())?);
        let url = format!(
            "{}{canonical_uri}?{canonical_query}&X-Amz-Signature={sig}",
            self.endpoint
        );
        Ok(SignedDownload { url, expires_at })
    }
}

pub fn build_storage(cfg: &Config) -> anyhow::Result<std::sync::Arc<dyn FileStorage>> {
    match cfg.storage_backend.as_str() {
        "local" => {
            let secret = if cfg.storage_download_secret.is_empty() {
                cfg.jwt_secret.clone()
            } else {
                cfg.storage_download_secret.clone()
            };
            Ok(std::sync::Arc::new(LocalStorage::new(
                PathBuf::from(&cfg.storage_local_dir),
                cfg.public_base_url.clone(),
                secret,
            )))
        }
        "s3" => Ok(std::sync::Arc::new(S3Storage::from_config(cfg)?)),
        other => anyhow::bail!("STORAGE_BACKEND không hợp lệ: {other} (local | s3)"),
    }
}
