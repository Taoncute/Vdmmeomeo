use async_trait::async_trait;
use serde::Deserialize;

use super::{
    hmac_sha256_hex, verify_hmac_hex, CreatePayment, PaymentGateway, PaymentInstructions,
    WebhookError, WebhookEvent,
};
use crate::error::AppError;

/// Adapter giả cho dev/test. Webhook dạng `{"order_code", "amount", "signature"}`,
/// chữ ký là HMAC-SHA256 của `"{order_code}:{amount}"` với khóa bí mật của adapter.
pub struct FakeGateway {
    secret: String,
}

#[derive(Deserialize)]
struct FakeWebhook {
    order_code: i64,
    amount: i64,
    signature: String,
}

impl FakeGateway {
    pub fn new(secret: &str) -> Self {
        Self {
            secret: secret.to_string(),
        }
    }

    pub fn sign(&self, order_code: i64, amount: i64) -> String {
        hmac_sha256_hex(&self.secret, &format!("{order_code}:{amount}"))
    }
}

#[async_trait]
impl PaymentGateway for FakeGateway {
    fn name(&self) -> &'static str {
        "fake"
    }

    async fn create_payment(&self, req: &CreatePayment) -> Result<PaymentInstructions, AppError> {
        Ok(PaymentInstructions {
            gateway: "fake".to_string(),
            gateway_ref: Some(format!("fake-{}", req.order_code)),
            checkout_url: Some(format!("https://fake-gateway.local/pay/{}", req.order_code)),
            qr_code: None,
            bank: None,
            transfer_content: req.transfer_code.clone(),
            auto_confirm: true,
        })
    }

    fn verify_webhook(&self, raw_body: &[u8]) -> Result<WebhookEvent, WebhookError> {
        let w: FakeWebhook = serde_json::from_slice(raw_body)
            .map_err(|e| WebhookError::Malformed(e.to_string()))?;
        let msg = format!("{}:{}", w.order_code, w.amount);
        if !verify_hmac_hex(&self.secret, &msg, &w.signature) {
            return Err(WebhookError::InvalidSignature);
        }
        Ok(WebhookEvent {
            order_code: w.order_code,
            amount: w.amount,
            reference: None,
        })
    }
}
