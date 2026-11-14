use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

pub struct AuditEntry<'a> {
    pub actor_id: Option<Uuid>,
    pub action: &'a str,
    pub target_type: &'a str,
    pub target_id: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub ip: Option<&'a str>,
}

/// Ghi một dòng `audit_log` trong cùng transaction với thao tác đang làm.
pub async fn log(conn: &mut PgConnection, e: AuditEntry<'_>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit_log (actor_id, action, target_type, target_id, before, after, ip) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(e.actor_id)
    .bind(e.action)
    .bind(e.target_type)
    .bind(e.target_id)
    .bind(e.before)
    .bind(e.after)
    .bind(e.ip)
    .execute(conn)
    .await?;
    Ok(())
}
