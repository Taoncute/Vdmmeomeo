# Giao việc cho Claude Code: phase 1 và phase 2

Kế hoạch đã được chủ dự án xác nhận ngày 14/11/2026, không cần chờ xác nhận lại. Làm phase 1, commit riêng, rồi phase 2, commit riêng.

## Thứ tự ưu tiên khi tài liệu mâu thuẫn

1. File này.
2. `docs/KE_HOACH.md`.
3. Mục 0 của `PROMPT.md`.
4. Phần còn lại của `PROMPT.md`.

Những dòng cũ trong `PROMPT.md` bị thay thế:

- Mục 3.5: bỏ cụm "hoặc tự động theo hạn mức". Rút tiền chỉ do admin duyệt tay.
- Mục 2: có 4 vai (`buyer`, `seller`, `reviewer`, `admin`), không phải 3.
- Mục 3.6: thuê gian hàng chọn một nguồn `wallet_spendable` hoặc `earnings_available`, không trộn.
- Mục 3, danh sách tài khoản ledger: thêm `withdrawal_hold` và `seller_debt`.

Gặp mâu thuẫn hoặc chỗ thiếu mà các tài liệu trên chưa nói: dừng và hỏi, không tự đoán.

## Phase 1: Nền tảng

- Cấu trúc repo theo mục 3 của `KE_HOACH.md`. Chỉ dựng phần phase 1–2 cần: workspace `backend/` (crate `ledger` mới có khung, crate `server`), `web/` là khung Vite + React + TS + Tailwind với `lib/api.ts` và `lib/platform.ts` (stub), `docker-compose.yml` (Postgres), `.env.example`, `justfile`.
- Migration (sqlx, có version): `users`, `roles`, `user_roles`, `refresh_tokens`, `settings` (kèm seed giá trị mặc định), `audit_log` (trigger chặn UPDATE/DELETE), `jobs`.
- Auth: register, login, refresh, logout, `GET/PATCH /me`. Argon2id, access JWT 15 phút, refresh token xoay vòng có phát hiện dùng lại, refresh qua cookie httpOnly cho web. Thiết kế để chế độ lưu refresh token qua `platform.ts` của Tauri thêm được ở phase 7, chưa cần làm đủ bây giờ.
- Vai trò: người đăng ký mới có `buyer`. Extractor/middleware kiểm vai trò. Seed sẵn một `admin`, một `reviewer` và hai người dùng demo.
- Settings: getter có kiểu, `GET/PATCH /admin/settings`, mọi thay đổi ghi `audit_log`.
- Hạ tầng: rate limit theo IP và theo user, CORS chặt theo cấu hình, validate input, structured logging (tracing, không log dữ liệu nhạy cảm), `GET /healthz`, `GET /readyz`.
- CI `.github/workflows/ci.yml`: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` với Postgres service, và `npm ci && npm run build` cho `web/`.
- README: cách chạy local và chạy test.
- Test phase 1: đăng ký/đăng nhập, xoay vòng refresh token, dùng lại refresh token cũ bị thu hồi cả họ token, phân quyền settings, `audit_log` không sửa/xóa được.

## Phase 2: Ledger, ví, nạp/rút, thuê gian hàng

**Migration:** `shops` (đủ cho thuê gian hàng), `stall_rentals`, `seller_kyc`, `ledger_accounts`, `ledger_transactions`, `ledger_entries` (bất biến, constraint trigger tổng bằng 0), `account_balances` (có CHECK), `deposits`, `withdrawals`, `notifications`, và một bảng `earnings_holds` (id, seller_id, order_ref, amount, release_at, released_at) để job nhả tiền từ `earnings_pending` sang `earnings_available`. `escrow` có `order_id` nullable, chưa FK vì bảng `orders` thuộc phase 3. Được đổi tên bảng phụ nếu có thiết kế tốt hơn, miễn giữ đúng hành vi.

**Crate `ledger`:** kiểu tiền `i64`, hàm tính phí `fee = (amount × fee_bps + 5000) / 10000`, bộ dựng bút toán kiểm tổng bằng 0, các loại tài khoản. Không phụ thuộc HTTP.

**Hàm nghiệp vụ** (mỗi hàm một DB transaction, khóa theo thứ tự id tăng dần, có `idempotency_key`): xác nhận nạp, giữ tiền escrow, hoàn tất sang `earnings_pending`, nhả `earnings_pending` sang `earnings_available`, hoàn tiền từ escrow hoặc `earnings_pending`, tạo/duyệt/từ chối lệnh rút, thuê gian hàng (tham số `source`), admin hoàn tiền nạp thừa. Các hàm escrow nhận `order_id` và được test ở tầng ledger, chưa có luồng đơn hàng.

**Thanh toán:** trait `PaymentGateway` và ba adapter: PayOS, chuyển khoản thủ công theo mã nội dung, và adapter giả. Webhook `POST /webhooks/payos` xác thực chữ ký và idempotent. Đọc tài liệu PayOS hiện hành để làm đúng cách ký và định dạng webhook, không dựa vào trí nhớ. Chưa có tài khoản PayOS thật thì chạy bằng chế độ chuyển khoản thủ công và viết test chữ ký bằng dữ liệu mẫu.

**Quy tắc rút tiền:** phải có KYC đã duyệt, số tiền từ `withdraw_min` (1.000.000đ) đến `earnings_available`, `seller_debt` bằng 0, hiển thị trước phí và số thực nhận. Tiền khóa ngay khi tạo lệnh (`earnings_available → withdrawal_hold`).

**Thuê gian hàng:** lần đầu tạo `shops` và cấp vai `seller`. Cộng 30 ngày vào `stall_until`. Có job tạo thông báo trong app trước hạn 3 ngày, kèm `GET /notifications` tối giản.

**API:** theo mục 6 của `KE_HOACH.md` cho nhóm Ví, Người bán (`/stall/rent`), Admin (deposits, withdrawals, refund-deposit, audit-log). Bổ sung `GET /admin/kyc` và `POST /admin/kyc/:id/approve|reject`.

**Giao diện:** chỉ trang admin tối giản, chưa style: đăng nhập, xác nhận nạp, duyệt lệnh rút, duyệt KYC, hoàn tiền nạp thừa.

**Dữ liệu seed:** người dùng có số dư mẫu ở từng loại ví để thử tay.

## Test bắt buộc

Chạy với Postgres thật (docker-compose ở local, service trong CI), không mock DB. Bốn nhóm sau là điều kiện nghiệm thu quan trọng nhất:

1. **Ledger cân bằng.** Property test (`proptest`): chuỗi thao tác ngẫu nhiên thì mọi giao dịch có tổng bằng 0, tổng toàn sổ bằng 0, `balance` bằng tổng `entries`, không ví nào âm. Thêm test chứng minh DB từ chối giao dịch tổng khác 0 và từ chối UPDATE/DELETE bút toán.
2. **Rút đồng thời không âm ví.** Chạy nhiều tác vụ song song trên các kết nối riêng, mỗi lệnh riêng lẻ hợp lệ nhưng tổng vượt `earnings_available`. Chỉ số lệnh vừa đủ được tạo, các lệnh còn lại bị từ chối, số dư không bao giờ âm, ledger vẫn cân bằng.
3. **Webhook lặp.** Cùng một webhook hợp lệ gửi nhiều lần, tuần tự và đồng thời, chỉ tạo đúng một giao dịch ledger. Webhook sai chữ ký bị từ chối và không đổi gì trong ledger.
4. **Làm tròn phí.** Property test `fee + net = amount` và `0 ≤ fee ≤ amount` với mọi `amount` hợp lệ. Kèm các ca cố định: 1.000.000đ, 1.000.005đ, 1.000.004đ, số rất lớn không tràn `i64`.

Các test còn lại theo mục 4 của `KE_HOACH.md`: tiền nạp không rút được, rút dưới mức tối thiểu hoặc quá số dư, rút khi còn công nợ, hoàn tiền từ escrow và `earnings_pending`, hoàn tự động bị từ chối sau khi tiền đã sang `earnings_available`, thuê gian hàng từng nguồn và thiếu tiền ở nguồn đã chọn, hoàn tiền nạp thừa chỉ admin làm được và có `audit_log`.

## Nghiệm thu mỗi phase

- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` đều chạy thật và sạch.
- README có hướng dẫn chạy thử.
- Commit riêng cho từng phase.
- Báo cáo cuối mỗi phase ghi lệnh đã chạy và kết quả thật (số test pass, cảnh báo clippy nếu có). Thứ gì không chạy được thì nói rõ là chưa chạy, không báo "pass" suông.

## Không làm ở phase 1–2

Sản phẩm, upload và giao file, quét file, đơn hàng trọn luồng, tranh chấp, feed, chat, thống kê, giao diện kiểu chocode, Tauri, deploy.

## Chưa có, dùng mặc định

Nhà cung cấp email, tài khoản PayOS thật, tên miền, nhà cung cấp Postgres khi deploy. Không chặn phase 1–2.
