//! Dữ liệu demo cho thử tay. Chỉ dùng ở môi trường dev; mật khẩu công khai trong README.

use std::collections::BTreeMap;

use ledger::{kinds, AccountKey, TxBuilder};
use sqlx::PgConnection;
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

/// Nạp tiền demo từ tài khoản `external`. Khóa idempotency cố định nên chạy lại không cộng thêm.
/// Trả về `true` nếu lần này mới ghi.
async fn credit(
    conn: &mut PgConnection,
    email: &str,
    label: &str,
    account: AccountKey,
    amount: i64,
) -> anyhow::Result<bool> {
    let tx = TxBuilder::new(kinds::SEED, format!("seed:{email}:{label}"))
        .debit(AccountKey::external(), amount)
        .credit(account, amount)
        .build()?;
    Ok(ledger::post(conn, &tx).await?.is_new())
}

/// Số dư mẫu ở từng loại ví, một gian hàng đang thuê và KYC đã duyệt cho người bán demo.
pub async fn seed_wallets(
    state: &AppState,
    ids: &BTreeMap<String, Uuid>,
) -> anyhow::Result<()> {
    let buyer = ids["buyer@demo.local"];
    let seller = ids["seller@demo.local"];
    let mut tx = state.db.begin().await?;

    credit(&mut tx, "buyer@demo.local", "spendable", AccountKey::wallet_spendable(buyer), 5_000_000)
        .await?;
    credit(&mut tx, "seller@demo.local", "spendable", AccountKey::wallet_spendable(seller), 1_000_000)
        .await?;
    credit(&mut tx, "seller@demo.local", "available", AccountKey::earnings_available(seller), 3_000_000)
        .await?;
    let pending_is_new = credit(
        &mut tx,
        "seller@demo.local",
        "pending",
        AccountKey::earnings_pending(seller),
        500_000,
    )
    .await?;
    if pending_is_new {
        sqlx::query(
            "INSERT INTO earnings_holds (seller_id, order_ref, amount, release_at) \
             VALUES ($1, $2, $3, now() + interval '2 days')",
        )
        .bind(seller)
        .bind(Uuid::new_v4())
        .bind(500_000_i64)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO shops (owner_id, name, name_key, slug, bio, stall_until) \
         VALUES ($1, 'Gian hàng demo', 'gian hàng demo', 'gian-hang-demo', \
                 'Gian hàng mẫu để thử nghiệm.', now() + interval '30 days') \
         ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(seller)
    .execute(&mut *tx)
    .await?;
    grant_role(&mut tx, seller, Role::Seller).await?;

    let (enc, key_id) = state
        .crypto
        .encrypt("0123456789")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    sqlx::query(
        "INSERT INTO seller_kyc (user_id, full_name, bank_name, account_no_enc, key_id, status, \
                                 reviewed_at) \
         VALUES ($1, 'Người Bán Demo', 'Vietcombank', $2, $3, 'approved', now()) \
         ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(seller)
    .bind(enc)
    .bind(key_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn run(state: &AppState) -> anyhow::Result<()> {
    if state.config.app_env == "production" {
        anyhow::bail!("Không chạy seed ở môi trường production");
    }
    let ids = seed_users(state).await?;
    seed_wallets(state, &ids).await?;
    for (email, id) in &ids {
        tracing::info!(%email, %id, "đã seed người dùng demo");
    }
    Ok(())
}
