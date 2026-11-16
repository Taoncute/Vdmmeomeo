mod common;

use axum::http::{Method, StatusCode};
use common::*;
use ledger::AccountKey;
use serde_json::{json, Value};
use server::domain::refund;
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn moi_api_quan_tri_chan_nguoi_thuong_va_khach(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let buyer = register(&app, "mua@example.com").await;
    let id = Uuid::new_v4();
    let endpoints: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, "/api/v1/admin/settings".into(), None),
        (
            Method::PATCH,
            "/api/v1/admin/settings".into(),
            Some(json!({ "settings": { "stall_days": 5 } })),
        ),
        (Method::GET, "/api/v1/admin/deposits".into(), None),
        (
            Method::POST,
            format!("/api/v1/admin/deposits/{id}/confirm"),
            Some(json!({})),
        ),
        (Method::GET, "/api/v1/admin/withdrawals".into(), None),
        (
            Method::POST,
            format!("/api/v1/admin/withdrawals/{id}/approve"),
            Some(json!({ "bank_txn_ref": "x" })),
        ),
        (
            Method::POST,
            format!("/api/v1/admin/withdrawals/{id}/reject"),
            Some(json!({ "reason": "x" })),
        ),
        (Method::GET, "/api/v1/admin/kyc".into(), None),
        (
            Method::POST,
            format!("/api/v1/admin/kyc/{id}/approve"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/v1/admin/kyc/{id}/reject"),
            Some(json!({ "reason": "x" })),
        ),
        (
            Method::POST,
            format!("/api/v1/admin/users/{id}/refund-deposit"),
            Some(json!({ "amount": 1, "reason": "x" })),
        ),
        (Method::GET, "/api/v1/admin/audit-log".into(), None),
    ];
    for (method, uri, body) in endpoints {
        let guest = send(&app, request(method.clone(), &uri, body.clone(), None, &[])).await;
        assert_eq!(
            guest.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri} (khách)"
        );
        let user = send(
            &app,
            request(method.clone(), &uri, body, Some(&buyer.access), &[]),
        )
        .await;
        assert_eq!(
            user.status,
            StatusCode::FORBIDDEN,
            "{method} {uri} (người mua)"
        );
    }
    // Mọi lần bị chặn đều không để lại dấu vết thay đổi.
    assert_eq!(count(&pool, "SELECT count(*) FROM audit_log").await, 0);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn kyc_gui_duyet_tu_choi_va_ma_hoa(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let s = register(&app, "ban@example.com").await;
    let admin = register_admin(&app, &pool, "admin@example.com").await;
    let submit = |acc: &'static str| json!({ "full_name": "Trần Thị B", "bank_name": "Techcombank", "account_no": acc });

    assert_eq!(
        get(&app, "/api/v1/me/kyc", Some(&s.access)).await.body["kyc"],
        Value::Null
    );
    let r = post(
        &app,
        "/api/v1/me/kyc",
        submit("9876543210"),
        Some(&s.access),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["account_no_masked"], "******3210");

    // Số tài khoản không nằm ở dạng rõ trong DB.
    let (enc, key_id): (Vec<u8>, String) =
        sqlx::query_as("SELECT account_no_enc, key_id FROM seller_kyc WHERE user_id = $1")
            .bind(s.user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(key_id, "dev");
    assert!(!enc.windows(10).any(|w| w == b"9876543210"));

    // Từ chối cần lý do; từ chối xong thì người bán gửi lại được.
    let uri = |action: &str| format!("/api/v1/admin/kyc/{}/{action}", s.user_id);
    let no_reason = post(
        &app,
        &uri("reject"),
        json!({ "reason": "  " }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(no_reason.body["error"]["code"], "reason_required");
    let rej = post(
        &app,
        &uri("reject"),
        json!({ "reason": "Ảnh mờ" }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(rej.status, StatusCode::OK, "{:?}", rej.body);
    let mine = get(&app, "/api/v1/me/kyc", Some(&s.access)).await;
    assert_eq!(mine.body["kyc"]["status"], "rejected");
    assert_eq!(mine.body["kyc"]["reject_reason"], "Ảnh mờ");
    assert_eq!(
        post(
            &app,
            &uri("reject"),
            json!({ "reason": "x" }),
            Some(&admin.access)
        )
        .await
        .status,
        StatusCode::CONFLICT
    );

    let again = post(
        &app,
        "/api/v1/me/kyc",
        submit("9876543210"),
        Some(&s.access),
    )
    .await;
    assert_eq!(again.body["status"], "pending");
    assert_eq!(
        post(&app, &uri("approve"), json!({}), Some(&admin.access))
            .await
            .status,
        StatusCode::OK
    );

    // Đã duyệt thì người dùng không tự đổi tài khoản nhận tiền được.
    let locked = post(
        &app,
        "/api/v1/me/kyc",
        submit("1111222233"),
        Some(&s.access),
    )
    .await;
    assert_eq!(locked.status, StatusCode::CONFLICT);
    assert_eq!(locked.body["error"]["code"], "kyc_approved_locked");
    let still = get(&app, "/api/v1/me/kyc", Some(&s.access)).await;
    assert_eq!(still.body["kyc"]["account_no_masked"], "******3210");
    assert_eq!(
        post(&app, &uri("approve"), json!({}), Some(&admin.access))
            .await
            .body["error"]["code"],
        "kyc_not_pending"
    );

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE target_type = 'seller_kyc' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        actions,
        vec!["kyc.reject".to_string(), "kyc.approve".to_string()]
    );
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn hoan_tien_nap_thua_chi_admin_co_ly_do_va_audit(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let victim = register(&app, "nap-thua@example.com").await;
    let admin = register_admin(&app, &pool, "admin@example.com").await;
    let wallet = AccountKey::wallet_spendable(victim.user_id);
    fund(&pool, wallet, 100_000).await;
    let uri = format!("/api/v1/admin/users/{}/refund-deposit", victim.user_id);

    // Người thường (HTTP) và người không phải admin (gọi thẳng nghiệp vụ) đều bị chặn.
    let denied = post(
        &app,
        &uri,
        json!({ "amount": 10_000, "reason": "x" }),
        Some(&victim.access),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let direct = refund::refund_deposit(
        &pool,
        admin_ctx(victim.user_id),
        victim.user_id,
        10_000,
        "tự hoàn",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(direct.code(), "forbidden");
    assert_eq!(balance(&pool, wallet).await, 100_000);

    // Thiếu lý do, số tiền sai, quá số dư.
    for (body, status, code) in [
        (
            json!({ "amount": 10_000, "reason": "" }),
            StatusCode::BAD_REQUEST,
            "reason_required",
        ),
        (
            json!({ "amount": 0, "reason": "Nạp thừa" }),
            StatusCode::BAD_REQUEST,
            "invalid_amount",
        ),
        (
            json!({ "amount": 100_001, "reason": "Nạp thừa" }),
            StatusCode::UNPROCESSABLE_ENTITY,
            "insufficient_funds",
        ),
    ] {
        let r = post(&app, &uri, body, Some(&admin.access)).await;
        assert_eq!(r.status, status, "{:?}", r.body);
        assert_eq!(r.body["error"]["code"], code);
    }
    assert_eq!(balance(&pool, wallet).await, 100_000);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM audit_log WHERE action = 'deposit.refund'"
        )
        .await,
        0
    );

    let ok = post_with(
        &app,
        &uri,
        json!({ "amount": 30_000, "reason": "Nạp nhầm hai lần" }),
        Some(&admin.access),
        &[("idempotency-key", "hoan-1")],
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    assert_eq!(ok.body["replayed"], false);
    let replay = post_with(
        &app,
        &uri,
        json!({ "amount": 30_000, "reason": "Nạp nhầm hai lần" }),
        Some(&admin.access),
        &[("idempotency-key", "hoan-1")],
    )
    .await;
    assert_eq!(replay.body["replayed"], true);
    assert_eq!(replay.body["tx_id"], ok.body["tx_id"]);
    assert_eq!(balance(&pool, wallet).await, 70_000);

    let rows: Vec<(Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT actor_id, after->>'reason' FROM audit_log WHERE action = 'deposit.refund'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(Some(admin.user_id), Some("Nạp nhầm hai lần".to_string()))]
    );
    assert_books_clean(&pool).await;

    // Hoàn tiền chỉ lấy từ ví nạp, không đụng tới doanh thu.
    let seller = create_user(&pool, "ban@example.com").await;
    fund(&pool, AccountKey::earnings_available(seller), 1_000_000).await;
    let r = post(
        &app,
        &format!("/api/v1/admin/users/{seller}/refund-deposit"),
        json!({ "amount": 10_000, "reason": "thử" }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(r.body["error"]["code"], "insufficient_funds");
    assert_eq!(
        balance(&pool, AccountKey::earnings_available(seller)).await,
        1_000_000
    );
    // Người dùng không tồn tại.
    let missing = post(
        &app,
        &format!("/api/v1/admin/users/{}/refund-deposit", Uuid::new_v4()),
        json!({ "amount": 1, "reason": "thử" }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn lich_su_vi_phan_trang_va_chi_thay_cua_minh(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let me = register(&app, "toi@example.com").await;
    let other = create_user(&pool, "nguoi-khac@example.com").await;
    for amount in [1_000, 2_000, 3_000, 4_000, 5_000] {
        fund(&pool, AccountKey::wallet_spendable(me.user_id), amount).await;
    }
    fund(&pool, AccountKey::wallet_spendable(other), 777).await;

    let mut seen = Vec::new();
    let mut uri = "/api/v1/wallet/entries?limit=2".to_string();
    loop {
        let page = get(&app, &uri, Some(&me.access)).await;
        assert_eq!(page.status, StatusCode::OK);
        for e in page.body["entries"].as_array().unwrap() {
            seen.push((e["id"].as_i64().unwrap(), e["amount"].as_i64().unwrap()));
        }
        match page.body["next_before_id"].as_i64() {
            Some(next) => uri = format!("/api/v1/wallet/entries?limit=2&before_id={next}"),
            None => break,
        }
    }
    let amounts: Vec<i64> = seen.iter().map(|(_, a)| *a).collect();
    assert_eq!(
        amounts,
        vec![5_000, 4_000, 3_000, 2_000, 1_000],
        "mới nhất trước, không sót, không trùng"
    );
    assert!(seen.windows(2).all(|w| w[0].0 > w[1].0));
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn doc_audit_log_loc_theo_hanh_dong_va_phan_trang(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let admin = register_admin(&app, &pool, "admin@example.com").await;
    for days in [5, 6, 7] {
        let r = patch(
            &app,
            "/api/v1/admin/settings",
            json!({ "settings": { "stall_days": days } }),
            Some(&admin.access),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let all = get(
        &app,
        "/api/v1/admin/audit-log?action=settings.update",
        Some(&admin.access),
    )
    .await;
    let rows = all.body["audit_log"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["after"], 7, "mới nhất trước");
    assert_eq!(rows[0]["before"], 6);

    let first = get(&app, "/api/v1/admin/audit-log?limit=1", Some(&admin.access)).await;
    let last_id = first.body["audit_log"][0]["id"].as_i64().unwrap();
    let next = get(
        &app,
        &format!("/api/v1/admin/audit-log?limit=1&before_id={last_id}"),
        Some(&admin.access),
    )
    .await;
    assert_ne!(next.body["audit_log"][0]["id"].as_i64().unwrap(), last_id);

    let none = get(
        &app,
        "/api/v1/admin/audit-log?action=khong.co",
        Some(&admin.access),
    )
    .await;
    assert_eq!(none.body["audit_log"].as_array().unwrap().len(), 0);
}
