use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::audit::{self, AuditEntry};
use crate::error::{AppError, AppResult};

/// Tham số số nguyên: (tên, nhỏ nhất, lớn nhất).
const INT_KEYS: &[(&str, i64, i64)] = &[
    ("stall_price", 1, 1_000_000_000),
    ("stall_days", 1, 3650),
    ("withdraw_min", 1, 1_000_000_000_000),
    ("withdraw_fee_bps", 0, 10_000),
    ("hold_days", 0, 365),
    ("auto_complete_days", 0, 365),
    ("max_upload_mb", 1, 2048),
    ("download_limit", 1, 1000),
];
const BOOL_KEYS: &[&str] = &["auto_approve_trusted"];
const EXT_LIST_KEY: &str = "file_ext_allowlist";

/// Các tham số cấu hình có kiểu, đọc từ bảng `settings`.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub stall_price: i64,
    pub stall_days: i64,
    pub withdraw_min: i64,
    pub withdraw_fee_bps: i64,
    pub hold_days: i64,
    pub auto_complete_days: i64,
    pub auto_approve_trusted: bool,
    pub max_upload_mb: i64,
    pub download_limit: i64,
    pub file_ext_allowlist: Vec<String>,
}

impl Settings {
    fn from_map(map: &BTreeMap<String, Value>) -> AppResult<Self> {
        let int = |key: &str| -> AppResult<i64> {
            map.get(key)
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::internal(format!("setting {key} thiếu hoặc sai kiểu")))
        };
        let boolean = |key: &str| -> AppResult<bool> {
            map.get(key)
                .and_then(Value::as_bool)
                .ok_or_else(|| AppError::internal(format!("setting {key} thiếu hoặc sai kiểu")))
        };
        let ext_list = map
            .get(EXT_LIST_KEY)
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter()
                    .map(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
                    .collect::<Option<Vec<String>>>()
            })
            .ok_or_else(|| {
                AppError::internal(format!("setting {EXT_LIST_KEY} thiếu hoặc sai kiểu"))
            })?;
        Ok(Self {
            stall_price: int("stall_price")?,
            stall_days: int("stall_days")?,
            withdraw_min: int("withdraw_min")?,
            withdraw_fee_bps: int("withdraw_fee_bps")?,
            hold_days: int("hold_days")?,
            auto_complete_days: int("auto_complete_days")?,
            auto_approve_trusted: boolean("auto_approve_trusted")?,
            max_upload_mb: int("max_upload_mb")?,
            download_limit: int("download_limit")?,
            file_ext_allowlist: ext_list,
        })
    }
}

pub async fn load_all(conn: &mut PgConnection) -> AppResult<BTreeMap<String, Value>> {
    let rows = sqlx::query_as::<_, (String, Value)>("SELECT key, value FROM settings")
        .fetch_all(conn)
        .await?;
    Ok(rows.into_iter().collect())
}

pub async fn load(conn: &mut PgConnection) -> AppResult<Settings> {
    let map = load_all(conn).await?;
    Settings::from_map(&map)
}

pub fn validate_setting(key: &str, value: &Value) -> AppResult<()> {
    if let Some((_, min, max)) = INT_KEYS.iter().find(|(k, _, _)| *k == key) {
        let n = value.as_i64().ok_or_else(|| {
            AppError::bad_request("invalid_setting", format!("{key} phải là số nguyên"))
        })?;
        if n < *min || n > *max {
            return Err(AppError::bad_request(
                "invalid_setting",
                format!("{key} phải nằm trong khoảng {min} đến {max}"),
            ));
        }
        return Ok(());
    }
    if BOOL_KEYS.contains(&key) {
        if value.is_boolean() {
            return Ok(());
        }
        return Err(AppError::bad_request(
            "invalid_setting",
            format!("{key} phải là true hoặc false"),
        ));
    }
    if key == EXT_LIST_KEY {
        let arr = value.as_array().ok_or_else(|| {
            AppError::bad_request(
                "invalid_setting",
                format!("{key} phải là mảng các đuôi tệp"),
            )
        })?;
        if arr.is_empty() || arr.len() > 50 {
            return Err(AppError::bad_request(
                "invalid_setting",
                format!("{key} phải có từ 1 đến 50 đuôi tệp"),
            ));
        }
        for item in arr {
            let ext = item.as_str().ok_or_else(|| {
                AppError::bad_request(
                    "invalid_setting",
                    format!("Mỗi phần tử trong {key} phải là chuỗi"),
                )
            })?;
            let valid = !ext.is_empty()
                && ext.len() <= 16
                && ext
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
            if !valid {
                return Err(AppError::bad_request(
                    "invalid_setting",
                    format!(
                        "Đuôi tệp '{ext}' trong {key} không hợp lệ (chỉ chữ thường a-z và số 0-9)"
                    ),
                ));
            }
        }
        return Ok(());
    }
    Err(AppError::bad_request(
        "unknown_setting",
        format!("Không có tham số cấu hình tên {key}"),
    ))
}

/// Cập nhật nhiều tham số trong một transaction; mỗi tham số đổi giá trị ghi một dòng audit_log.
pub async fn update(
    db: &PgPool,
    actor: Uuid,
    ip: &str,
    changes: BTreeMap<String, Value>,
) -> AppResult<BTreeMap<String, Value>> {
    if changes.is_empty() {
        return Err(AppError::bad_request(
            "empty_patch",
            "Không có tham số nào để cập nhật.",
        ));
    }
    for (key, value) in &changes {
        validate_setting(key, value)?;
    }
    let mut tx = db.begin().await?;
    for (key, new) in &changes {
        let old =
            sqlx::query_scalar::<_, Value>("SELECT value FROM settings WHERE key = $1 FOR UPDATE")
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or_else(|| AppError::internal(format!("setting {key} không tồn tại")))?;
        if old == *new {
            continue;
        }
        sqlx::query(
            "UPDATE settings SET value = $2, updated_by = $3, updated_at = now() WHERE key = $1",
        )
        .bind(key)
        .bind(new)
        .bind(actor)
        .execute(&mut *tx)
        .await?;
        audit::log(
            &mut tx,
            AuditEntry {
                actor_id: Some(actor),
                action: "settings.update",
                target_type: "setting",
                target_id: Some(key.clone()),
                before: Some(old),
                after: Some(new.clone()),
                ip: Some(ip),
            },
        )
        .await?;
    }
    let all = load_all(&mut tx).await?;
    tx.commit().await?;
    Ok(all)
}
