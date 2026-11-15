use chrono::{DateTime, FixedOffset, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::AppResult;

/// Nhắc trước khi gian hàng hết hạn (số ngày).
pub const RENT_REMINDER_DAYS: i64 = 3;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub kind: String,
    pub body: String,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Ngày theo giờ Việt Nam (UTC+7), dạng `dd/mm/yyyy`, dùng trong nội dung thông báo.
pub fn vn_date(t: DateTime<Utc>) -> String {
    let vn = FixedOffset::east_opt(7 * 3600).expect("múi giờ +07:00 hợp lệ");
    t.with_timezone(&vn).format("%d/%m/%Y").to_string()
}

/// Tạo thông báo; `dedupe_key` trùng với thông báo cũ của cùng người thì bỏ qua.
/// Trả về `true` nếu có thông báo mới.
pub async fn create(
    conn: &mut PgConnection,
    user_id: Uuid,
    kind: &str,
    body: &str,
    dedupe_key: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "INSERT INTO notifications (user_id, kind, body, dedupe_key) VALUES ($1, $2, $3, $4) \
         ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(kind)
    .bind(body)
    .bind(dedupe_key)
    .execute(conn)
    .await?;
    Ok(res.rows_affected() > 0)
}

pub async fn list(db: &PgPool, user_id: Uuid, limit: i64) -> AppResult<Vec<Notification>> {
    let rows = sqlx::query_as::<_, Notification>(
        "SELECT id, kind, body, read_at, created_at FROM notifications \
         WHERE user_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2",
    )
    .bind(user_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Job: nhắc người bán có gian hàng sắp hết hạn. Mỗi hạn chỉ nhắc một lần
/// (gia hạn xong thì hạn mới sẽ được nhắc lại). Trả về số thông báo mới tạo.
pub async fn create_stall_reminders(db: &PgPool) -> AppResult<usize> {
    let shops = sqlx::query_as::<_, (Uuid, Uuid, DateTime<Utc>)>(
        "SELECT id, owner_id, stall_until FROM shops \
         WHERE stall_until IS NOT NULL AND stall_until > now() \
         AND stall_until <= now() + ($1::bigint * 86400 * interval '1 second')",
    )
    .bind(RENT_REMINDER_DAYS)
    .fetch_all(db)
    .await?;
    let mut created = 0usize;
    for (shop_id, owner_id, stall_until) in shops {
        let dedupe = format!("stall_expiring:{shop_id}:{}", stall_until.timestamp());
        let body = format!(
            "Gian hàng của bạn sẽ hết hạn vào {}. Hãy gia hạn để sản phẩm tiếp tục hiển thị trên sàn.",
            vn_date(stall_until)
        );
        let mut conn = db.acquire().await?;
        if create(&mut conn, owner_id, "stall_expiring", &body, Some(dedupe.as_str())).await? {
            created += 1;
        }
    }
    Ok(created)
}
