mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::Router;
use common::*;
use ledger::AccountKey;
use serde_json::{json, Map, Value};
use server::config::Config;
use server::domain::deposits::{self, ConfirmOutcome, ConfirmSource};
use server::payments::fake::FakeGateway;
use server::payments::payos::{sign_data, PayOsGateway, TEST_CHECKSUM_KEY};
use server::state::AppState;
use sqlx::PgPool;
use tokio::task::JoinSet;
use uuid::Uuid;

const WEBHOOK_URI: &str = "/api/v1/webhooks/payos";

/// App dùng adapter PayOS thật (chỉ phần ký và verify, không gọi mạng).
fn payos_app(pool: PgPool) -> Router {
    let cfg = Config::for_tests();
    let gw = PayOsGateway::from_config(&cfg).unwrap();
    let state = AppState::with_gateway(pool, cfg, Arc::new(gw)).unwrap();
    server::api::router(state)
}

fn payos_data(order_code: i64, amount: i64) -> Map<String, Value> {
    json!({
        "orderCode": order_code, "amount": amount, "description": "NAP",
        "accountNumber": "12345678", "reference": "TF230204212323",
        "transactionDateTime": "2023-02-04 18:25:00", "currency": "VND",
        "paymentLinkId": "124c33293c43417ab7879e14c8d9eb18", "code": "00", "desc": "Thành công",
        "counterAccountBankId": "", "counterAccountBankName": "", "counterAccountName": "",
        "counterAccountNumber": "", "virtualAccountName": "", "virtualAccountNumber": ""
    })
    .as_object()
    .unwrap()
    .clone()
}

fn webhook_body(data: &Map<String, Value>, signature: &str, success: bool) -> Vec<u8> {
    json!({ "code": "00", "desc": "success", "success": success, "data": data, "signature": signature })
        .to_string()
        .into_bytes()
}

fn signed_webhook(order_code: i64, amount: i64) -> Vec<u8> {
    let data = payos_data(order_code, amount);
    let sig = sign_data(&data, TEST_CHECKSUM_KEY);
    webhook_body(&data, &sig, true)
}

async fn new_deposit(pool: &PgPool, user: Uuid, amount: i64) -> deposits::Deposit {
    deposits::create(pool, user, amount, "payos").await.unwrap()
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn webhook_hop_le_cong_tien_mot_lan(pool: PgPool) {
    let app = payos_app(pool.clone());
    let u = create_user(&pool, "a@example.com").await;
    let d = new_deposit(&pool, u, 50_000).await;
    assert_eq!(d.transfer_code, format!("NAP{}", d.order_code));

    let r = post_raw(&app, WEBHOOK_URI, signed_webhook(d.order_code, 50_000)).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["result"], "confirmed");
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 50_000);

    // PayOS gửi lại: vẫn 200 nhưng không cộng thêm.
    for _ in 0..3 {
        let again = post_raw(&app, WEBHOOK_URI, signed_webhook(d.order_code, 50_000)).await;
        assert_eq!(again.status, StatusCode::OK);
        assert_eq!(again.body["result"], "already_confirmed");
    }
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 50_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_transactions WHERE kind = 'deposit'").await, 1);
    assert_eq!(count(&pool, "SELECT count(*) FROM notifications WHERE kind = 'deposit_confirmed'").await, 1);
    let status: String = sqlx::query_scalar("SELECT status FROM deposits WHERE id = $1")
        .bind(d.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "confirmed");
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn webhook_gui_dong_thoi_chi_cong_mot_lan(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    let d = new_deposit(&pool, u, 70_000).await;
    let big = big_pool(&pool).await;
    let app = payos_app(big);

    let mut set = JoinSet::new();
    for _ in 0..12 {
        let app = app.clone();
        let body = signed_webhook(d.order_code, 70_000);
        set.spawn(async move { post_raw(&app, WEBHOOK_URI, body).await });
    }
    let mut confirmed = 0;
    while let Some(res) = set.join_next().await {
        let r = res.unwrap();
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        if r.body["result"] == "confirmed" {
            confirmed += 1;
        }
    }
    assert_eq!(confirmed, 1, "đúng một webhook được ghi sổ");
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 70_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_transactions WHERE kind = 'deposit'").await, 1);
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn webhook_sai_chu_ky_bi_tu_choi(pool: PgPool) {
    let app = payos_app(pool.clone());
    let u = create_user(&pool, "a@example.com").await;
    let d = new_deposit(&pool, u, 50_000).await;

    // Chữ ký ký bằng khóa khác.
    let data = payos_data(d.order_code, 50_000);
    let forged = sign_data(&data, "khoa-gia-mao");
    let r = post_raw(&app, WEBHOOK_URI, webhook_body(&data, &forged, true)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["error"]["code"], "invalid_signature");

    // Sửa số tiền sau khi ký.
    let good_sig = sign_data(&data, TEST_CHECKSUM_KEY);
    let mut tampered = data.clone();
    tampered.insert("amount".to_string(), json!(5_000_000));
    let r = post_raw(&app, WEBHOOK_URI, webhook_body(&tampered, &good_sig, true)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Thiếu chữ ký, thân không phải JSON.
    let r = post_raw(&app, WEBHOOK_URI, webhook_body(&data, "", true)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = post_raw(&app, WEBHOOK_URI, b"khong phai json".to_vec()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 0);
    let status: String = sqlx::query_scalar("SELECT status FROM deposits WHERE id = $1")
        .bind(d.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_transactions").await, 0);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn webhook_sai_so_tien_hoac_khong_phai_thanh_cong_khong_cong_tien(pool: PgPool) {
    let app = payos_app(pool.clone());
    let u = create_user(&pool, "a@example.com").await;
    let d = new_deposit(&pool, u, 50_000).await;

    // Chữ ký đúng nhưng số tiền không khớp lệnh nạp.
    let r = post_raw(&app, WEBHOOK_URI, signed_webhook(d.order_code, 49_000)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "amount_mismatch");

    // Chữ ký đúng nhưng success = false.
    let data = payos_data(d.order_code, 50_000);
    let sig = sign_data(&data, TEST_CHECKSUM_KEY);
    let r = post_raw(&app, WEBHOOK_URI, webhook_body(&data, &sig, false)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["ignored"], "not_a_successful_payment");

    // Mã đơn không tồn tại (như webhook thử khi đăng ký URL): 200 nhưng bỏ qua.
    let r = post_raw(&app, WEBHOOK_URI, signed_webhook(123, 3000)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["ignored"], "unknown_order");

    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_transactions").await, 0);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn admin_xac_nhan_va_webhook_den_sau_khong_cong_doi(pool: PgPool) {
    let u = create_user(&pool, "a@example.com").await;
    let admin = create_admin(&pool, "admin@example.com").await;
    let stranger = create_user(&pool, "b@example.com").await;
    let d = new_deposit(&pool, u, 80_000).await;

    // Người không phải admin không xác nhận được.
    let denied = deposits::confirm(&pool, d.id, None, ConfirmSource::Admin(admin_ctx(stranger)))
        .await
        .unwrap_err();
    assert_eq!(denied.code(), "forbidden");
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 0);

    let first = deposits::confirm(&pool, d.id, None, ConfirmSource::Admin(admin_ctx(admin)))
        .await
        .unwrap();
    assert!(matches!(first, ConfirmOutcome::Confirmed { .. }));
    let second = deposits::confirm(&pool, d.id, None, ConfirmSource::Admin(admin_ctx(admin)))
        .await
        .unwrap();
    assert_eq!(second, ConfirmOutcome::AlreadyConfirmed);

    // Webhook đến muộn sau khi admin đã xác nhận.
    let app = payos_app(pool.clone());
    let late = post_raw(&app, WEBHOOK_URI, signed_webhook(d.order_code, 80_000)).await;
    assert_eq!(late.body["result"], "already_confirmed");
    assert_eq!(balance(&pool, AccountKey::wallet_spendable(u)).await, 80_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM ledger_transactions WHERE kind = 'deposit'").await, 1);
    let by: Option<Uuid> = sqlx::query_scalar("SELECT confirmed_by FROM deposits WHERE id = $1")
        .bind(d.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(by, Some(admin));
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn tao_lenh_nap_chuyen_khoan_thu_cong_qua_http(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "nap@example.com").await;
    let admin = register_admin(&app, &pool, "admin@example.com").await;

    assert_eq!(post(&app, "/api/v1/deposits", json!({ "amount": 1000 }), None).await.status, StatusCode::UNAUTHORIZED);
    for bad in [0, -5, 1_000_000_001] {
        let r = post(&app, "/api/v1/deposits", json!({ "amount": bad }), Some(&s.access)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "amount {bad}: {:?}", r.body);
    }

    let r = post(&app, "/api/v1/deposits", json!({ "amount": 120_000 }), Some(&s.access)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["payment"]["gateway"], "manual");
    assert_eq!(r.body["payment"]["auto_confirm"], false);
    let code = r.body["deposit"]["transfer_code"].as_str().unwrap().to_string();
    assert_eq!(r.body["payment"]["transfer_content"], code.as_str());
    assert_eq!(r.body["payment"]["bank"]["account_no"], "123456789");
    let id = r.body["deposit"]["id"].as_str().unwrap().to_string();

    // Chưa xác nhận thì ví chưa có tiền; webhook không dùng được với chuyển khoản thủ công.
    assert_eq!(get(&app, "/api/v1/wallet", Some(&s.access)).await.body["spendable"], 0);
    let hook = post_raw(&app, WEBHOOK_URI, b"{}".to_vec()).await;
    assert_eq!(hook.status, StatusCode::NOT_FOUND);

    let pending = get(&app, "/api/v1/admin/deposits?status=pending", Some(&admin.access)).await;
    assert_eq!(pending.body["deposits"][0]["email"], "nap@example.com");
    let denied = post(&app, &format!("/api/v1/admin/deposits/{id}/confirm"), json!({}), Some(&s.access)).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let ok = post(&app, &format!("/api/v1/admin/deposits/{id}/confirm"), json!({}), Some(&admin.access)).await;
    assert_eq!(ok.body["result"], "confirmed");
    let again = post(&app, &format!("/api/v1/admin/deposits/{id}/confirm"), json!({}), Some(&admin.access)).await;
    assert_eq!(again.body["result"], "already_confirmed");

    let wallet = get(&app, "/api/v1/wallet", Some(&s.access)).await;
    assert_eq!(wallet.body["spendable"], 120_000);
    let entries = get(&app, "/api/v1/wallet/entries", Some(&s.access)).await;
    assert_eq!(entries.body["entries"][0]["amount"], 120_000);
    assert_eq!(entries.body["entries"][0]["account_kind"], "wallet_spendable");
    assert_eq!(entries.body["entries"][0]["tx_kind"], "deposit");
    let notes = get(&app, "/api/v1/notifications", Some(&s.access)).await;
    assert_eq!(notes.body["notifications"][0]["kind"], "deposit_confirmed");
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn adapter_gia_tao_link_va_nhan_webhook(pool: PgPool) {
    let gw = Arc::new(FakeGateway::new("bi-mat-test"));
    let state = AppState::with_gateway(pool.clone(), Config::for_tests(), gw.clone()).unwrap();
    let app = server::api::router(state);
    let s = register(&app, "fake@example.com").await;

    let r = post(&app, "/api/v1/deposits", json!({ "amount": 30_000 }), Some(&s.access)).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["payment"]["gateway"], "fake");
    assert_eq!(r.body["payment"]["auto_confirm"], true);
    assert!(r.body["payment"]["checkout_url"].as_str().unwrap().starts_with("https://fake-gateway.local/pay/"));
    let order_code = r.body["deposit"]["order_code"].as_i64().unwrap();
    let gateway_ref: Option<String> = sqlx::query_scalar("SELECT gateway_ref FROM deposits WHERE order_code = $1")
        .bind(order_code)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(gateway_ref, Some(format!("fake-{order_code}")));

    let bad = json!({ "order_code": order_code, "amount": 30_000, "signature": "00" }).to_string();
    assert_eq!(post_raw(&app, WEBHOOK_URI, bad.into_bytes()).await.status, StatusCode::UNAUTHORIZED);
    let good = json!({ "order_code": order_code, "amount": 30_000, "signature": gw.sign(order_code, 30_000) })
        .to_string();
    let ok = post_raw(&app, WEBHOOK_URI, good.into_bytes()).await;
    assert_eq!(ok.body["result"], "confirmed");
    assert_eq!(get(&app, "/api/v1/wallet", Some(&s.access)).await.body["spendable"], 30_000);
}
