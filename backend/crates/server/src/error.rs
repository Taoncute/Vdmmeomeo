use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    Api {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
    Internal(anyhow::Error),
}

impl AppError {
    pub fn api(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        AppError::Api {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::api(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::api(StatusCode::UNAUTHORIZED, "unauthorized", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::api(StatusCode::FORBIDDEN, "forbidden", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::api(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::api(StatusCode::CONFLICT, code, message)
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::api(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    pub fn too_many_requests() -> Self {
        Self::api(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Bạn thao tác quá nhanh. Hãy đợi một lát rồi thử lại.",
        )
    }

    pub fn internal(msg: impl fmt::Display) -> Self {
        AppError::Internal(anyhow::anyhow!("{msg}"))
    }

    /// Mã lỗi ngắn gọn, dùng trong test và log.
    pub fn code(&self) -> &str {
        match self {
            AppError::Api { code, .. } => code,
            AppError::Internal(_) => "internal_error",
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Api { code, message, .. } => write!(f, "{code}: {message}"),
            AppError::Internal(e) => write!(f, "internal error: {e}"),
        }
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Api {
                status,
                code,
                message,
            } => (
                status,
                Json(json!({ "error": { "code": code, "message": message } })),
            )
                .into_response(),
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "lỗi nội bộ");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": {
                            "code": "internal_error",
                            "message": "Lỗi hệ thống. Vui lòng thử lại sau."
                        }
                    })),
                )
                    .into_response()
            }
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(e)
    }
}

impl From<validator::ValidationErrors> for AppError {
    fn from(e: validator::ValidationErrors) -> Self {
        let mut fields: Vec<String> = e.field_errors().keys().map(|k| k.to_string()).collect();
        fields.sort();
        AppError::bad_request(
            "validation_error",
            format!("Dữ liệu không hợp lệ ở: {}", fields.join(", ")),
        )
    }
}

impl From<ledger::LedgerError> for AppError {
    fn from(e: ledger::LedgerError) -> Self {
        match e {
            ledger::LedgerError::InsufficientFunds {
                balance, needed, ..
            } => AppError::unprocessable(
                "insufficient_funds",
                format!(
                    "Không đủ số dư: hiện có {}, cần {}.",
                    crate::util::format_vnd(balance),
                    crate::util::format_vnd(needed)
                ),
            ),
            ledger::LedgerError::InvalidAmount => {
                AppError::bad_request("invalid_amount", "Số tiền không hợp lệ.")
            }
            ledger::LedgerError::Database(db) => AppError::from(db),
            other => AppError::internal(other),
        }
    }
}

fn db_code(e: &sqlx::Error) -> Option<String> {
    e.as_database_error()
        .and_then(|d| d.code())
        .map(|c| c.to_string())
}

/// Lỗi vi phạm ràng buộc UNIQUE của Postgres (mã 23505).
pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    db_code(e).as_deref() == Some("23505")
}

/// Lỗi vi phạm ràng buộc CHECK của Postgres (mã 23514).
pub fn is_check_violation(e: &sqlx::Error) -> bool {
    db_code(e).as_deref() == Some("23514")
}
