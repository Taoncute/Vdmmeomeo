-- Phase 3: sản phẩm, phiên bản, file/quét/duyệt, đơn hàng qua escrow, tranh chấp, giao file, đánh giá.

-- ---------- Bổ sung cài đặt Phase 3 ----------

INSERT INTO settings (key, value) VALUES
    ('max_upload_mb', '100'),
    ('download_limit', '5'),
    ('file_ext_allowlist', '["zip","rar","7z","py","js","json","pdf","md","txt"]')
ON CONFLICT (key) DO NOTHING;

-- Cho phép đóng băng khoản giữ doanh thu khi có tranh chấp ở trạng thái earnings_pending.
ALTER TABLE earnings_holds DROP CONSTRAINT IF EXISTS earnings_holds_status_check;
ALTER TABLE earnings_holds ADD CONSTRAINT earnings_holds_status_check
    CHECK (status IN ('pending', 'frozen', 'released', 'refunded'));

-- ---------- Danh mục ----------

CREATE TABLE categories (
    id         smallint PRIMARY KEY,
    slug       text NOT NULL UNIQUE,
    name       text NOT NULL,
    icon       text NOT NULL,
    risk_level text NOT NULL DEFAULT 'normal' CHECK (risk_level IN ('normal', 'high'))
);

INSERT INTO categories (id, slug, name, icon, risk_level) VALUES
    (1, 'tool-script', 'Tool / Script', 'wrench', 'normal'),
    (2, 'website-web-app', 'Website / Web App', 'globe', 'normal'),
    (3, 'mmo-tool', 'MMO Tool', 'gamepad', 'high'),
    (4, 'bot-chat-discord', 'Bot Chat / Discord', 'bot', 'high'),
    (5, 'automation', 'Automation', 'zap', 'normal'),
    (6, 'khac', 'Khác', 'package', 'normal');

-- ---------- Sản phẩm & phiên bản ----------

CREATE TABLE products (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    shop_id              uuid NOT NULL REFERENCES shops (id),
    category_id          smallint NOT NULL REFERENCES categories (id),
    title                text NOT NULL,
    slug                 text NOT NULL UNIQUE,
    price                bigint NOT NULL CHECK (price > 0),
    description_md       text NOT NULL DEFAULT '',
    requirements         text NOT NULL DEFAULT '',
    status               text NOT NULL DEFAULT 'draft'
                             CHECK (status IN ('draft', 'active', 'taken_down')),
    published_version_id uuid,
    sales_count          integer NOT NULL DEFAULT 0 CHECK (sales_count >= 0),
    rating_avg           double precision,
    rating_count         integer NOT NULL DEFAULT 0 CHECK (rating_count >= 0),
    takedown_reason      text,
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX products_shop_idx ON products (shop_id, created_at DESC);
CREATE INDEX products_cat_status_idx ON products (category_id, status, created_at DESC);
CREATE INDEX products_status_created_idx ON products (status, created_at DESC);

CREATE TABLE product_versions (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id          uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    version             text NOT NULL,
    changelog           text NOT NULL DEFAULT '',
    status              text NOT NULL DEFAULT 'draft'
                            CHECK (status IN ('draft', 'pending_review', 'testing', 'approved', 'rejected')),
    reject_reason       text,
    commitment_accepted boolean NOT NULL DEFAULT false,
    submitted_at        timestamptz,
    claimed_by          uuid REFERENCES users (id),
    claimed_at          timestamptz,
    reviewed_by         uuid REFERENCES users (id),
    reviewed_at         timestamptz,
    created_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT product_versions_unique UNIQUE (product_id, version)
);
CREATE INDEX product_versions_product_idx ON product_versions (product_id, created_at DESC);
CREATE INDEX product_versions_queue_idx ON product_versions (status, submitted_at ASC)
    WHERE status IN ('pending_review', 'testing');

ALTER TABLE products
    ADD CONSTRAINT products_published_version_fk
    FOREIGN KEY (published_version_id) REFERENCES product_versions (id);

CREATE TABLE product_files (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id  uuid NOT NULL REFERENCES product_versions (id) ON DELETE CASCADE,
    object_key  text NOT NULL UNIQUE,
    filename    text NOT NULL,
    size        bigint NOT NULL CHECK (size > 0),
    mime        text NOT NULL,
    sha256      text NOT NULL,
    scan_status text NOT NULL DEFAULT 'pending'
                    CHECK (scan_status IN ('pending', 'clean', 'suspicious', 'infected', 'error')),
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX product_files_version_idx ON product_files (version_id, created_at ASC);

CREATE TABLE product_media (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    product_id uuid NOT NULL REFERENCES products (id) ON DELETE CASCADE,
    kind       text NOT NULL CHECK (kind IN ('image', 'video')),
    url        text NOT NULL,
    position   integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX product_media_product_idx ON product_media (product_id, position ASC);

CREATE TABLE review_reports (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id  uuid NOT NULL REFERENCES product_versions (id) ON DELETE CASCADE,
    kind        text NOT NULL CHECK (kind IN ('auto', 'manual')),
    result      jsonb NOT NULL DEFAULT '{}'::jsonb,
    checklist   jsonb,
    reviewer_id uuid REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX review_reports_version_idx ON review_reports (version_id, created_at DESC);

-- ---------- Đơn hàng, tải file, tranh chấp, đánh giá ----------

CREATE TABLE orders (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    buyer_id           uuid NOT NULL REFERENCES users (id),
    seller_id          uuid NOT NULL REFERENCES users (id),
    shop_id            uuid NOT NULL REFERENCES shops (id),
    product_id         uuid NOT NULL REFERENCES products (id),
    version_id         uuid NOT NULL REFERENCES product_versions (id),
    total              bigint NOT NULL CHECK (total > 0),
    status             text NOT NULL DEFAULT 'paid'
                           CHECK (status IN ('paid', 'completed', 'disputed', 'refunded')),
    pre_dispute_status text CHECK (pre_dispute_status IN ('paid', 'completed')),
    offer_id           uuid,
    idempotency_key    text NOT NULL,
    paid_at            timestamptz NOT NULL DEFAULT now(),
    auto_complete_at   timestamptz NOT NULL,
    completed_at       timestamptz,
    release_at         timestamptz,
    refunded_at        timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT orders_buyer_idem UNIQUE (buyer_id, idempotency_key)
);
CREATE INDEX orders_buyer_idx ON orders (buyer_id, created_at DESC);
CREATE INDEX orders_seller_idx ON orders (seller_id, created_at DESC);
CREATE INDEX orders_auto_complete_idx ON orders (auto_complete_at) WHERE status = 'paid';
-- Mỗi người mua chỉ mua một sản phẩm một lần (trừ khi đơn trước đã hoàn tiền).
CREATE UNIQUE INDEX orders_buyer_product_active_key
    ON orders (buyer_id, product_id)
    WHERE status <> 'refunded';

CREATE TABLE order_items (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id   uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    product_id uuid NOT NULL REFERENCES products (id),
    version_id uuid NOT NULL REFERENCES product_versions (id),
    price      bigint NOT NULL CHECK (price > 0)
);
CREATE INDEX order_items_order_idx ON order_items (order_id);

CREATE TABLE download_logs (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id   uuid NOT NULL REFERENCES orders (id) ON DELETE CASCADE,
    file_id    uuid NOT NULL REFERENCES product_files (id),
    user_id    uuid NOT NULL REFERENCES users (id),
    ip_hash    text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX download_logs_order_idx ON download_logs (order_id, created_at DESC);

CREATE TABLE disputes (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id        uuid NOT NULL UNIQUE REFERENCES orders (id),
    opened_by       uuid NOT NULL REFERENCES users (id),
    reason          text NOT NULL,
    evidence        jsonb NOT NULL DEFAULT '[]'::jsonb,
    status          text NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved')),
    resolution      text CHECK (resolution IN ('refund', 'release')),
    resolution_note text,
    resolved_by     uuid REFERENCES users (id),
    resolved_at     timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX disputes_status_idx ON disputes (status, created_at DESC);

CREATE TABLE reviews (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id   uuid NOT NULL UNIQUE REFERENCES orders (id),
    product_id uuid NOT NULL REFERENCES products (id),
    buyer_id   uuid NOT NULL REFERENCES users (id),
    rating     smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    body       text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX reviews_product_idx ON reviews (product_id, created_at DESC);
