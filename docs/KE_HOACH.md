# Kế hoạch kỹ thuật: Chợ cộng đồng sản phẩm số

Đã được chủ dự án xác nhận ngày 14/11/2026. Đi kèm `PROMPT.md` (gốc repo) và `PHASE_1_2.md` (phạm vi giao việc đợt này).

## 1. Quyết định đã chốt

### Từ bản PROMPT.md mới

1. **Duyệt thủ công:** test tay mọi sản phẩm và mọi phiên bản, không ngoại lệ. Tham số `auto_approve_trusted` mặc định tắt, chỉ để dành cho người bán uy tín về sau.
2. **Giữ tiền:** hai tham số riêng, `auto_complete_days = 3` (escrow → `earnings_pending`) và `hold_days = 3` (→ `earnings_available`). Tiền về ví rút được sau tối đa 6 ngày.
3. **Tách ví:** `wallet_spendable` (tiền nạp, chỉ để mua hàng, không rút được), `earnings_pending` và `earnings_available` (doanh thu, rút được). Tiền nạp thừa muốn lấy lại là ngoại lệ do admin xử lý thủ công, có log.
4. **Hoàn tiền khi tiền đã rút:** chỉ hoàn tự động khi tiền còn ở escrow hoặc `earnings_pending`. Nếu người bán đã rút rồi thì ghi công nợ, chặn rút khi còn nợ, làm ở phase sau.

### Xác nhận thêm sau khi xem kế hoạch

- **Thuê gian hàng:** người bán chọn nguồn `wallet_spendable` hoặc `earnings_available`, mỗi lần một nguồn, nguồn đó phải đủ 99.000đ, không trộn hai nguồn. Giao diện chọn sẵn nguồn đủ tiền, ưu tiên `wallet_spendable`.
- **Mua hàng:** chỉ trừ `wallet_spendable`. Người bán không dùng doanh thu để mua hàng, muốn dùng thì phải rút trước (mất 10% phí).
- **Công nợ người bán:** nằm ở tài khoản ledger riêng `seller_debt`, không để `earnings_available` âm. Phase 2 chỉ chừa loại tài khoản này và chặn rút khi nợ lớn hơn 0.

### Đã giải quyết theo mục 0 của PROMPT.md

- Rút tiền chỉ do admin duyệt tay. Bỏ cụm "hoặc tự động theo hạn mức" ở mục 3.5.
- Bốn vai: `buyer`, `seller`, `reviewer`, `admin` (mục 2 chỉ nêu ba vai).
- Tab Ví chứa thống kê, không tách tab (mục 9 ghi "Ví/Thống kê").
- Giá thuê 99.000đ cho 30 ngày, nằm trong `settings`.

### Mặc định khác (làm theo trừ khi được sửa)

- **Làm tròn phí rút:** `fee = (amount × fee_bps + 5000) / 10000` bằng số nguyên, `fee_bps = 1000`, `net = amount − fee`. Tiền luôn là `i64`, tỷ lệ phí lưu dạng basis point, không có float.
- **Đơn hàng:** một đơn = một sản phẩm của một người bán, không có giỏ hàng. Mỗi người mua chỉ mua một sản phẩm một lần, bản cập nhật tải miễn phí. Bảng `order_items` vẫn có để mở rộng sau.
- **Gian hàng hết hạn:** sản phẩm bị ẩn khỏi sàn, người đã mua vẫn tải được, đơn đang mở vẫn xử lý bình thường.
- **Hoàn tiền về người mua** luôn vào `wallet_spendable`.
- **Test hoàn tiền ở phase 2** chạy ở tầng ledger (escrow → người mua). Test trọn đơn hàng làm ở phase 3, vì phase 2 chưa có đơn.
- **Phase 2 có trang admin tối giản** (chưa style) để xác nhận nạp tiền thủ công, duyệt lệnh rút, duyệt KYC và hoàn tiền nạp thừa.
- **Phase 4:** tab Feed và Chat hiện trạng thái "Sắp có" cho tới phase 6, không ẩn tab.
- **Email:** bản đầu không bắt xác minh email, nhắc hết hạn thuê là thông báo trong app. Gửi email (quên mật khẩu, nhắc hạn) làm sau khi chọn nhà cung cấp.
- **KYC:** người bán tự khai họ tên và số tài khoản, admin đối chiếu khi duyệt lệnh rút đầu tiên.

## 2. Quyết định kỹ thuật

- **Backend:** Rust, Axum, Tokio, SQLx. Workspace có crate `ledger` (lõi tiền thuần, có property test, không phụ thuộc HTTP) và crate `server` (API, WebSocket, worker).
- **Job nền:** bảng `jobs` trong Postgres, lấy việc bằng `FOR UPDATE SKIP LOCKED`, chạy cùng binary với cờ `--worker`. Không cần Redis.
- **Realtime:** WebSocket của Axum, kết nối bằng ticket ngắn hạn. Khi chạy nhiều máy Fly thì phát tin qua Postgres `LISTEN/NOTIFY`.
- **Auth:** argon2id, access JWT 15 phút, refresh token 30 ngày xoay vòng và phát hiện dùng lại token cũ. Trên web, refresh token nằm trong cookie httpOnly của `api.<tên miền>` (cùng site với web nên dùng SameSite=Lax) kèm header chống CSRF cho `/auth/refresh`. Trên Tauri, refresh token lưu qua `platform.ts` (secure storage). Access token chỉ giữ trong bộ nhớ.
- **Upload:** người bán gửi file qua backend (stream, giới hạn dung lượng, allowlist đuôi + MIME, tính SHA-256) rồi lên R2 bucket private. Đi qua backend để ép giới hạn chặt hơn so với presigned PUT.
- **Tải file:** `POST /orders/:id/downloads` kiểm tra quyền và số lượt, ghi `download_logs`, trả signed URL sống 2 phút. Không bao giờ lộ URL gốc.
- **Quét file:** trait `Scanner` (ClamAV qua clamd + quét chuỗi đáng ngờ). ClamAV cần khoảng 1–2 GB RAM nên chạy máy riêng khi deploy. Phase 3 chạy trong docker-compose, phase 4 triển khai máy riêng.
- **SEO:** SPA thuần không có Open Graph cho crawler, nên dùng Cloudflare Pages Function chèn title/meta/OG cho `/p/:id` từ dữ liệu API. `sitemap.xml` do backend sinh.
- **Thanh toán:** trait `PaymentGateway` với ba adapter: PayOS (xác thực chữ ký webhook, idempotent theo mã lệnh nạp), chuyển khoản thủ công (admin xác nhận), và adapter giả cho test.
- **Mã hóa thông tin ngân hàng:** AES-256-GCM ở tầng ứng dụng, khóa trong secret, lưu kèm `key_id` để xoay khóa.
- **CORS:** chỉ origin của web và origin Tauri Android. Kiểm tra giá trị origin thực tế khi đóng gói ở phase 7.
- **Deploy:** Cloudflare Pages (web), Fly.io hoặc Railway (Docker), Neon hoặc Supabase (Postgres, ưu tiên vùng Singapore), Cloudflare R2. Chốt nhà cung cấp Postgres ở phase 4 theo giá và độ trễ.
- **Bảo mật chung (mục 10):** validate input bằng `serde` + `validator`, rate limit theo IP và theo user, sanitize markdown (`ammonia` ở backend, DOMPurify ở web), log có cấu trúc và che dữ liệu nhạy cảm, `audit_log` append-only cho mọi thao tác admin và tiền.

## 3. Cấu trúc thư mục

```
cho-cong-dong/
├─ README.md, PROMPT.md, justfile, docker-compose.yml, .env.example
├─ .github/workflows/   ci.yml, android.yml, deploy.yml
├─ backend/
│  ├─ Cargo.toml        (workspace)
│  ├─ migrations/       sqlx migrate, đánh version 0001_...
│  ├─ crates/
│  │  ├─ ledger/        lõi sổ cái, phí, bất biến, property test
│  │  └─ server/        src/{api,domain,infra,jobs,ws}
│  ├─ seed/             dữ liệu demo
│  └─ Dockerfile
├─ web/                 React + TS + Tailwind + Vite + React Query
│  ├─ src/{app,pages,features,components,lib}
│  │                    lib/api.ts, lib/platform.ts
│  ├─ functions/        Cloudflare Pages Functions (OG meta)
│  ├─ src-tauri/        Tauri 2, Android
│  └─ public/
└─ docs/                KE_HOACH.md, PHASE_1_2.md, điều khoản, hướng dẫn deploy
```

## 4. Sổ cái và luồng tiền

**Cấu trúc**

- Loại tài khoản: `wallet_spendable`, `earnings_pending`, `earnings_available` (ba loại này theo từng người dùng), `escrow` (mỗi đơn một tài khoản), `withdrawal_hold`, `platform_revenue`, `seller_debt` (chỉ chừa chỗ), `external`.
- `ledger_transactions(id, kind, idempotency_key unique, ref_type, ref_id)` và `ledger_entries(id, tx_id, account_id, amount i64 có dấu)`. Bút toán bất biến (trigger chặn UPDATE/DELETE). Constraint trigger kiểm tra tổng `amount` của mỗi giao dịch bằng 0.
- `account_balances(account_id, balance, version)` cập nhật trong cùng DB transaction, khóa bằng `SELECT ... FOR UPDATE` theo thứ tự id tăng dần để tránh deadlock. `CHECK (balance >= 0)` cho ví người dùng, escrow và `seller_debt`. `external` được phép âm. Job đối soát hằng giờ so `balance` với tổng `entries`.

**Bút toán** (số dương là cộng vào tài khoản)

1. Nạp X: `external −X`, `buyer.wallet_spendable +X`. Key `deposit:{id}`, nên webhook gọi lặp không cộng hai lần.
2. Mua giá P: `buyer.wallet_spendable −P`, `escrow(order) +P`. Đơn chuyển `paid`, file mở tải.
3. Hoàn tất (người mua xác nhận hoặc sau `auto_complete_days`): `escrow −P`, `seller.earnings_pending +P`. Sau `hold_days`: `earnings_pending −P`, `earnings_available +P`.
4. Hoàn tiền: `escrow −P` (hoặc `seller.earnings_pending −P`) rồi `buyer.wallet_spendable +P`.
5. Tạo lệnh rút X (≥ `withdraw_min`, ≤ `earnings_available`): `earnings_available −X`, `withdrawal_hold +X`. Khóa tiền ngay khi tạo lệnh, nếu không hai lệnh đồng thời có thể vượt số dư.
6. Duyệt lệnh rút: `withdrawal_hold −X`, `platform_revenue +fee`, `external +net`. Từ chối: `withdrawal_hold −X`, `earnings_available +X`.
7. Thuê gian hàng (nguồn `wallet_spendable` hoặc `earnings_available`): `<nguồn> −99.000`, `platform_revenue +99.000`, `stall_until = max(now, stall_until) + 30 ngày`.
8. Hoàn tiền nạp thừa (ngoại lệ, chỉ admin, bắt buộc có lý do và `audit_log`): `wallet_spendable −X`, `external +X`. Không thu phí.

**Bộ test bắt buộc (phase 2)**

- Làm tròn phí: `fee + net = amount` với mọi số (kèm ca lẻ như 1.000.005đ), `0 ≤ fee ≤ amount`.
- Rút đúng bằng `earnings_available` thì được. Rút dưới mức tối thiểu hoặc quá số dư thì bị chặn.
- Tiền nạp không rút được: người chỉ có `wallet_spendable` tạo lệnh rút bị từ chối, dù số dư lớn.
- Hai lệnh rút đồng thời không làm `earnings_available` âm.
- Webhook gọi lặp không cộng thêm, webhook sai chữ ký bị từ chối.
- Hoàn tiền từ escrow và từ `earnings_pending` về `wallet_spendable`. Khi tiền đã sang `earnings_available` thì hoàn tự động bị từ chối.
- Thuê gian hàng trả từ từng nguồn. Nguồn đã chọn thiếu tiền thì bị từ chối dù nguồn kia còn đủ.
- Hoàn tiền nạp thừa chỉ admin làm được và luôn có `audit_log`.
- Property test: chuỗi thao tác ngẫu nhiên thì mọi giao dịch tổng bằng 0, tổng toàn sổ bằng 0, `balance` bằng tổng `entries`, không ví nào âm.

## 5. Schema DB

Cột chính của từng bảng. Bảng đánh dấu (+) là bảng bổ sung ngoài danh sách tối thiểu ở mục 0.

**Tài khoản và cấu hình**

- `users`: id (uuid), email (citext, unique), password_hash, display_name, avatar_url, status, created_at
- `roles`: id, name. `user_roles` (+): user_id, role_id. `seller` được cấp khi thuê gian hàng lần đầu.
- `refresh_tokens` (+): id, user_id, token_hash, family_id, expires_at, revoked_at
- `seller_kyc` (+): user_id, full_name, bank_name, account_no_enc, key_id, status, reviewed_by
- `settings`: key (pk), value (jsonb), updated_by, updated_at. Gồm `stall_price`, `stall_days`, `withdraw_min`, `withdraw_fee_bps`, `hold_days`, `auto_complete_days`, `auto_approve_trusted` (mặc định tắt), `max_upload_mb`, `download_limit`, `file_ext_allowlist`.

**Gian hàng và sản phẩm**

- `shops`: id, owner_id (unique), name, slug, bio, avatar_url, stall_until, rating_avg
- `stall_rentals`: id, shop_id, ledger_tx_id, price, days, starts_at, ends_at
- `categories` (+): id, slug, name, icon, risk_level (normal/high)
- `products`: id, shop_id, category_id, title, slug, price, description_md, requirements, status (draft/active/taken_down), published_version_id, sales_count, rating_avg
- `product_versions`: id, product_id, version, changelog, status (draft/pending_review/testing/approved/rejected), reject_reason, submitted_at, reviewed_by. Trạng thái duyệt nằm ở phiên bản, nên bản cũ giữ nguyên bán khi bản mới đang chờ duyệt.
- `product_files`: id, version_id, object_key, filename, size, mime, sha256, scan_status
- `product_media` (+): id, product_id, kind, url, position
- `review_reports`: id, version_id, kind (auto/manual), result (jsonb), checklist (jsonb), reviewer_id

**Đơn hàng và tiền**

- `orders`: id, buyer_id, seller_id, total, status (paid/completed/disputed/refunded), offer_id, paid_at, auto_complete_at, completed_at, release_at
- `order_items`: id, order_id, product_id, version_id, price
- `download_logs` (+): id, order_id, file_id, ip_hash, created_at
- `ledger_accounts`, `ledger_transactions` (+), `ledger_entries`, `account_balances` (+): xem mục 4
- `deposits`: id, user_id, amount, gateway, gateway_ref, transfer_code, status
- `withdrawals`: id, user_id, amount, fee, net, status (pending/approved/rejected), bank_txn_ref, reviewed_by, reason
- `disputes`: id, order_id, opened_by, reason, evidence (jsonb), status, resolution (refund/release), resolved_by
- `reviews`: id, order_id (unique), product_id, buyer_id, rating, body

**Cộng đồng và chat**

- `posts`: id, author_id, body, images, product_id, category_tag, status, like_count, comment_count
- `comments`: id, post_id, author_id, parent_id (chỉ lồng 1 cấp), body
- `likes`: (user_id, post_id). `follows`: follower_id, target_type (user/shop), target_id
- `conversations`: id, user_a, user_b (unique theo cặp), last_message_at
- `messages`: id, conversation_id, sender_id, body, image_url, seen_at, flagged
- `offers`: id, conversation_id, seller_id, buyer_id, product_id, price, note, status, order_id
- `reports`: id, reporter_id, target_type, target_id, reason, status

**Vận hành**

- `audit_log`: id, actor_id, action, target_type, target_id, before, after (jsonb), ip, created_at. Append-only.
- `jobs` (+): id, kind, payload, run_at, attempts, status. `notifications` (+): id, user_id, kind, body, read_at.

## 6. Danh sách API

Tiền tố `/api/v1`. Phase ghi trong ngoặc khi không phải phase 1–4.

- **Auth và hồ sơ:** `POST /auth/register`, `/auth/login`, `/auth/refresh`, `/auth/logout`. `GET/PATCH /me`. `POST /me/kyc`.
- **Danh mục công khai:** `GET /categories`, `GET /products?cat&q&sort&cursor`, `GET /products/:id`, `GET /products/:id/reviews`, `GET /shops/:id`, `GET /sitemap.xml`.
- **Người bán:** `POST /shops`, `POST /stall/rent` (body có `source`: `spendable` hoặc `earnings`), `GET /seller/products`, `POST /products`, `PATCH /products/:id`, `POST /products/:id/versions`, `POST /versions/:id/files` (multipart), `POST /versions/:id/submit`.
- **Duyệt sản phẩm (reviewer/admin):** `GET /review/queue`, `POST /review/versions/:id/claim`, `POST /review/versions/:id/checklist`, `POST /review/versions/:id/decision`.
- **Ví:** `GET /wallet` (trả ba số dư: `spendable`, `earnings_pending`, `earnings_available`), `GET /wallet/entries?cursor`, `POST /deposits`, `POST /webhooks/payos`, `POST /withdrawals/preview`, `POST /withdrawals`, `GET /withdrawals`.
- **Đơn hàng:** `POST /orders` (body có `product_id` hoặc `offer_id`, kèm idempotency key), `GET /orders`, `GET /orders/:id`, `POST /orders/:id/confirm`, `POST /orders/:id/downloads`, `POST /orders/:id/disputes`, `POST /products/:id/reviews`.
- **Admin:** `GET/PATCH /admin/settings`, `GET /admin/deposits` + `POST .../:id/confirm`, `POST /admin/users/:id/refund-deposit` (hoàn tiền nạp thừa, bắt buộc có lý do), `GET /admin/withdrawals` + `POST .../:id/approve` (kèm mã giao dịch ngân hàng) và `/reject`, `GET /admin/disputes` + `POST .../:id/resolve`, `POST /admin/products/:id/takedown`, `POST /admin/users/:id/ban`, `GET /admin/audit-log`, `GET /admin/reports`.
- **Thống kê (phase 5):** `GET /stats/buyer`, `GET /stats/seller`, `GET /admin/stats`, `GET /admin/export/ledger.csv`, `GET/PUT /me/budget`.
- **Feed (phase 6):** `GET /feed?tab&cursor`, `POST /posts`, `POST /posts/:id/like`, `GET/POST /posts/:id/comments`, `POST /follows`, `POST /reports`.
- **Chat (phase 6):** `GET/POST /conversations`, `GET /conversations/:id/messages`, `POST /offers`, `POST /offers/:id/accept`, WebSocket `/ws`.
- **Vận hành:** `GET /healthz`, `GET /readyz`.

## 7. Phase và nghiệm thu

Giữ nguyên 7 phase ở mục 11 của PROMPT.md, cộng điều chỉnh nhỏ ở mục 1 (trang admin tối giản ở phase 2, tab "Sắp có" ở phase 4). Mỗi phase commit riêng và đạt: build sạch, `cargo clippy` không cảnh báo, test pass, README có hướng dẫn chạy thử. README ghi rõ chủ sàn tự kiểm tra nghĩa vụ pháp lý và thuế khi thu tiền hộ, không phải tư vấn pháp lý.

Đợt giao việc hiện tại chỉ gồm phase 1 và 2. Chi tiết ở `PHASE_1_2.md`.
