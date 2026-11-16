mod common;

use axum::http::StatusCode;
use common::*;
use ledger::AccountKey;
use serde_json::json;
use server::domain::notifications;
use server::domain::stall::{self, Source};
use sqlx::PgPool;
use uuid::Uuid;

async fn funded_user(pool: &PgPool, email: &str, spendable: i64) -> Uuid {
    let id = create_user(pool, email).await;
    if spendable > 0 {
        fund(pool, AccountKey::wallet_spendable(id), spendable).await;
    }
    id
}

async fn stall_until(pool: &PgPool, user: Uuid) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("SELECT stall_until FROM shops WHERE owner_id = $1")
        .bind(user)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn has_role(pool: &PgPool, user: Uuid, role: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id \
         WHERE ur.user_id = $1 AND r.name = $2)",
    )
    .bind(user)
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn thue_lan_dau_tao_shop_cap_vai_seller_va_tru_tien(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 200_000).await;
    assert!(!has_role(&pool, u, "seller").await);

    let r = stall::rent(&pool, u, Source::Spendable, Some("  Cửa Hàng Đẹp "), None)
        .await
        .unwrap();
    assert!(r.created_shop && !r.replayed);
    assert_eq!(r.shop.name, "Cửa Hàng Đẹp");
    assert_eq!(r.shop.slug, "cua-hang-dep");
    assert!(has_role(&pool, u, "seller").await);
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(u)).await,
        101_000
    );
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 99_000);

    let days = (stall_until(&pool, u).await - chrono::Utc::now()).num_seconds() as f64 / 86_400.0;
    assert!((days - 30.0).abs() < 0.01, "hạn còn {days} ngày");
    let (price, days_col, source): (i64, i32, String) =
        sqlx::query_as("SELECT price, days, source FROM stall_rentals")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (price, days_col, source.as_str()),
        (99_000, 30, "spendable")
    );
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_log WHERE action = 'stall.rent'"
        )
        .await,
        1
    );
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn lan_dau_bat_buoc_ten_shop_va_ten_phai_hop_le(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 500_000).await;
    for (name, code) in [
        (None, "shop_name_required"),
        (Some("   "), "shop_name_required"),
        (Some("ab"), "invalid_shop_name"),
        (Some("a".repeat(41).as_str()), "invalid_shop_name"),
    ] {
        let e = stall::rent(&pool, u, Source::Spendable, name, None)
            .await
            .unwrap_err();
        assert_eq!(e.code(), code, "tên {name:?}");
    }
    // Lỗi thì không tạo shop, không trừ tiền, không cấp vai.
    assert_eq!(count(&pool, "SELECT count(*) FROM shops").await, 0);
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(u)).await,
        500_000
    );
    assert!(!has_role(&pool, u, "seller").await);
    // Đúng 3 và đúng 40 ký tự đều được.
    stall::rent(&pool, u, Source::Spendable, Some("Abc"), None)
        .await
        .unwrap();
    let v = funded_user(&pool, "b@example.com", 500_000).await;
    stall::rent(
        &pool,
        v,
        Source::Spendable,
        Some("b".repeat(40).as_str()),
        None,
    )
    .await
    .unwrap();
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn ten_shop_duy_nhat_khong_phan_biet_hoa_thuong(pool: PgPool) {
    let a = funded_user(&pool, "a@example.com", 200_000).await;
    let b = funded_user(&pool, "b@example.com", 200_000).await;
    stall::rent(&pool, a, Source::Spendable, Some("Cửa Hàng Đẹp"), None)
        .await
        .unwrap();
    let e = stall::rent(&pool, b, Source::Spendable, Some("CỬA HÀNG ĐẸP"), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "shop_name_taken");
    // Giao dịch bị hủy trọn vẹn: người b không mất tiền.
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(b)).await,
        200_000
    );
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 99_000);
    assert_eq!(count(&pool, "SELECT count(*) FROM shops").await, 1);
    // Slug trùng nhưng tên khác thì được cấp slug khác.
    stall::rent(&pool, b, Source::Spendable, Some("cua hang dep"), None)
        .await
        .unwrap();
    let slugs: Vec<String> = sqlx::query_scalar("SELECT slug FROM shops ORDER BY slug")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        slugs,
        vec!["cua-hang-dep".to_string(), "cua-hang-dep-2".to_string()]
    );
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn gia_han_khi_con_han_cong_don_30_ngay(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 300_000).await;
    stall::rent(&pool, u, Source::Spendable, Some("Shop Một"), None)
        .await
        .unwrap();
    let first = stall_until(&pool, u).await;

    // Gia hạn bỏ qua shop_name (kể cả khi gửi tên khác).
    let r = stall::rent(&pool, u, Source::Spendable, Some("Tên Khác Hẳn"), None)
        .await
        .unwrap();
    assert!(!r.created_shop);
    assert_eq!(r.shop.name, "Shop Một");
    assert_eq!(
        stall_until(&pool, u).await - first,
        chrono::Duration::days(30)
    );
    // Gia hạn không cần shop_name.
    stall::rent(&pool, u, Source::Spendable, None, None)
        .await
        .unwrap();
    assert_eq!(
        stall_until(&pool, u).await - first,
        chrono::Duration::days(60)
    );
    assert_eq!(
        balance(&pool, AccountKey::platform_revenue()).await,
        297_000
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM stall_rentals").await, 3);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn gia_han_khi_da_het_han_tinh_tu_bay_gio(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 200_000).await;
    stall::rent(&pool, u, Source::Spendable, Some("Shop Hết Hạn"), None)
        .await
        .unwrap();
    sqlx::query("UPDATE shops SET stall_until = now() - interval '10 days'")
        .execute(&pool)
        .await
        .unwrap();
    stall::rent(&pool, u, Source::Spendable, None, None)
        .await
        .unwrap();
    let days = (stall_until(&pool, u).await - chrono::Utc::now()).num_seconds() as f64 / 86_400.0;
    assert!((days - 30.0).abs() < 0.01, "{days}");
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn tra_bang_doanh_thu_va_khong_tu_chuyen_nguon(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 1_000_000).await;
    // Ví nạp nhiều tiền nhưng chọn nguồn doanh thu (đang 0): phải báo thiếu, không lấy ví nạp.
    let e = stall::rent(&pool, u, Source::Earnings, Some("Shop Doanh Thu"), None)
        .await
        .unwrap_err();
    assert_eq!(e.code(), "insufficient_funds_source");
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(u)).await,
        1_000_000
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM shops").await, 0);

    fund(&pool, AccountKey::earnings_available(u), 150_000).await;
    stall::rent(&pool, u, Source::Earnings, Some("Shop Doanh Thu"), None)
        .await
        .unwrap();
    assert_eq!(
        balance(&pool, AccountKey::earnings_available(u)).await,
        51_000
    );
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(u)).await,
        1_000_000
    );
    assert_eq!(balance(&pool, AccountKey::platform_revenue()).await, 99_000);
    let source: String = sqlx::query_scalar("SELECT source FROM stall_rentals")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(source, "earnings");
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn idempotency_key_khong_tru_tien_hai_lan(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 500_000).await;
    let a = stall::rent(
        &pool,
        u,
        Source::Spendable,
        Some("Shop Một Lần"),
        Some("khoa-1"),
    )
    .await
    .unwrap();
    let until = stall_until(&pool, u).await;
    let b = stall::rent(
        &pool,
        u,
        Source::Spendable,
        Some("Shop Một Lần"),
        Some("khoa-1"),
    )
    .await
    .unwrap();
    assert!(b.replayed);
    assert_eq!(a.rental_id, b.rental_id);
    assert_eq!(stall_until(&pool, u).await, until);
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(u)).await,
        401_000
    );
    assert_eq!(count(&pool, "SELECT count(*) FROM stall_rentals").await, 1);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn gia_thue_lay_tu_settings(pool: PgPool) {
    set_setting(&pool, "stall_price", json!(120_000)).await;
    set_setting(&pool, "stall_days", json!(10)).await;
    let u = funded_user(&pool, "a@example.com", 200_000).await;
    stall::rent(&pool, u, Source::Spendable, Some("Shop Giá Mới"), None)
        .await
        .unwrap();
    assert_eq!(
        balance(&pool, AccountKey::platform_revenue()).await,
        120_000
    );
    let days = (stall_until(&pool, u).await - chrono::Utc::now()).num_seconds() as f64 / 86_400.0;
    assert!((days - 10.0).abs() < 0.01, "{days}");
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn job_nhac_gia_han_truoc_3_ngay_moi_han_nhac_mot_lan(pool: PgPool) {
    let u = funded_user(&pool, "a@example.com", 300_000).await;
    stall::rent(&pool, u, Source::Spendable, Some("Shop Sắp Hết"), None)
        .await
        .unwrap();
    let remind = || async { notifications::create_stall_reminders(&pool).await.unwrap() };

    // Còn 30 ngày: chưa nhắc.
    assert_eq!(remind().await, 0);
    // Còn 2 ngày: nhắc một lần, chạy lại không nhắc thêm.
    sqlx::query("UPDATE shops SET stall_until = now() + interval '2 days'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(remind().await, 1);
    assert_eq!(remind().await, 0);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM notifications WHERE kind = 'stall_expiring'"
        )
        .await,
        1
    );
    // Đã hết hạn thì không nhắc nữa.
    sqlx::query("UPDATE shops SET stall_until = now() - interval '1 day'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(remind().await, 0);
    // Gia hạn xong rồi lại gần hết hạn: hạn mới được nhắc lại.
    sqlx::query("UPDATE shops SET stall_until = now() + interval '1 day'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(remind().await, 1);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM notifications WHERE kind = 'stall_expiring'"
        )
        .await,
        2
    );
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn job_nen_chay_qua_hang_doi(pool: PgPool) {
    let (_, state) = test_app(pool.clone());
    let u = funded_user(&pool, "a@example.com", 300_000).await;
    stall::rent(&pool, u, Source::Spendable, Some("Shop Hàng Đợi"), None)
        .await
        .unwrap();
    sqlx::query("UPDATE shops SET stall_until = now() + interval '1 day'")
        .execute(&pool)
        .await
        .unwrap();
    for kind in ["rent_reminders", "ledger_reconcile", "release_holds"] {
        server::jobs::enqueue(&pool, kind, json!({}), chrono::Utc::now(), None)
            .await
            .unwrap();
    }
    assert_eq!(server::jobs::run_due_jobs(&state).await.unwrap(), 3);
    let failed = count(
        &pool,
        "SELECT count(*) FROM jobs WHERE status <> 'done' AND run_at <= now()",
    )
    .await;
    assert_eq!(failed, 0, "các job phải chạy xong không lỗi");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM notifications WHERE kind = 'stall_expiring'"
        )
        .await,
        1
    );
    // Mỗi job lặp lại tự xếp lịch lần kế tiếp, và chỉ một bản đang chờ.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status = 'queued'").await,
        3
    );
    server::jobs::ensure_recurring(&pool).await.unwrap();
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE status = 'queued'").await,
        3
    );
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn http_thue_gian_hang_va_sua_ho_so(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "ban@example.com").await;
    fund(&pool, AccountKey::wallet_spendable(s.user_id), 250_000).await;

    // PATCH /shops/me khi chưa có shop.
    let none = patch(
        &app,
        "/api/v1/shops/me",
        json!({ "bio": "x" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);

    let bad = post(
        &app,
        "/api/v1/stall/rent",
        json!({ "source": "tien-mat" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let no_name = post(
        &app,
        "/api/v1/stall/rent",
        json!({ "source": "spendable" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(no_name.status, StatusCode::BAD_REQUEST);
    assert_eq!(no_name.body["error"]["code"], "shop_name_required");

    let first = post(
        &app,
        "/api/v1/stall/rent",
        json!({ "source": "spendable", "shop_name": "Cửa Hàng HTTP" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.body);
    assert_eq!(first.body["created_shop"], true);
    assert_eq!(first.body["shop"]["slug"], "cua-hang-http");

    let renew = post(
        &app,
        "/api/v1/stall/rent",
        json!({ "source": "spendable" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(renew.status, StatusCode::OK, "{:?}", renew.body);
    assert_eq!(renew.body["created_shop"], false);

    let me = get(&app, "/api/v1/me", Some(&s.access)).await;
    assert_eq!(me.body["shop"]["name"], "Cửa Hàng HTTP");
    assert!(me.body["roles"]
        .as_array()
        .unwrap()
        .contains(&json!("seller")));
    assert_eq!(
        get(&app, "/api/v1/wallet", Some(&s.access)).await.body["spendable"],
        52_000
    );

    let upd = patch(
        &app,
        "/api/v1/shops/me",
        json!({ "bio": "  Bán plugin chất lượng  ", "avatar_url": "https://example.com/logo.png" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(upd.status, StatusCode::OK, "{:?}", upd.body);
    assert_eq!(upd.body["bio"], "Bán plugin chất lượng");
    assert_eq!(upd.body["avatar_url"], "https://example.com/logo.png");
    assert_eq!(upd.body["name"], "Cửa Hàng HTTP");
    let bad_logo = patch(
        &app,
        "/api/v1/shops/me",
        json!({ "avatar_url": "http://x/y.png" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(bad_logo.status, StatusCode::BAD_REQUEST);
    let cleared = patch(
        &app,
        "/api/v1/shops/me",
        json!({ "bio": "" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(cleared.body["bio"], serde_json::Value::Null);
    assert_books_clean(&pool).await;
}
