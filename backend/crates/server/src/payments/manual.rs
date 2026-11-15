use async_trait::async_trait;

use super::{
    BankInstructions, CreatePayment, PaymentGateway, PaymentInstructions, WebhookError,
    WebhookEvent,
};
use crate::config::Config;
use crate::error::AppError;

/// Chuyển khoản thủ công theo mã nội dung; admin đối chiếu rồi bấm xác nhận.
pub struct ManualGateway {
    bank: BankInstructions,
}

impl ManualGateway {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            bank: BankInstructions {
                bank_name: cfg.manual_bank_name.clone(),
                account_no: cfg.manual_account_no.clone(),
                account_name: cfg.manual_account_name.clone(),
            },
        }
    }
}

#[async_trait]
impl PaymentGateway for ManualGateway {
    fn name(&self) -> &'static str {
        "manual"
    }

    async fn create_payment(&self, req: &CreatePayment) -> Result<PaymentInstructions, AppError> {
        Ok(PaymentInstructions {
            gateway: "manual".to_string(),
            gateway_ref: None,
            checkout_url: None,
            qr_code: None,
            bank: Some(self.bank.clone()),
            transfer_content: req.transfer_code.clone(),
            auto_confirm: false,
        })
    }

    fn verify_webhook(&self, _raw_body: &[u8]) -> Result<WebhookEvent, WebhookError> {
        Err(WebhookError::Unsupported)
    }
}
