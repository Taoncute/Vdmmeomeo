# PROMPT CHO CLAUDE CODE — Chợ cộng đồng sản phẩm số (Rust + Tauri 2)

> Lưu ý: file này có **mục 0 đến mục 11**. Nếu bạn chỉ thấy một phần (ví dụ bắt đầu từ mục 10), nghĩa là nội dung bị cắt khi sao chép: hãy báo ngay và đọc file đầy đủ `PROMPT.md` trong repo thay vì làm tiếp.

Bạn là kỹ sư trưởng. Hãy dựng một chợ online cộng đồng cho tool/script/code/bot/website, chạy trên **web và app Android từ 1 codebase**. Trước khi code, đọc kỹ toàn bộ prompt, viết kế hoạch ngắn (cấu trúc thư mục, schema DB, danh sách API) rồi chờ tôi xác nhận. Sau đó làm **từng phase**, mỗi phase commit riêng, chạy test và báo lại.

File `cho-cong-dong.html` (đính kèm) là bản demo giao diện/logic ban đầu, chỉ dùng làm tham khảo. Bản demo đã bị đánh giá là chưa ổn ở: luồng tiền, giao file, trang chi tiết sản phẩm, giao diện, thống kê, feed và chat.

## 0. Quyết định đã chốt (đọc trước, không cần hỏi lại)
- **"chocode"** là tên một sàn tham khảo (chocode.com.vn), không phải thư viện UI. "Giao diện chocode" nghĩa là bám bố cục/phong cách của sàn đó theo mục 9.
- **Frontend:** React + TypeScript + Tailwind + Vite, dạng **SPA / static build** (để vừa deploy web vừa cho Tauri đóng gói).
- **Nạp tiền:** qua cổng thanh toán (PayOS) bằng webhook có xác thực chữ ký. Nếu chưa có tài khoản cổng, dùng chế độ dự phòng: người dùng chuyển khoản theo mã nội dung, admin xác nhận thủ công trong trang quản trị. Cả hai đi qua cùng một `PaymentGateway` trait.
- **Rút tiền:** bản đầu là **lệnh rút do admin duyệt rồi chuyển khoản thủ công**, ghi mã giao dịch ngân hàng vào lệnh. Không tự động hóa chi tiền ở phase đầu.
- **Giá thuê gian hàng:** 99.000đ/tháng, nằm ở mục 1 và bảng `settings`.
- **Nơi deploy:** web trên Cloudflare Pages, backend Rust đóng Docker chạy trên Fly.io (hoặc Railway), PostgreSQL trên Supabase/Neon, file trên Cloudflare R2. Nếu thấy phương án khác rẻ/hợp hơn, đề xuất trong kế hoạch.
- **Tab điều hướng dưới:** Trang chủ, Feed, Chat, **Ví** (thống kê là mục con trong Ví, không tách tab), Tài khoản.
- **Mô hình dữ liệu tối thiểu:** `users`, `roles`, `shops`, `stall_rentals`, `products`, `product_versions`, `product_files`, `review_reports`, `orders`, `order_items`, `ledger_accounts`, `ledger_entries`, `withdrawals`, `deposits`, `disputes`, `reviews`, `posts`, `comments`, `likes`, `follows`, `conversations`, `messages`, `offers`, `reports`, `settings`, `audit_log`. Thiết kế cột và quan hệ chi tiết trong bản kế hoạch.
- Phân quyền: `buyer`, `seller`, `reviewer` (chỉ duyệt/test sản phẩm), `admin`.

## 1. Mô hình kinh doanh
- Chủ sàn (admin) cho người bán **thuê gian hàng theo tháng** (mặc định 99.000đ/tháng, chỉnh được trong bảng settings).
- Người bán nhận **100% doanh thu đơn hàng** vào ví. Sàn thu **10% phí khi rút tiền** (phí rút), tính trên số tiền rút. Ví dụ rút 100.000đ → nhận 90.000đ, phí 10.000đ.
- **Quy tắc rút tiền:** mức rút tối thiểu **1.000.000đ**, phí **10%** trên mỗi lệnh rút. Không cho rút nhỏ lẻ để né phí. Các tham số `withdraw_min` (1.000.000), `withdraw_fee_rate` (10%), giá thuê, số ngày giữ tiền (mặc định 3) nằm trong bảng `settings` để admin chỉnh sau, hiển thị công khai trong điều khoản và trang đăng ký bán.
- Hàng hóa: sản phẩm số hợp pháp (Tool/Script, Website/Web App, MMO Tool, Bot Chat/Discord, Automation, Khác). Cấm hàng cấm, mã độc, tài khoản/dữ liệu đánh cắp, vi phạm bản quyền.

## 2. Stack
- Backend: **Rust**, Axum, Tokio, SQLx (PostgreSQL), tower-http, tracing. Realtime bằng WebSocket của Axum.
- Frontend: **1 codebase React + TypeScript + TailwindCSS + Vite + React Query**, build ra **2 đầu ra bắt buộc**:
  1. **Website thật** (static build `vite build`, deploy lên Cloudflare Pages hoặc Vercel, có tên miền, responsive cả máy tính lẫn điện thoại, SEO cơ bản cho trang sản phẩm: title/meta/Open Graph, sitemap).
  2. **App Android** bằng **Tauri 2** bọc đúng frontend đó (APK).
  Không được viết 2 giao diện riêng. API base URL cấu hình bằng biến môi trường (`VITE_API_URL`). Gọi API qua REST, chat qua WebSocket. Backend bật CORS cho domain web và origin của Tauri. Phần nào chỉ có trên app (thông báo đẩy, lưu file tải về) thì tách qua lớp `platform.ts`, trên web dùng phương án thay thế (tải file qua trình duyệt, Web Push/không có).
- Giao diện web trên màn hình rộng: layout 2–3 cột (sidebar danh mục, nội dung, cột gợi ý), trên điện thoại giữ layout 1 cột như chocode.
- Lưu file: S3-compatible (Cloudflare R2 hoặc Supabase Storage), bucket **private**.
- Auth: email + mật khẩu (argon2), access token JWT ngắn hạn + refresh token xoay vòng, vai trò `buyer`, `seller`, `admin`. Một tài khoản có thể vừa mua vừa bán.
- Thanh toán nạp tiền: trừu tượng hóa bằng trait `PaymentGateway`, viết adapter PayOS (hoặc VNPay) + adapter giả để test. Webhook phải **xác thực chữ ký** và **idempotent**.
- Dev: `docker-compose` cho Postgres, file `.env.example`, `Makefile` hoặc `justfile`.
- CI: GitHub Actions chạy `cargo fmt`, `clippy`, `test`, và workflow build **APK Android** bằng Tauri 2.

## 3. Luồng tiền (quan trọng nhất, làm cẩn thận)
Nguyên tắc: mọi số tiền là `i64` đơn vị đồng, **không dùng float**. Dùng **sổ cái kép (ledger)**: mọi biến động tiền là các dòng bút toán bất biến, số dư là kết quả cộng dồn, tổng bút toán của mỗi giao dịch phải bằng 0. Mọi thao tác tiền chạy trong DB transaction, khóa dòng (`SELECT ... FOR UPDATE`), có `idempotency_key`.

Tài khoản trong ledger: `wallet_spendable` (tiền nạp, chỉ để mua hàng, **không rút được**), `earnings_pending` (doanh thu đang giữ), `earnings_available` (doanh thu rút được), `escrow`, `platform_revenue`, `external` (nạp/rút). Khi mua hàng, trừ `wallet_spendable` của người mua. Người bán muốn thuê gian hàng có thể trả bằng `earnings_available` hoặc `wallet_spendable`. Tiền nạp thừa muốn lấy lại chỉ là ngoại lệ do admin xử lý thủ công, có log.

1. **Nạp tiền (người mua):** tạo lệnh nạp → cổng thanh toán → webhook xác nhận → cộng ví người mua.
2. **Mua hàng:** trừ ví người mua → chuyển vào `escrow`, đơn ở trạng thái `paid`. File mở tải ngay.
3. **Hoàn tất đơn:** người mua bấm xác nhận, hoặc tự động sau `auto_complete_days` (mặc định 3 ngày, job nền) nếu không có khiếu nại → chuyển 100% từ `escrow` sang `earnings_pending`, rồi sang `earnings_available` sau `hold_days` (mặc định 3 ngày). Hai là **hai tham số riêng** trong `settings`, nên tiền về ví rút được sau tối đa 6 ngày. Hoàn tiền tự động chỉ khi tiền còn ở `escrow` hoặc `earnings_pending`; trường hợp hiếm tiền đã rút rồi thì ghi **công nợ** cho người bán (số dư âm bị chặn rút), xử lý ở phase sau.
4. **Khiếu nại/hoàn tiền:** người mua mở tranh chấp trong thời gian giữ tiền → đơn đóng băng, admin phân xử: hoàn người mua hoặc trả người bán. Có log đầy đủ.
5. **Rút tiền (người bán):** nhập số tiền ≥ 1.000.000đ (`withdraw_min`) và ≤ `earnings_available` (**chỉ rút được doanh thu bán hàng, không rút tiền nạp**) → hiển thị trước phí 10% và số thực nhận → tạo lệnh rút trạng thái `pending` → admin duyệt (hoặc tự động theo hạn mức) → ghi bút toán: ví người bán −X, `platform_revenue` +10%·X, `external` +90%·X. Từ chối lệnh thì hoàn lại. Người bán phải xác minh danh tính (KYC cơ bản: họ tên, số tài khoản ngân hàng) trước khi rút lần đầu.
6. **Thuê gian hàng:** trừ ví người bán → `platform_revenue`, cộng 30 ngày vào `stall_until`. Hết hạn thì gian hàng bị ẩn (không xóa dữ liệu), cho phép gia hạn. Nhắc trước 3 ngày.
7. Test bắt buộc: làm tròn phí, rút đúng bằng số dư, rút đồng thời 2 lệnh (không âm ví), webhook gọi lặp, hoàn tiền, tổng ledger luôn cân bằng (property test).

## 4. Giao file sau khi mua
- Người bán upload file (zip, rar, 7z, py, js, json, pdf, exe… theo allowlist, giới hạn dung lượng cấu hình được) lên bucket private. Có phiên bản (`version`) và changelog.
- Chỉ người đã mua (đơn `paid`/`completed`) mới nhận **signed URL có hạn** (vài phút), giới hạn số lượt tải và ghi log tải. Không bao giờ lộ URL gốc.
- **Quy trình duyệt bắt buộc, không sản phẩm nào lên sàn nếu chưa qua:**
  1. Người bán nộp: file, mô tả, ảnh/video demo, hướng dẫn cài đặt, yêu cầu hệ thống, cam kết không chứa mã độc/hành vi lấy cắp dữ liệu.
  2. Tự động: kiểm tra đuôi/MIME, SHA-256, quét virus (ClamAV, job nền), quét chuỗi đáng ngờ (đọc cookie/token, gửi dữ liệu ra server lạ, obfuscate nặng). Kết quả lưu vào `review_reports`.
  3. Admin test thủ công trong môi trường cô lập (VM/sandbox, không dùng máy chính), tick checklist: chạy được đúng như mô tả, không hành vi độc hại, không vi phạm bản quyền/điều khoản game, hướng dẫn đủ dùng.
  4. Trạng thái: `draft` → `pending_review` → `testing` → `approved` hoặc `rejected` (kèm lý do, người bán sửa rồi nộp lại). Chỉ `approved` mới hiển thị và bán được.
  5. Mỗi **phiên bản cập nhật** mới cũng phải duyệt lại. Phiên bản cũ đang bán giữ nguyên cho tới khi bản mới được duyệt.
  6. Sản phẩm bị báo cáo sau khi lên sàn có thể bị gỡ tạm thời và hoàn tiền cho người mua đã tải.
  7. **Test tay mọi sản phẩm và mọi phiên bản, không ngoại lệ.** Danh mục rủi ro cao (MMO Tool, Bot) có checklist riêng chặt hơn. Tham số `auto_approve_trusted` mặc định **tắt**, chỉ để dành nới cho người bán uy tín về sau.
- Trang "Đơn của tôi": danh sách đơn, nút Tải về, cập nhật phiên bản mới, nút Báo lỗi/Khiếu nại, Đánh giá.

## 5. Trang chi tiết sản phẩm
Ảnh/video demo (gallery), tên, giá, danh mục, mô tả (markdown an toàn, sanitize), yêu cầu hệ thống, phiên bản + changelog, số lượt bán, điểm đánh giá, danh sách review (chỉ người đã mua mới được review), card người bán (avatar, tên shop, điểm, số sản phẩm, nút **Nhắn tin**), nút **Mua ngay**, sản phẩm liên quan. Có skeleton loading, trạng thái lỗi/trống rõ ràng.

## 6. Feed cộng đồng kiểu Facebook
- Đăng bài: văn bản + tối đa N ảnh, gắn thẻ sản phẩm (hiện card sản phẩm trong bài), hashtag theo danh mục.
- Tương tác: like, bình luận (lồng 1 cấp), chia sẻ link bài, theo dõi người dùng/gian hàng, tab "Dành cho bạn" / "Đang theo dõi".
- Phân trang bằng cursor (infinite scroll), cập nhật realtime số like/bình luận.
- Kiểm duyệt: báo cáo bài, ẩn/xóa bởi admin, chặn người dùng, giới hạn tần suất đăng (rate limit), lọc link/spam, cấm quảng cáo hàng cấm.

## 7. Nhắn tin và chốt giao dịch ngay trên web
- Chat 1-1 realtime (WebSocket): trạng thái đã gửi/đã xem, gửi ảnh, nút chat nổi ở góc dưới phải như giao diện chocode, danh sách hội thoại.
- **Thẻ ưu đãi (offer card) trong chat:** người bán tạo đề nghị (sản phẩm + giá thỏa thuận + mô tả) → người mua bấm **Chấp nhận & thanh toán** → tạo đơn đi qua đúng luồng escrow ở mục 3. Mọi giao dịch chốt trong chat đều qua ví sàn để được bảo vệ.
- Cảnh báo an toàn trong chat: nhắc không chuyển tiền ngoài sàn; phát hiện chuỗi giống số tài khoản/link ngoài thì hiện cảnh báo. Có nút báo cáo cuộc trò chuyện. Lưu lịch sử chat làm bằng chứng khi có tranh chấp.

## 8. Thống kê và quản lý chi tiêu cho anh em
- **Người mua:** tổng chi tiêu theo tháng, theo danh mục, danh sách đơn, số dư ví, lịch sử nạp/mua/hoàn, đặt ngân sách tháng và cảnh báo khi vượt.
- **Người bán:** doanh thu theo ngày/tuần/tháng, số đơn, sản phẩm bán chạy, tỷ lệ hoàn/khiếu nại, tiền đang giữ (pending), tiền khả dụng, lịch sử rút và phí đã trả, ngày hết hạn thuê gian hàng.
- **Admin:** doanh thu sàn (phí rút + tiền thuê), số người dùng/gian hàng/đơn, hàng chờ duyệt (sản phẩm, lệnh rút, tranh chấp, báo cáo), xuất CSV đối soát.
- Biểu đồ dùng thư viện nhẹ (recharts), tính toán tổng hợp ở backend bằng SQL.

## 9. Giao diện (bám sát chocode.com.vn trong ảnh tham khảo)
- Mobile-first, nền sáng `#f4f5f8` có hỗ trợ dark mode, card trắng bo góc 14–20px, màu chính xanh `#2563eb`, tiêu đề có gradient xanh→tím nhẹ, font Be Vietnam Pro (fallback system-ui).
- Header: nút menu hamburger, tiêu đề trang, nút `+` xanh (đăng sản phẩm/bài), nút Đăng nhập.
- Trang chủ: khối hero "Bạn cần làm gì hôm nay?" nền lưới chấm, ô tìm kiếm + nút Tìm, lưới 2 cột 6 danh mục có icon, banner nhắc đăng nhập để theo dõi đơn, mục "Sản phẩm mới" + "Xem tất cả", skeleton khi tải.
- Nút chat nổi tròn xanh góc dưới phải. Thanh điều hướng dưới: Trang chủ, Feed, Chat, Ví/Thống kê, Tài khoản.
- Chữ sentence case, nút nói rõ hành động ("Mua ngay", "Rút tiền"), lỗi nói rõ nguyên nhân và cách sửa. Hỗ trợ bàn phím, tương phản đủ, tôn trọng `prefers-reduced-motion`.

## 10. Bảo mật, pháp lý, vận hành
- Validate toàn bộ input, rate limit, CORS chặt, sanitize markdown/HTML, bảo vệ CSRF/XSS, không log dữ liệu nhạy cảm, mã hóa thông tin ngân hàng, bảng `audit_log` cho mọi hành động admin và thao tác tiền.
- Trang Điều khoản/Chính sách: phí rút 10% và giá thuê công khai, quy trình hoàn tiền/tranh chấp, danh mục hàng cấm. Ghi chú trong README rằng chủ sàn cần tự kiểm tra nghĩa vụ pháp lý/thuế khi thu tiền hộ và chạy thật (không đưa ra như tư vấn pháp lý).
- Healthcheck, structured logging, migration có version (`sqlx migrate`), seed data demo.

## 11. Các phase giao hàng
1. **Nền tảng:** cấu trúc repo, Postgres + migrations, auth, vai trò, settings, CI.
2. **Ledger + ví + nạp/rút + thuê gian hàng** kèm bộ test mục 3. Chưa làm UI phức tạp, tập trung đúng tiền.
3. **Sản phẩm, upload/giao file, duyệt admin, trang chi tiết, đơn hàng + escrow + tranh chấp.**
4. **Giao diện chocode** cho trang chủ, danh mục, tìm kiếm, chi tiết, đơn của tôi, ví. **Deploy website lên mạng ở phase này** (backend + web chạy được qua link), hướng dẫn gắn tên miền.
5. **Thống kê** cho người mua/người bán/admin.
6. **Feed + chat + offer card.**
7. **Đóng gói Tauri Android** từ chính frontend của website + GitHub Actions ra APK, kiểm tra web và app cùng chạy đúng trên một backend, tài liệu triển khai.

Tiêu chí nghiệm thu mỗi phase: build sạch, `clippy` không cảnh báo, test pass, có hướng dẫn chạy thử trong README. Nếu thấy chỗ nào trong prompt mâu thuẫn hoặc thiếu, hãy hỏi tôi trước khi làm.
