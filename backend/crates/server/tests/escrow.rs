mod common;

use common::*;
use ledger::AccountKey;
use server::domain::escrow::{self, ReleaseOutcome};
use sqlx::PgPool;
use uuid::Uuid;

struct Setup {
    buyer: Uuid,
    seller: Uuid,
    order: Uuid,
}

async fn setup(pool: &PgPool, buyer_funds: i64) -> Setup {
    let buyer = create_user(pool, "mua@example.com").await;
    let seller = create_user(pool, "ban@example.com").await;
    fund(pool, AccountKey::wallet_spendable(buyer), buyer_funds).await;
    Setup {
        buyer,
        seller,
        order: Uuid::new_v4(),
    }
}

async fn hold_id(pool: &PgPool, order: Uuid) -> Uuid {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM earnings_holds WHERE order_ref = $1")
        .bind(order)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn make_hold_due(pool: &PgPool) {
    sqlx::query("UPDATE earnings_holds SET release_at = now() - interval '1 minute'")
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn doanh_thu_di_qua_escrow_pending_roi_available(pool: PgPool) {
    let s = setup(&pool, 1_000_000).await;

    escrow::hold(&pool, s.buyer, s.order, 400_000).await.unwrap();
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 600_000);
    assert_eq!(balance(&pool, AccountKey::escrow(s.order)).await, 400_000);

    escrow::complete(&pool, s.seller, s.order, 400_000).await.unwrap();
    assert_eq!(balance(&pool, AccountKey::escrow(s.order)).await, 0);
    assert_eq!(balance(&pool, AccountKey::earnings_pending(s.seller)).await, 400_000);
    assert_eq!(balance(&pool, AccountKey::earnings_available(s.seller)).await, 0);

    // Khoản giữ hẹn nhả sau hold_days (mặc định 3) ngày.
    let days: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM release_at - now()) / 86400)::float8 FROM earnings_holds",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((days - 3.0).abs() < 0.01, "release_at cách bây giờ {days} ngày");

    // Chưa đến hạn thì chưa nhả.
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 0);
    assert_eq!(balance(&pool, AccountKey::earnings_pending(s.seller)).await, 400_000);

    make_hold_due(&pool).await;
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 1);
    assert_eq!(balance(&pool, AccountKey::earnings_pending(s.seller)).await, 0);
    assert_eq!(balance(&pool, AccountKey::earnings_available(s.seller)).await, 400_000);
    // Chạy lại job thì không nhả đôi.
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 0);
    assert_eq!(balance(&pool, AccountKey::earnings_available(s.seller)).await, 400_000);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn hold_days_doi_trong_settings_co_hieu_luc(pool: PgPool) {
    set_setting(&pool, "hold_days", serde_json::json!(7)).await;
    let s = setup(&pool, 100_000).await;
    escrow::hold(&pool, s.buyer, s.order, 100_000).await.unwrap();
    escrow::complete(&pool, s.seller, s.order, 100_000).await.unwrap();
    let days: f64 = sqlx::query_scalar(
        "SELECT (extract(epoch FROM release_at - now()) / 86400)::float8 FROM earnings_holds",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((days - 7.0).abs() < 0.01, "{days}");
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn goi_lap_khong_ghi_so_hai_lan(pool: PgPool) {
    let s = setup(&pool, 1_000_000).await;
    assert!(escrow::hold(&pool, s.buyer, s.order, 400_000).await.unwrap().is_new());
    assert!(!escrow::hold(&pool, s.buyer, s.order, 400_000).await.unwrap().is_new());
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 600_000);

    assert!(escrow::complete(&pool, s.seller, s.order, 400_000).await.unwrap().is_new());
    assert!(!escrow::complete(&pool, s.seller, s.order, 400_000).await.unwrap().is_new());
    assert_eq!(balance(&pool, AccountKey::earnings_pending(s.seller)).await, 400_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM earnings_holds").await, 1);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn thanh_toan_thieu_tien_bi_tu_choi(pool: PgPool) {
    let s = setup(&pool, 100_000).await;
    let e = escrow::hold(&pool, s.buyer, s.order, 100_001).await.unwrap_err();
    assert_eq!(e.code(), "insufficient_funds");
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 100_000);
    assert_eq!(balance(&pool, AccountKey::escrow(s.order)).await, 0);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn hoan_tien_khi_tien_con_o_escrow(pool: PgPool) {
    let s = setup(&pool, 500_000).await;
    escrow::hold(&pool, s.buyer, s.order, 500_000).await.unwrap();
    escrow::refund_from_escrow(&pool, s.buyer, s.order, 500_000).await.unwrap();
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 500_000);
    assert_eq!(balance(&pool, AccountKey::escrow(s.order)).await, 0);

    // Hoàn lần hai (cùng khóa) không đổi gì; chốt đơn sau khi đã hoàn thì bị chặn.
    assert!(!escrow::refund_from_escrow(&pool, s.buyer, s.order, 500_000).await.unwrap().is_new());
    let e = escrow::complete(&pool, s.seller, s.order, 500_000).await.unwrap_err();
    assert_eq!(e.code(), "escrow_empty");
    assert_eq!(balance(&pool, AccountKey::earnings_pending(s.seller)).await, 0);
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 500_000);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn hoan_tien_khi_tien_dang_o_earnings_pending(pool: PgPool) {
    let s = setup(&pool, 300_000).await;
    escrow::hold(&pool, s.buyer, s.order, 300_000).await.unwrap();
    escrow::complete(&pool, s.seller, s.order, 300_000).await.unwrap();

    let wrong = escrow::refund_from_pending(&pool, s.seller, s.buyer, s.order, 299_999).await.unwrap_err();
    assert_eq!(wrong.code(), "amount_mismatch");

    escrow::refund_from_pending(&pool, s.seller, s.buyer, s.order, 300_000).await.unwrap();
    assert_eq!(balance(&pool, AccountKey::earnings_pending(s.seller)).await, 0);
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 300_000);
    let status: String = sqlx::query_scalar("SELECT status FROM earnings_holds")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "refunded");

    // Job nhả doanh thu không được nhả khoản đã hoàn, kể cả khi đã đến hạn.
    make_hold_due(&pool).await;
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 0);
    assert_eq!(balance(&pool, AccountKey::earnings_available(s.seller)).await, 0);
    assert_eq!(
        escrow::release_hold(&pool, hold_id(&pool, s.order).await).await.unwrap(),
        ReleaseOutcome::NotPending
    );
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn da_sang_earnings_available_thi_khong_hoan_tu_dong(pool: PgPool) {
    let s = setup(&pool, 300_000).await;
    escrow::hold(&pool, s.buyer, s.order, 300_000).await.unwrap();
    escrow::complete(&pool, s.seller, s.order, 300_000).await.unwrap();
    make_hold_due(&pool).await;
    escrow::release_due(&pool, 100).await.unwrap();
    assert_eq!(balance(&pool, AccountKey::earnings_available(s.seller)).await, 300_000);

    let e = escrow::refund_from_pending(&pool, s.seller, s.buyer, s.order, 300_000).await.unwrap_err();
    assert_eq!(e.code(), "refund_after_release");
    let e2 = escrow::refund_from_escrow(&pool, s.buyer, s.order, 300_000).await.unwrap_err();
    assert_eq!(e2.code(), "refund_not_possible");

    // Không ví nào thay đổi.
    assert_eq!(balance(&pool, AccountKey::earnings_available(s.seller)).await, 300_000);
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(s.buyer)).await, 0);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn don_chua_chot_khong_co_khoan_giu_de_hoan_tu_pending(pool: PgPool) {
    let s = setup(&pool, 100_000).await;
    escrow::hold(&pool, s.buyer, s.order, 100_000).await.unwrap();
    let e = escrow::refund_from_pending(&pool, s.seller, s.buyer, s.order, 100_000).await.unwrap_err();
    assert_eq!(e.code(), "hold_not_found");
    assert_eq!(balance(&pool, AccountKey::escrow(s.order)).await, 100_000);
}
