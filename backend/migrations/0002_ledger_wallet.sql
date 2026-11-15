-- Phase 2: sổ cái, ví, nạp/rút, thuê gian hàng.
-- Tên tài khoản và loại giao dịch ở dạng text có CHECK (không dùng enum Postgres) cho dễ đổi.

-- ---------- Sổ cái ----------

CREATE TABLE ledger_accounts (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    key        text NOT NULL UNIQUE,
    kind       text NOT NULL CHECK (kind IN (
        'wallet_spendable', 'earnings_pending', 'earnings_available', 'escrow',
        'withdrawal_hold', 'platform_revenue', 'seller_debt', 'external')),
    user_id    uuid REFERENCES users (id),
    -- Escrow thuộc một đơn hàng. Chưa có FK vì bảng orders thuộc phase 3.
    order_id   uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT ledger_accounts_scope CHECK (
        (kind IN ('wallet_spendable', 'earnings_pending', 'earnings_available',
                  'withdrawal_hold', 'seller_debt')
            AND user_id IS NOT NULL AND order_id IS NULL)
        OR (kind = 'escrow' AND order_id IS NOT NULL AND user_id IS NULL)
        OR (kind IN ('platform_revenue', 'external')
            AND user_id IS NULL AND order_id IS NULL))
);
CREATE INDEX ledger_accounts_user_idx ON ledger_accounts (user_id) WHERE user_id IS NOT NULL;

CREATE TABLE ledger_transactions (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind            text NOT NULL,
    idempotency_key text NOT NULL UNIQUE,
    ref_type        text,
    ref_id          uuid,
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ledger_entries (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tx_id      uuid   NOT NULL REFERENCES ledger_transactions (id),
    account_id uuid   NOT NULL REFERENCES ledger_accounts (id),
    amount     bigint NOT NULL CHECK (amount <> 0),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ledger_entries_tx_idx ON ledger_entries (tx_id);
CREATE INDEX ledger_entries_account_idx ON ledger_entries (account_id, id DESC);

-- Bút toán và giao dịch bất biến.
CREATE TRIGGER ledger_entries_immutable
    BEFORE UPDATE OR DELETE ON ledger_entries
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER ledger_entries_no_truncate
    BEFORE TRUNCATE ON ledger_entries
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER ledger_transactions_immutable
    BEFORE UPDATE OR DELETE ON ledger_transactions
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER ledger_transactions_no_truncate
    BEFORE TRUNCATE ON ledger_transactions
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_mutation();

-- Tổng bút toán của mỗi giao dịch phải bằng 0, kiểm tra lúc COMMIT.
CREATE FUNCTION ledger_check_tx_balanced() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    s numeric;
BEGIN
    SELECT COALESCE(SUM(amount), 0) INTO s FROM ledger_entries WHERE tx_id = NEW.tx_id;
    IF s <> 0 THEN
        RAISE EXCEPTION 'ledger transaction % is not balanced (sum = %)', NEW.tx_id, s
            USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER ledger_entries_balanced
    AFTER INSERT ON ledger_entries
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ledger_check_tx_balanced();

CREATE TABLE account_balances (
    account_id     uuid PRIMARY KEY REFERENCES ledger_accounts (id),
    balance        bigint  NOT NULL DEFAULT 0,
    version        bigint  NOT NULL DEFAULT 0,
    allow_negative boolean NOT NULL DEFAULT false,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT account_balances_non_negative CHECK (allow_negative OR balance >= 0)
);

-- Hai tài khoản toàn cục tạo sẵn để mọi giao dịch không tranh nhau tạo chúng.
INSERT INTO ledger_accounts (key, kind) VALUES
    ('platform_revenue', 'platform_revenue'),
    ('external', 'external');
INSERT INTO account_balances (account_id, allow_negative)
    SELECT id, (kind = 'external') FROM ledger_accounts;

-- ---------- Nạp tiền ----------

CREATE SEQUENCE deposit_order_code_seq START 100000;

CREATE TABLE deposits (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      uuid NOT NULL REFERENCES users (id),
    amount       bigint NOT NULL CHECK (amount > 0),
    gateway      text NOT NULL CHECK (gateway IN ('payos', 'manual', 'fake')),
    gateway_ref  text,
    -- Mã số nguyên gửi cho PayOS (orderCode) và dùng làm mã nội dung chuyển khoản.
    order_code   bigint NOT NULL UNIQUE,
    transfer_code text NOT NULL UNIQUE,
    status       text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'confirmed', 'cancelled')),
    confirmed_at timestamptz,
    confirmed_by uuid REFERENCES users (id),
    ledger_tx_id uuid REFERENCES ledger_transactions (id),
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX deposits_user_idx ON deposits (user_id, created_at DESC);
CREATE INDEX deposits_status_idx ON deposits (status, created_at DESC);

-- ---------- Rút tiền ----------

CREATE TABLE withdrawals (
    id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         uuid NOT NULL REFERENCES users (id),
    amount          bigint NOT NULL CHECK (amount > 0),
    fee             bigint NOT NULL,
    net             bigint NOT NULL,
    status          text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected')),
    bank_txn_ref    text,
    reviewed_by     uuid REFERENCES users (id),
    reviewed_at     timestamptz,
    reason          text,
    idempotency_key text NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT withdrawals_amounts CHECK (fee >= 0 AND fee <= amount AND net = amount - fee),
    CONSTRAINT withdrawals_user_idem UNIQUE (user_id, idempotency_key)
);
CREATE INDEX withdrawals_status_idx ON withdrawals (status, created_at DESC);

CREATE TABLE seller_kyc (
    user_id        uuid PRIMARY KEY REFERENCES users (id),
    full_name      text NOT NULL,
    bank_name      text NOT NULL,
    account_no_enc bytea NOT NULL,
    key_id         text NOT NULL,
    status         text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'rejected')),
    reject_reason  text,
    reviewed_by    uuid REFERENCES users (id),
    reviewed_at    timestamptz,
    submitted_at   timestamptz NOT NULL DEFAULT now()
);

-- ---------- Gian hàng ----------

CREATE TABLE shops (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id    uuid NOT NULL UNIQUE REFERENCES users (id),
    name        text NOT NULL,
    -- Tên chữ thường (Unicode) để bảo đảm duy nhất không phân biệt hoa thường.
    name_key    text NOT NULL UNIQUE,
    slug        text NOT NULL UNIQUE,
    bio         text,
    avatar_url  text,
    stall_until timestamptz,
    rating_avg  double precision,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE stall_rentals (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    shop_id      uuid NOT NULL REFERENCES shops (id),
    ledger_tx_id uuid NOT NULL REFERENCES ledger_transactions (id),
    source       text NOT NULL CHECK (source IN ('spendable', 'earnings')),
    price        bigint NOT NULL CHECK (price > 0),
    days         integer NOT NULL CHECK (days > 0),
    starts_at    timestamptz NOT NULL,
    ends_at      timestamptz NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX stall_rentals_shop_idx ON stall_rentals (shop_id, created_at DESC);

-- ---------- Tiền doanh thu đang giữ ----------

CREATE TABLE earnings_holds (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    seller_id   uuid NOT NULL REFERENCES users (id),
    -- Mã đơn hàng. Chưa có FK vì bảng orders thuộc phase 3.
    order_ref   uuid,
    amount      bigint NOT NULL CHECK (amount > 0),
    release_at  timestamptz NOT NULL,
    status      text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'released', 'refunded')),
    released_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX earnings_holds_due_idx ON earnings_holds (release_at) WHERE status = 'pending';
CREATE UNIQUE INDEX earnings_holds_order_key ON earnings_holds (order_ref) WHERE order_ref IS NOT NULL;

-- ---------- Thông báo trong app ----------

CREATE TABLE notifications (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind       text NOT NULL,
    body       text NOT NULL,
    dedupe_key text,
    read_at    timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notifications_dedupe UNIQUE (user_id, dedupe_key)
);
CREATE INDEX notifications_user_idx ON notifications (user_id, created_at DESC);
