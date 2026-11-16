use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::AuthUser;
use crate::domain::deposits::{self, ConfirmOutcome, ConfirmSource};
use crate::error::{AppError, AppResult};
use crate::payments::{CreatePayment, WebhookError};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateDepositRequest {
    pub amount: i64,
}

/// Tạo lệnh nạp và trả hướng dẫn thanh toán của cổng đang bật (PayOS, chuyển khoản thủ công...).
pub async fn create_deposit(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateDepositRequest>,
) -> AppResult<(StatusCode, Json<Value>)> {
    let deposit = deposits::create(&state.db, user.id, req.amount, state.gateway.name()).await?;
    let payment = state
        .gateway
        .create_payment(&CreatePayment {
            order_code: deposit.order_code,
            amount: deposit.amount,
            transfer_code: deposit.transfer_code.clone(),
        })
        .await;
    let payment = match payment {
        Ok(p) => p,
        Err(e) => {
            deposits::cancel_pending(&state.db, deposit.id).await?;
            return Err(e);
        }
    };
    if let Some(gateway_ref) = &payment.gateway_ref {
        deposits::set_gateway_ref(&state.db, deposit.id, gateway_ref).await?;
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "deposit": deposit, "payment": payment })),
    ))
}

pub async fn list_deposits(
    State(state): State<AppState>,
    user: AuthUser,
) -> AppResult<Json<Value>> {
    let rows = deposits::list_for_user(&state.db, user.id, 50).await?;
    Ok(Json(json!({ "deposits": rows })))
}

/// Webhook của cổng thanh toán. Xác thực chữ ký trên thân request thô trước khi làm bất cứ điều gì.
/// Gọi lặp thì không cộng tiền lần hai.
pub async fn payment_webhook(State(state): State<AppState>, body: Bytes) -> AppResult<Json<Value>> {
    let event = match state.gateway.verify_webhook(&body) {
        Ok(event) => event,
        Err(WebhookError::InvalidSignature) => {
            tracing::warn!("webhook thanh toán sai chữ ký, đã từ chối");
            return Err(AppError::api(
                StatusCode::UNAUTHORIZED,
                "invalid_signature",
                "Chữ ký webhook không hợp lệ.",
            ));
        }
        Err(WebhookError::Malformed(msg)) => {
            return Err(AppError::bad_request("malformed_webhook", msg));
        }
        Err(WebhookError::NotPaymentSuccess) => {
            return Ok(Json(
                json!({ "ok": true, "ignored": "not_a_successful_payment" }),
            ));
        }
        Err(WebhookError::Unsupported) => {
            return Err(AppError::not_found(
                "Cổng thanh toán hiện tại không nhận webhook.",
            ));
        }
    };
    let Some(deposit_id) = deposits::find_by_order_code(&state.db, event.order_code).await? else {
        // Ví dụ webhook thử khi đăng ký URL với PayOS: chữ ký đúng nhưng không có lệnh nạp nào.
        return Ok(Json(json!({ "ok": true, "ignored": "unknown_order" })));
    };
    let outcome = deposits::confirm(
        &state.db,
        deposit_id,
        Some(event.amount),
        ConfirmSource::Webhook,
    )
    .await?;
    let result = match outcome {
        ConfirmOutcome::Confirmed { .. } => "confirmed",
        ConfirmOutcome::AlreadyConfirmed => "already_confirmed",
    };
    Ok(Json(json!({ "ok": true, "result": result })))
}
