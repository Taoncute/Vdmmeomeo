//! Dữ liệu demo cho thử tay. Chỉ dùng ở môi trường dev; mật khẩu công khai trong README.

use std::collections::BTreeMap;

use uuid::Uuid;

use crate::auth::{grant_role, hash_password_async, Role};
use crate::state::AppState;

pub struct DemoUser {
    pub email: &'static str,
    pub display_name: &'static str,
    pub password: &'static str,
    pub roles: &'static [Role],
}

pub const DEMO_USERS: &[DemoUser] = &[
    DemoUser {
        email: "admin@demo.local",
        display_name: "Quản trị viên",
        password: "Admin@12345",
        roles: &[Role::Admin],
    },
    DemoUser {
        email: "reviewer@demo.local",
        display_name: "Người duyệt",
        password: "Reviewer@12345",
        roles: &[Role::Reviewer],
    },
    DemoUser {
        email: "buyer@demo.local",
        display_name: "Người mua demo",
        password: "Buyer@12345",
        roles: &[],
    },
    DemoUser {
        email: "seller@demo.local",
        display_name: "Người bán demo",
        password: "Seller@12345",
        roles: &[],
    },
];

/// Tạo (hoặc giữ nguyên) các người dùng demo. Trả về map email -> id.
pub async fn seed_users(state: &AppState) -> anyhow::Result<BTreeMap<String, Uuid>> {
    let mut ids = BTreeMap::new();
    for u in DEMO_USERS {
        let hash = hash_password_async(
            state.config.argon2_m_kib,
            state.config.argon2_t,
            u.password.to_string(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut tx = state.db.begin().await?;
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO users (email, password_hash, display_name) VALUES ($1, $2, $3) \
             ON CONFLICT (email) DO UPDATE SET display_name = EXCLUDED.display_name \
             RETURNING id",
        )
        .bind(u.email)
        .bind(hash)
        .bind(u.display_name)
        .fetch_one(&mut *tx)
        .await?;
        grant_role(&mut tx, id, Role::Buyer).await?;
        for role in u.roles {
            grant_role(&mut tx, id, *role).await?;
        }
        tx.commit().await?;
        ids.insert(u.email.to_string(), id);
    }
    Ok(ids)
}

pub async fn run(state: &AppState) -> anyhow::Result<()> {
    if state.config.app_env == "production" {
        anyhow::bail!("Không chạy seed ở môi trường production");
    }
    let ids = seed_users(state).await?;
    for (email, id) in &ids {
        tracing::info!(%email, %id, "đã seed người dùng demo");
    }
    Ok(())
}
