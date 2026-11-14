mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use common::*;
use serde_json::json;
use server::config::Config;
use server::state::AppState;
use sqlx::PgPool;

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn dang_ky_dang_nhap_va_me(pool: PgPool) {
    let (app, _) = test_app(pool.clone());

    let s = register(&app, "Nguoi.Dung@Example.com").await;
    let me = get(&app, "/api/v1/me", Some(&s.access)).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["email"], "nguoi.dung@example.com");
    assert_eq!(me.body["roles"], json!(["buyer"]));

    // Email trùng (không phân biệt hoa thường).
    let dup = post(
        &app,
        "/api/v1/auth/register",
        json!({ "email": "NGUOI.DUNG@example.com", "password": PASSWORD, "display_name": "X" }),
        None,
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert_eq!(dup.body["error"]["code"], "email_taken");

    // Mật khẩu ngắn, email sai.
    let weak = post(
        &app,
        "/api/v1/auth/register",
        json!({ "email": "a@example.com", "password": "ngan", "display_name": "X" }),
        None,
    )
    .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    let bad_email = post(
        &app,
        "/api/v1/auth/register",
        json!({ "email": "khong-phai-email", "password": PASSWORD, "display_name": "X" }),
        None,
    )
    .await;
    assert_eq!(bad_email.status, StatusCode::BAD_REQUEST);

    // Đăng nhập sai mật khẩu và email không tồn tại cho cùng một lỗi.
    let wrong = login(&app, "nguoi.dung@example.com", "sai-mat-khau").await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body["error"]["code"], "invalid_credentials");
    let unknown = login(&app, "khong-co@example.com", PASSWORD).await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.body["error"]["code"], "invalid_credentials");

    let ok = login(&app, "NGUOI.DUNG@example.com", PASSWORD).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert!(ok.body["access_token"].is_string());
    assert!(refresh_cookie_from(&ok.headers).is_some());

    // Không có token hoặc token rác thì 401.
    assert_eq!(
        get(&app, "/api/v1/me", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&app, "/api/v1/me", Some("token.rac.rua")).await.status,
        StatusCode::UNAUTHORIZED
    );

    // PATCH /me.
    let upd = patch(
        &app,
        "/api/v1/me",
        json!({ "display_name": "  Tên mới  ", "avatar_url": "https://example.com/a.png" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(upd.status, StatusCode::OK);
    assert_eq!(upd.body["display_name"], "Tên mới");
    assert_eq!(upd.body["avatar_url"], "https://example.com/a.png");
    let bad_avatar = patch(
        &app,
        "/api/v1/me",
        json!({ "avatar_url": "http://khong-https.com/a.png" }),
        Some(&s.access),
    )
    .await;
    assert_eq!(bad_avatar.status, StatusCode::BAD_REQUEST);

    // Mật khẩu băm bằng argon2id; refresh token chỉ lưu dạng băm.
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
        .bind(s.user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"), "hash = {hash}");
    let raw_stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM refresh_tokens WHERE token_hash = $1")
            .bind(&s.refresh)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(raw_stored, 0, "không được lưu refresh token dạng thô");
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn xoay_vong_refresh_token(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "xoay@example.com").await;

    let r1 = refresh_with(&app, &s.refresh).await;
    assert_eq!(r1.status, StatusCode::OK);
    let new_refresh = refresh_cookie_from(&r1.headers).expect("cookie mới");
    assert_ne!(new_refresh, s.refresh, "refresh token phải được đổi mỗi lần");
    let new_access = r1.body["access_token"].as_str().unwrap();
    assert_eq!(
        get(&app, "/api/v1/me", Some(new_access)).await.status,
        StatusCode::OK
    );

    // Thiếu header chống CSRF thì bị chặn; thiếu cookie thì 401.
    let cookie = format!("refresh_token={new_refresh}");
    let no_csrf = send(
        &app,
        request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            None,
            &[("cookie", cookie.as_str())],
        ),
    )
    .await;
    assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
    let no_cookie = send(
        &app,
        request(
            Method::POST,
            "/api/v1/auth/refresh",
            None,
            None,
            &[("x-requested-with", "test")],
        ),
    )
    .await;
    assert_eq!(no_cookie.status, StatusCode::UNAUTHORIZED);

    // Cookie có HttpOnly và SameSite=Lax.
    let set_cookie = r1
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join(";");
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn dung_lai_refresh_token_cu_thu_hoi_ca_ho(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "danh.cap@example.com").await;

    let r1 = refresh_with(&app, &s.refresh).await;
    assert_eq!(r1.status, StatusCode::OK);
    let token_b = refresh_cookie_from(&r1.headers).unwrap();

    // Kẻ gian dùng lại token A (đã xoay vòng).
    let reuse = refresh_with(&app, &s.refresh).await;
    assert_eq!(reuse.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reuse.body["error"]["code"], "refresh_reused");

    // Token B hợp lệ trước đó cũng bị thu hồi.
    let after = refresh_with(&app, &token_b).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);

    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM refresh_tokens WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(s.user_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(active, 0, "cả họ token phải bị thu hồi");
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn dang_xuat_thu_hoi_token(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "thoat@example.com").await;
    let cookie = format!("refresh_token={}", s.refresh);
    let out = send(
        &app,
        request(
            Method::POST,
            "/api/v1/auth/logout",
            None,
            None,
            &[("cookie", cookie.as_str())],
        ),
    )
    .await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let cleared = out.headers.get("set-cookie").unwrap().to_str().unwrap();
    assert!(cleared.contains("Max-Age=0"));
    assert_eq!(
        refresh_with(&app, &s.refresh).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn phan_quyen_settings_va_audit(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let buyer = register(&app, "mua@example.com").await;
    let admin = register_admin(&app, &pool, "admin@example.com").await;

    assert_eq!(
        get(&app, "/api/v1/admin/settings", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&app, "/api/v1/admin/settings", Some(&buyer.access))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let denied = patch(
        &app,
        "/api/v1/admin/settings",
        json!({ "settings": { "stall_price": 1 } }),
        Some(&buyer.access),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let all = get(&app, "/api/v1/admin/settings", Some(&admin.access)).await;
    assert_eq!(all.status, StatusCode::OK);
    assert_eq!(all.body["settings"]["stall_price"], 99000);
    assert_eq!(all.body["settings"]["withdraw_fee_bps"], 1000);
    assert_eq!(all.body["settings"]["auto_approve_trusted"], false);

    let upd = patch(
        &app,
        "/api/v1/admin/settings",
        json!({ "settings": { "stall_price": 120000 } }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(upd.status, StatusCode::OK);
    assert_eq!(upd.body["settings"]["stall_price"], 120000);

    let rows = sqlx::query_as::<
        _,
        (
            String,
            Option<uuid::Uuid>,
            Option<serde_json::Value>,
            Option<serde_json::Value>,
        ),
    >("SELECT action, actor_id, before, after FROM audit_log")
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "settings.update");
    assert_eq!(rows[0].1, Some(admin.user_id));
    assert_eq!(rows[0].2, Some(json!(99000)));
    assert_eq!(rows[0].3, Some(json!(120000)));

    // Gửi lại cùng giá trị thì không ghi thêm audit.
    let same = patch(
        &app,
        "/api/v1/admin/settings",
        json!({ "settings": { "stall_price": 120000 } }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(same.status, StatusCode::OK);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // Giá trị sai hoặc khóa lạ bị từ chối, không đổi gì.
    for (body, code) in [
        (json!({ "withdraw_fee_bps": 20000 }), "invalid_setting"),
        (json!({ "stall_days": -3 }), "invalid_setting"),
        (json!({ "auto_approve_trusted": "yes" }), "invalid_setting"),
        (json!({ "khong_co_khoa_nay": 1 }), "unknown_setting"),
    ] {
        let r = patch(
            &app,
            "/api/v1/admin/settings",
            json!({ "settings": body }),
            Some(&admin.access),
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{:?}", r.body);
        assert_eq!(r.body["error"]["code"], code);
    }
    let fee: serde_json::Value =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'withdraw_fee_bps'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(fee, json!(1000));
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn audit_log_khong_sua_khong_xoa_duoc(pool: PgPool) {
    sqlx::query("INSERT INTO audit_log (action, target_type) VALUES ('thu', 'thu')")
        .execute(&pool)
        .await
        .unwrap();

    let upd = sqlx::query("UPDATE audit_log SET action = 'sua'")
        .execute(&pool)
        .await;
    let err = upd.expect_err("UPDATE phải bị chặn");
    assert!(err.to_string().contains("append-only"), "{err}");

    let del = sqlx::query("DELETE FROM audit_log").execute(&pool).await;
    assert!(del.is_err(), "DELETE phải bị chặn");

    let trunc = sqlx::query("TRUNCATE audit_log").execute(&pool).await;
    assert!(trunc.is_err(), "TRUNCATE phải bị chặn");

    let action: String = sqlx::query_scalar("SELECT action FROM audit_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(action, "thu");
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn hang_doi_job(pool: PgPool) {
    let (_, state) = test_app(pool.clone());
    server::jobs::enqueue(&pool, "noop", json!({}), Utc::now(), None)
        .await
        .unwrap();
    server::jobs::enqueue(&pool, "khong_ton_tai", json!({}), Utc::now(), None)
        .await
        .unwrap();
    // Cùng dedupe_key chỉ xếp được một job đang chờ.
    for _ in 0..3 {
        server::jobs::enqueue(&pool, "noop", json!({}), Utc::now() + chrono::Duration::hours(1), Some("k"))
            .await
            .unwrap();
    }
    let queued_k: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE dedupe_key = 'k' AND status = 'queued'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(queued_k, 1);

    let n = server::jobs::run_due_jobs(&state).await.unwrap();
    assert_eq!(n, 2);

    let noop: (String, i32) = sqlx::query_as("SELECT status, attempts FROM jobs WHERE kind = 'noop' AND dedupe_key IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(noop, ("done".to_string(), 1));

    let bad: (String, i32, Option<String>, bool) = sqlx::query_as(
        "SELECT status, attempts, last_error, run_at > now() FROM jobs WHERE kind = 'khong_ton_tai'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bad.0, "queued");
    assert_eq!(bad.1, 1);
    assert!(bad.2.is_some());
    assert!(bad.3, "job lỗi phải được hẹn chạy lại sau");

    // Chạy lại ngay thì không còn việc đến hạn.
    assert_eq!(server::jobs::run_due_jobs(&state).await.unwrap(), 0);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn health_va_ready(pool: PgPool) {
    let (app, _) = test_app(pool);
    for uri in ["/healthz", "/readyz", "/api/v1/healthz", "/api/v1/readyz"] {
        assert_eq!(get(&app, uri, None).await.status, StatusCode::OK, "{uri}");
    }
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn seed_chay_lai_duoc(pool: PgPool) {
    let (app, state) = test_app(pool.clone());
    server::seed::run(&state).await.unwrap();
    server::seed::run(&state).await.unwrap();

    let admin = login(&app, "admin@demo.local", "Admin@12345").await;
    assert_eq!(admin.status, StatusCode::OK);
    assert!(admin.body["user"]["roles"]
        .as_array()
        .unwrap()
        .contains(&json!("admin")));
    let reviewer = login(&app, "reviewer@demo.local", "Reviewer@12345").await;
    assert!(reviewer.body["user"]["roles"]
        .as_array()
        .unwrap()
        .contains(&json!("reviewer")));
    let buyer = login(&app, "buyer@demo.local", "Buyer@12345").await;
    assert_eq!(buyer.body["user"]["roles"], json!(["buyer"]));

    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 4);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn gioi_han_toc_do_dang_nhap(pool: PgPool) {
    let mut cfg = Config::for_tests();
    cfg.rate_limit_auth_per_min = 2;
    let state = AppState::new(pool, cfg).unwrap();
    let app = server::api::router(state);

    for _ in 0..2 {
        let r = login(&app, "a@example.com", "x").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let blocked = login(&app, "a@example.com", "x").await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(blocked.body["error"]["code"], "rate_limited");
    // Các đường dẫn khác không bị ảnh hưởng.
    assert_eq!(get(&app, "/healthz", None).await.status, StatusCode::OK);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn cors_chi_cho_origin_cau_hinh(pool: PgPool) {
    let (app, _) = test_app(pool);
    let preflight = |origin: &str| {
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/api/v1/me")
            .header("origin", origin)
            .header("access-control-request-method", "GET")
            .header("access-control-request-headers", "authorization")
            .body(Body::empty())
            .unwrap()
    };
    let ok = send(&app, preflight("http://localhost:5173")).await;
    assert_eq!(
        ok.headers.get("access-control-allow-origin").unwrap(),
        "http://localhost:5173"
    );
    assert_eq!(
        ok.headers.get("access-control-allow-credentials").unwrap(),
        "true"
    );
    let bad = send(&app, preflight("https://ke-xau.example")).await;
    assert!(bad.headers.get("access-control-allow-origin").is_none());
}
