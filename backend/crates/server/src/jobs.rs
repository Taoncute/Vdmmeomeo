//! Hàng đợi job trong Postgres: lấy việc bằng `FOR UPDATE SKIP LOCKED`.
//! Job đang chạy giữ khóa dòng; tiến trình chết giữa chừng thì transaction hủy và job vẫn còn trong hàng đợi.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

const MAX_ATTEMPTS: i32 = 5;

/// Job lặp lại: (loại, chu kỳ tính bằng giây). Sau mỗi lần chạy, job kế tiếp được xếp lịch.
pub const RECURRING: &[(&str, i64)] = &[];

pub async fn enqueue(
    db: &PgPool,
    kind: &str,
    payload: Value,
    run_at: DateTime<Utc>,
    dedupe_key: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO jobs (kind, payload, run_at, dedupe_key) VALUES ($1, $2, $3, $4) \
         ON CONFLICT DO NOTHING",
    )
    .bind(kind)
    .bind(payload)
    .bind(run_at)
    .bind(dedupe_key)
    .execute(db)
    .await?;
    Ok(())
}

/// Bảo đảm mỗi job lặp lại có đúng một bản đang chờ.
pub async fn ensure_recurring(db: &PgPool) -> Result<(), sqlx::Error> {
    for (kind, _) in RECURRING {
        enqueue(db, kind, json!({}), Utc::now(), Some(*kind)).await?;
    }
    Ok(())
}

async fn dispatch(_state: &AppState, kind: &str, _payload: &Value) -> AppResult<()> {
    match kind {
        "noop" => Ok(()),
        other => Err(AppError::internal(format!("Không biết loại job: {other}"))),
    }
}

/// Chạy mọi job đến hạn, trả về số job đã xử lý (kể cả job lỗi).
pub async fn run_due_jobs(state: &AppState) -> AppResult<usize> {
    let mut processed = 0usize;
    loop {
        let mut tx = state.db.begin().await?;
        let row = sqlx::query_as::<_, (uuid::Uuid, String, Value, i32)>(
            "SELECT id, kind, payload, attempts FROM jobs \
             WHERE status = 'queued' AND run_at <= now() \
             ORDER BY run_at LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;
        let Some((id, kind, payload, attempts)) = row else {
            tx.commit().await?;
            break;
        };
        match dispatch(state, &kind, &payload).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE jobs SET status = 'done', attempts = attempts + 1, \
                     last_error = NULL, finished_at = now() WHERE id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            Err(e) => {
                tracing::error!(job_id = %id, kind = %kind, error = %e, "job lỗi");
                let next_attempts = attempts + 1;
                if next_attempts >= MAX_ATTEMPTS {
                    sqlx::query(
                        "UPDATE jobs SET status = 'failed', attempts = $2, last_error = $3, \
                         finished_at = now() WHERE id = $1",
                    )
                    .bind(id)
                    .bind(next_attempts)
                    .bind(e.to_string())
                    .execute(&mut *tx)
                    .await?;
                } else {
                    let backoff_secs: i64 = 30 * (1i64 << attempts.min(10));
                    sqlx::query(
                        "UPDATE jobs SET attempts = $2, last_error = $3, \
                         run_at = now() + ($4 * interval '1 second') WHERE id = $1",
                    )
                    .bind(id)
                    .bind(next_attempts)
                    .bind(e.to_string())
                    .bind(backoff_secs)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
        if let Some((_, every)) = RECURRING.iter().find(|(k, _)| *k == kind) {
            sqlx::query(
                "INSERT INTO jobs (kind, run_at, dedupe_key) \
                 VALUES ($1, now() + ($2 * interval '1 second'), $1) ON CONFLICT DO NOTHING",
            )
            .bind(&kind)
            .bind(*every)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        processed += 1;
        if processed >= 1000 {
            break;
        }
    }
    Ok(processed)
}

pub async fn run_worker(state: AppState) -> anyhow::Result<()> {
    tracing::info!("worker khởi động");
    loop {
        if let Err(e) = ensure_recurring(&state.db).await {
            tracing::error!(error = ?e, "không xếp được job lặp lại");
        }
        match run_due_jobs(&state).await {
            Ok(n) if n > 0 => tracing::info!(processed = n, "đã chạy job"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "worker gặp lỗi"),
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(15)) => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("worker dừng");
                return Ok(());
            }
        }
    }
}
