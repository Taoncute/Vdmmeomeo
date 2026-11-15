mod common;

use axum::http::StatusCode;
use common::*;
use ledger::AccountKey;
use serde_json::json;
use server::domain::withdrawals;
use sqlx::PgPool;
use tokio::task::JoinSet;
use uuid::Uuid;

/// Người bán có KYC đã duyệt và `earnings` đồng doanh thu rút được.
async fn seller(pool: &PgPool, email: &str, earnings: i64) -> Uuid {
    let id = create_user(pool, email).await;
    set_kyc(pool, id, "approved").await;
    if earnings > 0 {
        fund(pool, AccountKey::earnings_available(id), earnings).await;
    }
    id
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn rut_dung_bang_so_du_duoc(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 2_000_000).await;
    let w = withdrawals::create(&pool, u, 2_000_000, None).await.unwrap();
    assert_eq!((w.amount, w.fee, w.net), (2_000_000, 200_000, 1_800_000));
    assert_eq!(w.status, "pending");
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 0);
    assert_eq!(balance(&pool, AccountKey::withdrawal_hold(u)).await, 2_000_000);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn rut_vuot_so_du_hoac_duoi_toi_thieu_bi_tu_choi(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 1_500_000).await;
    let over = withdrawals::create(&pool, u, 1_500_001, None).await.unwrap_err();
    assert_eq!(over.code(), "insufficient_earnings");
    let low = withdrawals::create(&pool, u, 999_999, None).await.unwrap_err();
    assert_eq!(low.code(), "below_minimum");
    assert_eq!(count(&pool, "SELECT count(*) FROM withdrawals").await, 0);
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 1_500_000);
    assert_eq!(balance(&pool, AccountKey::withdrawal_hold(u)).await, 0);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn tien_nap_khong_bao_gio_rut_duoc(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 0).await;
    fund(&pool, AccountKey::wallet_spendable(u), 50_000_000).await;
    let e = withdrawals::create(&pool, u, 1_000_000, None).await.unwrap_err();
    assert_eq!(e.code(), "insufficient_earnings");
    let p = withdrawals::preview(&pool, u, 1_000_000).await.unwrap();
    assert!(!p.eligible);
    assert_eq!(p.reason.as_deref(), Some("insufficient_earnings"));

    // Có thêm doanh thu thì chỉ phần doanh thu được tính, ví nạp vẫn không góp vào.
    fund(&pool, AccountKey::earnings_available(u), 500_000).await;
    let e = withdrawals::create(&pool, u, 2_000_000, None).await.unwrap_err();
    assert_eq!(e.code(), "insufficient_earnings");
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 50_000_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM withdrawals").await, 0);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn phai_co_kyc_da_duyet(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    fund(&pool, AccountKey::earnings_available(u), 5_000_000).await;
    let e = withdrawals::create(&pool, u, 1_000_000, None).await.unwrap_err();
    assert_eq!(e.code(), "kyc_required");
    for status in ["pending", "rejected"] {
        set_kyc(&pool, u, status).await;
        let e = withdrawals::create(&pool, u, 1_000_000, None).await.unwrap_err();
        assert_eq!(e.code(), "kyc_required", "KYC {status}");
    }
    set_kyc(&pool, u, "approved").await;
    withdrawals::create(&pool, u, 1_000_000, None).await.unwrap();
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn con_cong_no_thi_khong_rut(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 5_000_000).await;
    fund(&pool, AccountKey::seller_debt(u), 100_000).await;
    let e = withdrawals::create(&pool, u, 1_000_000, None).await.unwrap_err();
    assert_eq!(e.code(), "has_debt");
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 5_000_000);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn xem_truoc_phi_va_so_thuc_nhan(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 5_000_000).await;
    let p = withdrawals::preview(&pool, u, 1_000_005).await.unwrap();
    assert_eq!((p.fee, p.net), (100_001, 900_004));
    assert!(p.eligible);
    assert_eq!(p.reason, None);
    assert_eq!(p.earnings_available, 5_000_000);

    let big = withdrawals::preview(&pool, u, 6_000_000).await.unwrap();
    assert_eq!((big.fee, big.net), (600_000, 5_400_000));
    assert!(!big.eligible);

    let zero = withdrawals::preview(&pool, u, 0).await.unwrap_err();
    assert_eq!(zero.code(), "invalid_amount");
    // Xem trước không ghi gì.
    assert_eq!(count(&pool, "SELECT count(*) FROM withdrawals").await, 0);
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 5_000_000);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn idempotency_key_chong_tao_trung(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 5_000_000).await;
    let a = withdrawals::create(&pool, u, 1_000_000, Some("khoa-1")).await.unwrap();
    let b = withdrawals::create(&pool, u, 1_000_000, Some("khoa-1")).await.unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 4_000_000);
    let c = withdrawals::create(&pool, u, 1_000_000, Some("khoa-2")).await.unwrap();
    assert_ne!(a.id, c.id);
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 3_000_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM withdrawals").await, 2);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn tao_lenh_rut_dong_thoi_khong_vuot_so_du(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 10_000_000).await;
    let big = big_pool(&pool).await;

    let mut set = JoinSet::new();
    for i in 0..10 {
        let db = big.clone();
        set.spawn(async move {
            let key = format!("khoa-{i}");
            withdrawals::create(&db, u, 3_000_000, Some(key.as_str())).await
        });
    }
    let (mut ok, mut rejected) = (0, 0);
    while let Some(res) = set.join_next().await {
        match res.unwrap() {
            Ok(_) => ok += 1,
            Err(e) => {
                assert_eq!(e.code(), "insufficient_earnings");
                rejected += 1;
            }
        }
    }
    assert_eq!((ok, rejected), (3, 7), "chỉ đủ tiền cho đúng 3 lệnh");
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 1_000_000);
    assert_eq!(balance(&pool, AccountKey::withdrawal_hold(u)).await, 9_000_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM withdrawals").await, 3);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn duyet_lenh_rut_ghi_so_dung_phi_da_chot(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 3_000_000).await;
    let admin = create_admin(&pool, "admin@example.com").await;
    let w = withdrawals::create(&pool, u, 3_000_000, None).await.unwrap();
    assert_eq!(w.fee, 300_000);

    // Đổi phí sau khi tạo lệnh: lệnh cũ vẫn dùng đúng phí người dùng đã thấy.
    set_setting(&pool, "withdraw_fee_bps", json!(2000)).await;

    let empty = withdrawals::approve(&pool, admin_ctx(admin), w.id, "  ").await.unwrap_err();
    assert_eq!(empty.code(), "bank_txn_ref_required");
    let stranger = create_user(&pool, "khong-phai-admin@example.com").await;
    let denied = withdrawals::approve(&pool, admin_ctx(stranger), w.id, "VCB-1").await.unwrap_err();
    assert_eq!(denied.code(), "forbidden");

    let done = withdrawals::approve(&pool, admin_ctx(admin), w.id, "VCB-12345").await.unwrap();
    assert_eq!(done.status, "approved");
    assert_eq!(done.bank_txn_ref.as_deref(), Some("VCB-12345"));
    assert_eq!(balance(&pool, AccountKey::withdrawal_hold(u)).await, 0);
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 0);
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 300_000);
    assert_eq!(balance(&pool, AccountKey::external()).await, -300_000);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM ledger_entries e JOIN ledger_transactions t ON t.id = e.tx_id WHERE t.kind = 'withdrawal_approve'").await,
        3
    );

    let audit: (Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT actor_id, ip FROM audit_log WHERE action = 'withdrawal.approve'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(audit, (Some(admin), Some("127.0.0.1".to_string())));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM notifications WHERE kind = 'withdrawal_approved'").await,
        1
    );

    // Đã xử lý rồi thì duyệt hoặc từ chối lại đều bị chặn, sổ không đổi.
    for r in [
        withdrawals::approve(&pool, admin_ctx(admin), w.id, "VCB-2").await,
        withdrawals::reject(&pool, admin_ctx(admin), w.id, "nhầm").await,
    ] {
        assert_eq!(r.unwrap_err().code(), "withdrawal_not_pending");
    }
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 300_000);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn tu_choi_tra_tien_ve_doanh_thu(pool: PgPool) {
    let u = seller(&pool, "a@example.com", 2_000_000).await;
    let admin = create_admin(&pool, "admin@example.com").await;
    let w = withdrawals::create(&pool, u, 2_000_000, None).await.unwrap();

    let no_reason = withdrawals::reject(&pool, admin_ctx(admin), w.id, " ").await.unwrap_err();
    assert_eq!(no_reason.code(), "reason_required");
    let r = withdrawals::reject(&pool, admin_ctx(admin), w.id, "Sai thông tin ngân hàng").await.unwrap();
    assert_eq!(r.status, "rejected");
    assert_eq!(r.reason.as_deref(), Some("Sai thông tin ngân hàng"));
    assert_eq!(balance(&pool, AccountKey::earnings_available(u)).await, 2_000_000);
    assert_eq!(balance(&pool, AccountKey::withdrawal_hold(u)).await, 0);
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 0);
    // Rút lại được sau khi bị từ chối.
    withdrawals::create(&pool, u, 2_000_000, None).await.unwrap();
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn phi_bang_0_va_phi_100_phan_tram_van_ghi_so_dung(pool: PgPool) {
    let admin = create_admin(&pool, "admin@example.com").await;
    for (bps, fee, net) in [(0, 0, 1_000_000), (10_000, 1_000_000, 0)] {
        set_setting(&pool, "withdraw_fee_bps", json!(bps)).await;
        let u = seller(&pool, &format!("u{bps}@example.com"), 1_000_000).await;
        let w = withdrawals::create(&pool, u, 1_000_000, None).await.unwrap();
        assert_eq!((w.fee, w.net), (fee, net));
        withdrawals::approve(&pool, admin_ctx(admin), w.id, "MGD").await.unwrap();
        assert_eq!(balance(&pool, AccountKey::withdrawal_hold(u)).await, 0);
    }
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 1_000_000);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn http_rut_tien_tu_kyc_den_duyet(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "ban@example.com").await;
    let admin = register_admin(&app, &pool, "admin@example.com").await;
    fund(&pool, AccountKey::earnings_available(s.user_id), 5_000_000).await;

    // Chưa KYC thì không rút được.
    let no_kyc = post(&app, "/api/v1/withdrawals", json!({ "amount": 1_000_000 }), Some(&s.access)).await;
    assert_eq!(no_kyc.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(no_kyc.body["error"]["code"], "kyc_required");

    let bad = post(&app, "/api/v1/me/kyc", json!({ "full_name": "A", "bank_name": "VCB", "account_no": "12" }), Some(&s.access)).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let kyc = post(
        &app,
        "/api/v1/me/kyc",
        json!({ "full_name": "Nguyễn Văn Bán", "bank_name": "Vietcombank", "account_no": "0123 4567 89" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(kyc.status, StatusCode::OK, "{:?}", kyc.body);
    assert_eq!(kyc.body["status"], "pending");
    assert_eq!(kyc.body["account_no_masked"], "******6789");

    let pending = get(&app, "/api/v1/admin/kyc?status=pending", Some(&admin.access)).await;
    assert_eq!(pending.body["kyc"][0]["account_no"], "0123456789");
    let approve = post(&app, &format!("/api/v1/admin/kyc/{}/approve", s.user_id), json!({}), Some(&admin.access)).await;
    assert_eq!(approve.status, StatusCode::OK, "{:?}", approve.body);

    let preview = post(&app, "/api/v1/withdrawals/preview", json!({ "amount": 1_000_005 }), Some(&s.access)).await;
    assert_eq!(preview.status, StatusCode::OK);
    assert_eq!((preview.body["fee"].as_i64(), preview.body["net"].as_i64()), (Some(100_001), Some(900_004)));
    assert_eq!(preview.body["eligible"], true);

    let bad_key = post_with(&app, "/api/v1/withdrawals", json!({ "amount": 1_000_000 }), Some(&s.access), &[("idempotency-key", "khoa co dau cach")]).await;
    assert_eq!(bad_key.status, StatusCode::BAD_REQUEST);

    let first = post_with(&app, "/api/v1/withdrawals", json!({ "amount": 1_000_000 }), Some(&s.access), &[("idempotency-key", "abc-1")]).await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    let again = post_with(&app, "/api/v1/withdrawals", json!({ "amount": 1_000_000 }), Some(&s.access), &[("idempotency-key", "abc-1")]).await;
    assert_eq!(again.body["id"], first.body["id"]);

    let wallet = get(&app, "/api/v1/wallet", Some(&s.access)).await;
    assert_eq!(wallet.body["earnings_available"], 4_000_000);
    assert_eq!(wallet.body["withdrawal_hold"], 1_000_000);
    assert_eq!(wallet.body["spendable"], 0);
    assert_eq!(wallet.body["earnings_pending"], 0);

    // Admin thấy lệnh kèm số tài khoản đã giải mã; người thường thì không vào được.
    let list = get(&app, "/api/v1/admin/withdrawals?status=pending", Some(&admin.access)).await;
    assert_eq!(list.body["withdrawals"].as_array().unwrap().len(), 1);
    assert_eq!(list.body["withdrawals"][0]["kyc"]["account_no"], "0123456789");
    let id = first.body["id"].as_str().unwrap();
    let denied = post(&app, &format!("/api/v1/admin/withdrawals/{id}/approve"), json!({ "bank_txn_ref": "X" }), Some(&s.access)).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let ok = post(&app, &format!("/api/v1/admin/withdrawals/{id}/approve"), json!({ "bank_txn_ref": "MB-777" }), Some(&admin.access)).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(ok.body["status"], "approved");
    let mine = get(&app, "/api/v1/withdrawals", Some(&s.access)).await;
    assert_eq!(mine.body["withdrawals"][0]["status"], "approved");
    assert_books_clean(&pool).await;
}
