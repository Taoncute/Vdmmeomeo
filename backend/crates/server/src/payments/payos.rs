//! Adapter PayOS. Cách ký và định dạng webhook theo tài liệu payOS hiện hành
//! (https://payos.vn/docs/tich-hop-webhook/kiem-tra-du-lieu-voi-signature/):
//! HMAC_SHA256 (hex) của chuỗi `key1=value1&key2=value2...` với các key trong `data` sắp xếp
//! theo alphabet, khóa là Checksum Key của kênh thanh toán.
//!
//! Tạo link thanh toán: `POST {base}/v2/payment-requests`, header `x-client-id`, `x-api-key`;
//! chữ ký của request tạo link là
//! `amount=..&cancelUrl=..&description=..&orderCode=..&returnUrl=..`.
//! Phần tạo link chưa được thử với PayOS thật (chưa có tài khoản): chỉ phần ký và verify webhook
//! có test bằng dữ liệu mẫu.

use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::{
    hmac_sha256_hex, verify_hmac_hex, CreatePayment, PaymentGateway, PaymentInstructions,
    WebhookError, WebhookEvent,
};
use crate::config::Config;
use crate::error::AppError;

/// Checksum Key trong dữ liệu mẫu của tài liệu PayOS, chỉ dùng cho test.
pub const TEST_CHECKSUM_KEY: &str =
    "1a54716c8f0efb2744fb28b6e38b25da7f67a925d98bc1c18bd8faaecadd7675";

pub struct PayOsGateway {
    client_id: String,
    api_key: String,
    checksum_key: String,
    base_url: String,
    return_url: String,
    cancel_url: String,
    http: reqwest::Client,
}

#[derive(Deserialize)]
struct PayosWebhook {
    code: String,
    success: bool,
    data: Map<String, Value>,
    signature: String,
}

/// Giá trị của một trường khi ghép chuỗi ký: null thành rỗng, số và chuỗi giữ nguyên.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

/// Chuỗi dùng để ký: `key=value` nối bằng `&`, key theo thứ tự alphabet.
pub fn signing_string(data: &Map<String, Value>) -> String {
    let mut keys: Vec<&String> = data.keys().collect();
    keys.sort();
    keys.iter()
        .map(|k| format!("{}={}", k, value_to_string(&data[k.as_str()])))
        .collect::<Vec<_>>()
        .join("&")
}

pub fn sign_data(data: &Map<String, Value>, checksum_key: &str) -> String {
    hmac_sha256_hex(checksum_key, &signing_string(data))
}

impl PayOsGateway {
    pub fn from_config(cfg: &Config) -> anyhow::Result<Self> {
        if cfg.payos_client_id.is_empty()
            || cfg.payos_api_key.is_empty()
            || cfg.payos_checksum_key.is_empty()
        {
            anyhow::bail!(
                "PAYMENT_GATEWAY=payos cần PAYOS_CLIENT_ID, PAYOS_API_KEY và PAYOS_CHECKSUM_KEY"
            );
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| anyhow!("không tạo được HTTP client: {e}"))?;
        Ok(Self {
            client_id: cfg.payos_client_id.clone(),
            api_key: cfg.payos_api_key.clone(),
            checksum_key: cfg.payos_checksum_key.clone(),
            base_url: cfg.payos_base_url.trim_end_matches('/').to_string(),
            return_url: cfg.payos_return_url.clone(),
            cancel_url: cfg.payos_cancel_url.clone(),
            http,
        })
    }

    fn gateway_error(msg: impl std::fmt::Display) -> AppError {
        tracing::error!(error = %msg, "lỗi khi gọi PayOS");
        AppError::api(
            StatusCode::BAD_GATEWAY,
            "payment_gateway_error",
            "Không tạo được link thanh toán PayOS. Vui lòng thử lại sau.",
        )
    }
}

#[async_trait]
impl PaymentGateway for PayOsGateway {
    fn name(&self) -> &'static str {
        "payos"
    }

    async fn create_payment(&self, req: &CreatePayment) -> Result<PaymentInstructions, AppError> {
        // PayOS giới hạn mô tả 9 ký tự với tài khoản ngân hàng không liên kết qua payOS.
        let description: String = req.transfer_code.chars().take(9).collect();
        let to_sign = format!(
            "amount={}&cancelUrl={}&description={}&orderCode={}&returnUrl={}",
            req.amount, self.cancel_url, description, req.order_code, self.return_url
        );
        let body = json!({
            "orderCode": req.order_code,
            "amount": req.amount,
            "description": description,
            "cancelUrl": self.cancel_url,
            "returnUrl": self.return_url,
            "signature": hmac_sha256_hex(&self.checksum_key, &to_sign),
        });
        let resp = self
            .http
            .post(format!("{}/v2/payment-requests", self.base_url))
            .header("x-client-id", &self.client_id)
            .header("x-api-key", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(Self::gateway_error)?;
        let status = resp.status();
        let value: Value = resp.json().await.map_err(Self::gateway_error)?;
        if !status.is_success() || value["code"] != "00" {
            return Err(Self::gateway_error(format!(
                "PayOS trả về {status}: {}",
                value["desc"]
            )));
        }
        let data = &value["data"];
        Ok(PaymentInstructions {
            gateway: "payos".to_string(),
            gateway_ref: data["paymentLinkId"].as_str().map(str::to_string),
            checkout_url: data["checkoutUrl"].as_str().map(str::to_string),
            qr_code: data["qrCode"].as_str().map(str::to_string),
            bank: None,
            transfer_content: req.transfer_code.clone(),
            auto_confirm: true,
        })
    }

    fn verify_webhook(&self, raw_body: &[u8]) -> Result<WebhookEvent, WebhookError> {
        let payload: PayosWebhook =
            serde_json::from_slice(raw_body).map_err(|e| WebhookError::Malformed(e.to_string()))?;
        if !verify_hmac_hex(
            &self.checksum_key,
            &signing_string(&payload.data),
            &payload.signature,
        ) {
            return Err(WebhookError::InvalidSignature);
        }
        let data_code = payload
            .data
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !(payload.success && payload.code == "00" && data_code == "00") {
            return Err(WebhookError::NotPaymentSuccess);
        }
        let order_code = payload
            .data
            .get("orderCode")
            .and_then(Value::as_i64)
            .ok_or_else(|| WebhookError::Malformed("thiếu orderCode".to_string()))?;
        let amount = payload
            .data
            .get("amount")
            .and_then(Value::as_i64)
            .ok_or_else(|| WebhookError::Malformed("thiếu amount".to_string()))?;
        let reference = payload
            .data
            .get("reference")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(WebhookEvent {
            order_code,
            amount,
            reference,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dữ liệu mẫu và chữ ký trong tài liệu PayOS (ví dụ PHP, đã kiểm lại bằng HMAC-SHA256 thật).
    /// Lưu ý: ví dụ JSON ở cùng trang ghi chữ ký khác (8d8640d8...) và không khớp với dữ liệu đi kèm;
    /// tài liệu tự mâu thuẫn ở điểm đó, ta bám theo ví dụ PHP.
    const SAMPLE_SIGNATURE: &str =
        "412e915d2871504ed31be63c8f62a149a4410d34c4c42affc9006ef9917eaa03";

    fn sample_data() -> Map<String, Value> {
        let v = json!({
            "orderCode": 123,
            "amount": 3000,
            "description": "VQRIO123",
            "accountNumber": "12345678",
            "reference": "TF230204212323",
            "transactionDateTime": "2023-02-04 18:25:00",
            "currency": "VND",
            "paymentLinkId": "124c33293c43417ab7879e14c8d9eb18",
            "code": "00",
            "desc": "Thành công",
            "counterAccountBankId": "",
            "counterAccountBankName": "",
            "counterAccountName": "",
            "counterAccountNumber": "",
            "virtualAccountName": "",
            "virtualAccountNumber": ""
        });
        v.as_object().expect("object").clone()
    }

    fn gateway() -> PayOsGateway {
        PayOsGateway::from_config(&Config::for_tests()).expect("gateway")
    }

    fn body(data: &Map<String, Value>, signature: &str, success: bool) -> Vec<u8> {
        json!({
            "code": "00",
            "desc": "success",
            "success": success,
            "data": data,
            "signature": signature,
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn chuoi_ky_sap_xep_alphabet() {
        let s = signing_string(&sample_data());
        assert!(s.starts_with("accountNumber=12345678&amount=3000&code=00&counterAccountBankId=&"));
        assert!(s.contains("&desc=Thành công&description=VQRIO123&orderCode=123&"));
        assert!(s.ends_with("&virtualAccountName=&virtualAccountNumber="));
    }

    #[test]
    fn chu_ky_khop_du_lieu_mau_cua_payos() {
        assert_eq!(
            sign_data(&sample_data(), TEST_CHECKSUM_KEY),
            SAMPLE_SIGNATURE
        );
    }

    #[test]
    fn webhook_hop_le_duoc_chap_nhan() {
        let ev = gateway()
            .verify_webhook(&body(&sample_data(), SAMPLE_SIGNATURE, true))
            .expect("hợp lệ");
        assert_eq!(ev.order_code, 123);
        assert_eq!(ev.amount, 3000);
        assert_eq!(ev.reference.as_deref(), Some("TF230204212323"));
    }

    #[test]
    fn webhook_sai_chu_ky_bi_tu_choi() {
        let gw = gateway();
        // Sửa số tiền nhưng giữ chữ ký cũ.
        let mut tampered = sample_data();
        tampered.insert("amount".to_string(), json!(3_000_000));
        assert_eq!(
            gw.verify_webhook(&body(&tampered, SAMPLE_SIGNATURE, true)),
            Err(WebhookError::InvalidSignature)
        );
        // Chữ ký sai hoàn toàn, chữ ký không phải hex, chữ ký rỗng.
        for bad in ["00".repeat(32).as_str(), "khong-phai-hex", ""] {
            assert_eq!(
                gw.verify_webhook(&body(&sample_data(), bad, true)),
                Err(WebhookError::InvalidSignature),
                "chữ ký: {bad:?}"
            );
        }
        // Ký bằng khóa khác.
        let other = sign_data(&sample_data(), "khoa-khac");
        assert_eq!(
            gw.verify_webhook(&body(&sample_data(), &other, true)),
            Err(WebhookError::InvalidSignature)
        );
    }

    #[test]
    fn webhook_khong_phai_thanh_cong_hoac_hong() {
        let gw = gateway();
        // Chữ ký đúng nhưng success = false.
        assert_eq!(
            gw.verify_webhook(&body(&sample_data(), SAMPLE_SIGNATURE, false)),
            Err(WebhookError::NotPaymentSuccess)
        );
        // data.code khác "00" (ký lại cho hợp lệ).
        let mut failed = sample_data();
        failed.insert("code".to_string(), json!("01"));
        let sig = sign_data(&failed, TEST_CHECKSUM_KEY);
        assert_eq!(
            gw.verify_webhook(&body(&failed, &sig, true)),
            Err(WebhookError::NotPaymentSuccess)
        );
        assert!(matches!(
            gw.verify_webhook(b"khong phai json"),
            Err(WebhookError::Malformed(_))
        ));
    }
}
