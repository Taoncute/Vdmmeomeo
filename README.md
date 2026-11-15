# Chợ cộng đồng sản phẩm số

Backend Rust (Axum + SQLx + Postgres) và web React. Tài liệu giao việc: [`PROMPT.md`](PROMPT.md),
[`docs/KE_HOACH.md`](docs/KE_HOACH.md), [`docs/PHASE_1_2.md`](docs/PHASE_1_2.md).

> **Pháp lý:** chủ sàn tự kiểm tra nghĩa vụ pháp lý và thuế khi thu tiền hộ. README và mã nguồn này không phải tư vấn pháp lý.

## Phase 1 (nền tảng): đã có

- Auth: đăng ký, đăng nhập, refresh token xoay vòng (phát hiện dùng lại thì thu hồi cả họ token), đăng xuất, `GET/PATCH /me`.
  Mật khẩu argon2id, access JWT 15 phút, refresh token 30 ngày trong cookie httpOnly (`SameSite=Lax`) + header chống CSRF `X-Requested-With` cho `/auth/refresh`.
- 4 vai trò: `buyer`, `seller`, `reviewer`, `admin`. Người mới đăng ký có `buyer`.
- `settings` có kiểu, `GET/PATCH /api/v1/admin/settings`, mọi thay đổi ghi `audit_log` (append-only, có trigger chặn UPDATE/DELETE/TRUNCATE).
- Rate limit theo IP và theo user, CORS theo cấu hình, validate input, log có cấu trúc, `GET /healthz`, `GET /readyz`.
- Hàng đợi job trong Postgres (`FOR UPDATE SKIP LOCKED`), chạy bằng `--worker`.

## Phase 2 (sổ cái, ví, nạp/rút, gian hàng): đã có

- **Sổ cái bất biến** (`crates/ledger`): tiền là `i64` đồng; 8 loại tài khoản (`wallet_spendable`, `earnings_pending`, `earnings_available`, `escrow`, `withdrawal_hold`, `platform_revenue`, `seller_debt`, `external`).
  Mỗi giao dịch có `idempotency_key` duy nhất. Postgres tự chặn: giao dịch tổng khác 0 (trigger kiểm lúc COMMIT), sửa/xóa/TRUNCATE bút toán, số dư âm (trừ `external`).
  Ghi sổ khóa số dư theo thứ tự id tăng dần để không deadlock; `verify_books` đối soát toàn sổ (job `ledger_reconcile` chạy hằng giờ).
- **Ví**: `GET /wallet` (spendable, earnings_pending, earnings_available + withdrawal_hold, seller_debt), `GET /wallet/entries` (phân trang `before_id`).
- **Nạp tiền**: `POST /deposits`, adapter `PaymentGateway`: `payos`, `manual` (chuyển khoản theo mã nội dung, admin xác nhận), `fake` (dev/test). `POST /webhooks/payos` xác thực chữ ký trên thân thô, gọi lặp hay đồng thời vẫn chỉ cộng một lần. Chọn cổng bằng `PAYMENT_GATEWAY`.
- **Rút tiền**: chỉ từ `earnings_available`; tối thiểu `withdraw_min`; phải có KYC đã duyệt và không còn công nợ. `POST /withdrawals/preview`, `POST /withdrawals` (header `Idempotency-Key`), admin duyệt (kèm mã giao dịch ngân hàng) hoặc từ chối (có lý do).
  Phí `fee = (amount × fee_bps + 5000) / 10000` bằng số nguyên, chốt lúc tạo lệnh.
- **KYC**: `POST/GET /me/kyc`, số tài khoản mã hóa AES-256-GCM (có `key_id` để xoay khóa), admin duyệt ở `/admin/kyc`.
- **Escrow và doanh thu** (hàm nghiệp vụ, dùng ở phase 3): `escrow::hold/complete/release_hold/release_due/refund_from_escrow/refund_from_pending`. Doanh thu giữ `hold_days` ngày rồi job `release_holds` nhả sang `earnings_available`; tiền đã nhả thì không hoàn tự động.
- **Thuê gian hàng**: `POST /stall/rent` (`source`: `spendable` | `earnings`; `shop_name` bắt buộc ở lần đầu, gia hạn thì bỏ qua), cấp vai `seller`, gia hạn cộng dồn. `PATCH /shops/me` sửa mô tả/logo. Job `rent_reminders` nhắc trước 3 ngày.
- **Admin**: hoàn tiền nạp thừa `POST /admin/users/:id/refund-deposit` (bắt buộc lý do, luôn ghi audit_log), xem `audit_log`, `GET /notifications` cho người dùng.
- Trang admin tối giản ở `web/` (đăng nhập, xác nhận nạp, duyệt rút, duyệt KYC, hoàn tiền nạp thừa), chưa style.

## Chạy local

Cần: Rust stable, Docker, Node 20.

```bash
cp .env.example .env            # chỉnh JWT_SECRET
docker compose up -d postgres   # Postgres 16 ở localhost:5432
cd backend
cargo run -p server             # API ở http://localhost:8080, tự chạy migration
cargo run -p server -- seed     # dữ liệu demo (chỉ dev)
cargo run -p server -- --worker # worker job nền (terminal khác)
```

Web: `cd web && npm install && npm run dev` (http://localhost:5173).

Tài khoản demo (chỉ dev, sau khi `seed`):

| Email | Mật khẩu | Vai trò |
| --- | --- | --- |
| admin@demo.local | `Admin@12345` | admin |
| reviewer@demo.local | `Reviewer@12345` | reviewer |
| buyer@demo.local | `Buyer@12345` | buyer |
| seller@demo.local | `Seller@12345` | buyer (thành seller khi thuê gian hàng) |

## Chạy test

Test chạy trên Postgres thật, không mock DB. `#[sqlx::test]` tự tạo một database riêng cho từng test từ `DATABASE_URL`, nên kết nối đó cần quyền tạo database (user `postgres` của docker-compose là đủ).

```bash
docker compose up -d postgres
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
cd backend
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## CI

`.github/workflows/ci.yml` chạy 4 job độc lập mỗi lần push: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (service Postgres 16), và build web. Xem kết quả ở tab **Actions** của repo.

## Cấu hình

Xem [`.env.example`](.env.example). `JWT_SECRET` bắt buộc, dài ít nhất 32 ký tự.
Khi chạy sau proxy (Fly, Cloudflare) đặt `TRUST_PROXY=true` để rate limit dùng IP thật, và `COOKIE_SECURE=true` khi chạy HTTPS.

## Quyết định thiết kế ở phase 2 (tài liệu chưa nói rõ, đã chọn mặc định)

- `withdrawal_hold` và `seller_debt` theo từng người dùng (tránh một dòng nóng dùng chung).
- Phí rút và số thực nhận chốt lúc tạo lệnh; đổi `withdraw_fee_bps` sau đó không ảnh hưởng lệnh đang chờ.
- KYC đã duyệt thì người dùng không tự sửa qua API (`kyc_approved_locked`), tránh đổi số tài khoản nhận tiền sau khi đã duyệt. Số tài khoản chỉ gồm chữ số, 6 đến 20.
- Admin thấy số tài khoản đầy đủ (đã giải mã) ở `/admin/kyc` và `/admin/withdrawals` vì cần để chuyển khoản.
- Trần một lần nạp là 1.000.000.000đ (chống nhập nhầm và tràn số); chưa có mức nạp tối thiểu vì tài liệu chưa quy định.
- Webhook: chữ ký sai trả 401; chữ ký đúng nhưng số tiền lệch lệnh nạp trả 422 và không cộng tiền; mã đơn không tồn tại (ví dụ webhook thử khi đăng ký URL) trả 200 và bỏ qua.
- `audit_log.ip` chỉ có ở thao tác admin và `settings`; thao tác tiền của người dùng ghi `actor_id` nhưng `ip` để trống.
- `earnings_holds` có thêm cột `status` (`pending`/`released`/`refunded`) để job nhả tiền không nhả khoản đã hoàn.
- Phần tạo link thanh toán PayOS (`POST /v2/payment-requests`) viết theo tài liệu nhưng **chưa thử với PayOS thật**; chỉ phần ký và verify webhook có test bằng dữ liệu mẫu của PayOS.
  Tài liệu PayOS tự mâu thuẫn: ví dụ JSON ghi chữ ký `8d8640d8…` không khớp dữ liệu đi kèm, ví dụ PHP (`412e915d…`) khớp thuật toán HMAC-SHA256 trên chuỗi `key=value` sắp xếp alphabet; code bám theo ví dụ PHP.

## Lệch so với tài liệu (có chủ đích)

- `users.email` là `text` + ràng buộc `email = lower(email)` thay vì `citext`; hành vi không phân biệt hoa thường giữ nguyên, tránh lỗi so sánh `citext` với tham số `text`.
- `shops.name_key` (tên chữ thường theo Unicode) đảm bảo tên gian hàng duy nhất không phân biệt hoa thường, thay cho `citext`.
- Refresh token chỉ qua cookie ở phase 1; chế độ lưu qua `platform.ts` (Tauri) để phase 7.
- `max_upload_mb`, `download_limit`, `file_ext_allowlist` chưa seed vào `settings` vì tài liệu chưa nêu giá trị; phase 3 sẽ thêm khi chủ dự án chốt.
