mod common;

use common::*;
use ledger::AccountKey;
use server::domain::escrow;
use sqlx::PgPool;
use uuid::Uuid;

async fn user_id(pool: &PgPool, email: &str) -> Uuid {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn seed_co_so_du_o_tung_loai_vi_va_chay_lai_khong_cong_them(pool: PgPool) {
    let (app, state) = test_app(pool.clone());
    server::seed::run(&state).await.unwrap();
    server::seed::run(&state).await.unwrap();

    let buyer = user_id(&pool, "buyer@demo.local").await;
    let seller = user_id(&pool, "seller@demo.local").await;
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(buyer)).await, 5_000_000);
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(seller)).await, 1_000_000);
    assert_eq!(balance(&pool, AccountKey::earnings_available(seller)).await, 3_000_000);
    assert_eq!(balance(&pool, AccountKey::earnings_pending(seller)).await, 500_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM earnings_holds").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM shops WHERE stall_until > now()").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM seller_kyc WHERE status = 'approved'").await, 1);
    assert_books_clean(&pool).await;

    // Người bán demo đăng nhập được và có vai seller, ví đúng số.
    let login = login(&app, "seller@demo.local", "Seller@12345").await;
    assert!(login.body["user"]["roles"].as_array().unwrap().contains(&serde_json::json!("seller")));
    let token = login.body["access_token"].as_str().unwrap().to_string();
    let wallet = get(&app, "/api/v1/wallet", Some(&token)).await;
    assert_eq!(wallet.body["earnings_available"], 3_000_000);

    // Khoản doanh thu đang giữ của seed nhả được khi đến hạn.
    sqlx::query("UPDATE earnings_holds SET release_at = now() - interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(escrow::release_due(&pool, 10).await.unwrap(), 1);
    assert_eq!(balance(&pool, AccountKey::earnings_available(seller)).await, 3_500_000);
}
