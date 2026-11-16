//! Mã hóa thông tin ngân hàng ở tầng ứng dụng: AES-256-GCM, kèm `key_id` để xoay khóa.
//! Dữ liệu lưu: 12 byte nonce ngẫu nhiên + ciphertext (có thẻ xác thực).

use std::collections::HashMap;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::anyhow;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rand::RngCore;

use crate::config::Config;
use crate::error::{AppError, AppResult};

pub struct KycCrypto {
    active_id: String,
    keys: HashMap<String, [u8; 32]>,
}

impl KycCrypto {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        let mut keys = HashMap::new();
        let mut first: Option<String> = None;
        for part in cfg
            .kyc_keys
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let (id, b64) = part
                .split_once(':')
                .ok_or_else(|| anyhow!("KYC_KEYS phải có dạng id:base64"))?;
            let bytes = STANDARD
                .decode(b64)
                .map_err(|e| anyhow!("khóa KYC {id} không phải base64: {e}"))?;
            let key: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow!("khóa KYC {id} phải dài đúng 32 byte"))?;
            if first.is_none() {
                first = Some(id.to_string());
            }
            keys.insert(id.to_string(), key);
        }
        if keys.is_empty() {
            if cfg.app_env == "production" {
                anyhow::bail!("Thiếu KYC_KEYS ở môi trường production");
            }
            tracing::warn!("KYC_KEYS chưa đặt: dùng khóa dev cố định, KHÔNG dùng cho dữ liệu thật");
            return Ok(Self::for_tests());
        }
        let active_id = if cfg.kyc_active_key.is_empty() {
            first.unwrap_or_default()
        } else {
            cfg.kyc_active_key.clone()
        };
        if !keys.contains_key(&active_id) {
            anyhow::bail!("KYC_ACTIVE_KEY={active_id} không có trong KYC_KEYS");
        }
        Ok(Self { active_id, keys })
    }

    /// Khóa cố định cho dev/test.
    pub fn for_tests() -> Self {
        let mut keys = HashMap::new();
        keys.insert("dev".to_string(), [7u8; 32]);
        Self {
            active_id: "dev".to_string(),
            keys,
        }
    }

    /// Trả về (dữ liệu đã mã hóa, key_id).
    pub fn encrypt(&self, plaintext: &str) -> AppResult<(Vec<u8>, String)> {
        let key = self
            .keys
            .get(&self.active_id)
            .ok_or_else(|| AppError::internal("không có khóa KYC đang dùng"))?;
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| AppError::internal("khóa KYC không hợp lệ"))?;
        let mut nonce_bytes = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
            .map_err(|_| AppError::internal("không mã hóa được dữ liệu KYC"))?;
        let mut out = nonce_bytes.to_vec();
        out.extend_from_slice(&ciphertext);
        Ok((out, self.active_id.clone()))
    }

    pub fn decrypt(&self, key_id: &str, data: &[u8]) -> AppResult<String> {
        let key = self
            .keys
            .get(key_id)
            .ok_or_else(|| AppError::internal(format!("không có khóa KYC {key_id}")))?;
        if data.len() < 13 {
            return Err(AppError::internal("dữ liệu KYC mã hóa bị hỏng"));
        }
        let (nonce_bytes, ciphertext) = data.split_at(12);
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| AppError::internal("khóa KYC không hợp lệ"))?;
        let plain = cipher
            .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
            .map_err(|_| AppError::internal("không giải mã được dữ liệu KYC"))?;
        String::from_utf8(plain).map_err(|_| AppError::internal("dữ liệu KYC không phải UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ma_hoa_roi_giai_ma() {
        let c = KycCrypto::for_tests();
        let (enc, key_id) = c.encrypt("0123456789").unwrap();
        assert_eq!(key_id, "dev");
        assert!(!enc.windows(10).any(|w| w == b"0123456789"));
        assert_eq!(c.decrypt(&key_id, &enc).unwrap(), "0123456789");
        // Nonce ngẫu nhiên: hai lần mã hóa cho kết quả khác nhau.
        let (enc2, _) = c.encrypt("0123456789").unwrap();
        assert_ne!(enc, enc2);
        // Dữ liệu bị sửa thì giải mã thất bại (GCM có xác thực).
        let mut tampered = enc.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(c.decrypt("dev", &tampered).is_err());
        assert!(c.decrypt("khong-co", &enc).is_err());
    }

    #[test]
    fn doc_khoa_tu_cau_hinh_va_xoay_khoa() {
        let mut cfg = Config::for_tests();
        let k1 = STANDARD.encode([1u8; 32]);
        let k2 = STANDARD.encode([2u8; 32]);
        cfg.kyc_keys = format!("k1:{k1},k2:{k2}");
        cfg.kyc_active_key = "k1".to_string();
        let old = KycCrypto::from_config(&cfg).unwrap();
        let (enc, id) = old.encrypt("so-tai-khoan").unwrap();
        assert_eq!(id, "k1");
        // Đổi khóa đang dùng sang k2: dữ liệu cũ (k1) vẫn giải mã được.
        cfg.kyc_active_key = "k2".to_string();
        let rotated = KycCrypto::from_config(&cfg).unwrap();
        assert_eq!(rotated.decrypt("k1", &enc).unwrap(), "so-tai-khoan");
        assert_eq!(rotated.encrypt("x").unwrap().1, "k2");
        // Khóa sai độ dài bị từ chối.
        cfg.kyc_keys = format!("k1:{}", STANDARD.encode([1u8; 16]));
        assert!(KycCrypto::from_config(&cfg).is_err());
    }
}
