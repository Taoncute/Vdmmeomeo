-- Phase 1: nền tảng. Email lưu đã chữ thường (CHECK) thay cho citext để tránh
-- lệch kiểu giữa citext và text khi so sánh tham số; hành vi không phân biệt hoa thường giữ nguyên.

CREATE TABLE roles (
    id   smallint PRIMARY KEY,
    name text NOT NULL UNIQUE
);
INSERT INTO roles (id, name) VALUES (1, 'buyer'), (2, 'seller'), (3, 'reviewer'), (4, 'admin');

CREATE TABLE users (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email         text NOT NULL,
    password_hash text NOT NULL,
    display_name  text NOT NULL,
    avatar_url    text,
    status        text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'banned')),
    created_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT users_email_lowercase CHECK (email = lower(email))
);
CREATE UNIQUE INDEX users_email_key ON users (email);

CREATE TABLE user_roles (
    user_id uuid     NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role_id smallint NOT NULL REFERENCES roles (id),
    PRIMARY KEY (user_id, role_id)
);

CREATE TABLE refresh_tokens (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id    uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    family_id  uuid NOT NULL,
    token_hash text NOT NULL UNIQUE,
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX refresh_tokens_family_idx ON refresh_tokens (family_id);
CREATE INDEX refresh_tokens_user_idx ON refresh_tokens (user_id);

CREATE TABLE settings (
    key        text PRIMARY KEY,
    value      jsonb NOT NULL,
    updated_by uuid REFERENCES users (id),
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- max_upload_mb, download_limit, file_ext_allowlist chưa seed: tài liệu chưa nêu giá trị,
-- phase 3 (upload/giao file) sẽ thêm sau khi chủ dự án chốt.
INSERT INTO settings (key, value) VALUES
    ('stall_price', '99000'),
    ('stall_days', '30'),
    ('withdraw_min', '1000000'),
    ('withdraw_fee_bps', '1000'),
    ('hold_days', '3'),
    ('auto_complete_days', '3'),
    ('auto_approve_trusted', 'false');

-- Hàm chặn sửa/xóa dùng chung cho các bảng chỉ ghi thêm (audit_log, sổ cái).
CREATE FUNCTION forbid_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% on % is not allowed: table is append-only', TG_OP, TG_TABLE_NAME
        USING ERRCODE = '42501';
END;
$$;

CREATE TABLE audit_log (
    id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_id    uuid,
    action      text NOT NULL,
    target_type text NOT NULL,
    target_id   text,
    before      jsonb,
    after       jsonb,
    ip          text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_created_idx ON audit_log (created_at DESC);
CREATE TRIGGER audit_log_no_update_delete
    BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION forbid_mutation();
CREATE TRIGGER audit_log_no_truncate
    BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION forbid_mutation();

CREATE TABLE jobs (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind        text NOT NULL,
    payload     jsonb NOT NULL DEFAULT '{}'::jsonb,
    run_at      timestamptz NOT NULL DEFAULT now(),
    attempts    integer NOT NULL DEFAULT 0,
    status      text NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'done', 'failed')),
    last_error  text,
    dedupe_key  text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);
CREATE INDEX jobs_due_idx ON jobs (run_at) WHERE status = 'queued';
CREATE UNIQUE INDEX jobs_dedupe_queued ON jobs (dedupe_key)
    WHERE status = 'queued' AND dedupe_key IS NOT NULL;
