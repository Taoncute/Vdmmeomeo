//! Cổng thanh toán: trait `PaymentGateway` và ba adapter (PayOS, chuyển khoản thủ công, giả).

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::error::AppError;

pub mod fake;
pub mod manual;
pub mod payos;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct CreatePayment {
    pub order_code: i64,
    pub amount: i64,
    pub transfer_code: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BankInstructions {
    pub bank_name: String,
    pub account_no: String,
    pub account_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PaymentInstructions {
    pub gateway: String,
    pub gateway_ref: Option<String>,
    pub checkout_url: Option<String>,
    pub qr_code: Option<String>,
    pub bank: Option<BankInstructions>,
    /// Nội dung chuyển khoản phải ghi đúng để đối chiếu.
    pub transfer_content: String,
    /// `true`: tiền tự vào ví khi cổng gọi webhook. `false`: chờ admin xác nhận.
    pub auto_confirm: bool,
}

/// Sự kiện thanh toán thành công đã được xác thực chữ ký.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookEvent {
    pub order_code: i64,
    pub amount: i64,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookError {
    InvalidSignature,
    Malformed(String),
    /// Chữ ký đúng nhưng đây không phải thông báo thanh toán thành công.
    NotPaymentSuccess,
    /// Adapter không nhận webhook (chuyển khoản thủ công).
    Unsupported,
}

#[async_trait]
pub trait PaymentGateway: Send + Sync {
    fn name(&self) -> &'static str;

    async fn create_payment(&self, req: &CreatePayment) -> Result<PaymentInstructions, AppError>;

    /// Xác thực chữ ký và đọc webhook từ thân request thô. Không đổi gì trong DB.
    fn verify_webhook(&self, raw_body: &[u8]) -> Result<WebhookEvent, WebhookError>;
}

pub fn hmac_sha256_hex(key: &str, msg: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC nhận khóa độ dài bất kỳ");
    mac.update(msg.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// So khớp chữ ký hex theo thời gian hằng số.
pub fn verify_hmac_hex(key: &str, msg: &str, signature_hex: &str) -> bool {
    let Ok(sig) = hex::decode(signature_hex.trim()) else {
        return false;
    };
    let mut mac = HmacSha256::new_from_slice(key.as_bytes()).expect("HMAC nhận khóa độ dài bất kỳ");
    mac.update(msg.as_bytes());
    mac.verify_slice(&sig).is_ok()
}
