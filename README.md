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

## Lệch so với tài liệu (có chủ đích)

- `users.email` là `text` + ràng buộc `email = lower(email)` thay vì `citext`; hành vi không phân biệt hoa thường giữ nguyên, tránh lỗi so sánh `citext` với tham số `text`.
- Refresh token chỉ qua cookie ở phase 1; chế độ lưu qua `platform.ts` (Tauri) để phase 7.
- `max_upload_mb`, `download_limit`, `file_ext_allowlist` chưa seed vào `settings` vì tài liệu chưa nêu giá trị; phase 3 sẽ thêm khi chủ dự án chốt.
