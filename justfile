# Cần cài `just` (https://github.com/casey/just). Không bắt buộc: có thể chạy lệnh trực tiếp.
set dotenv-load := true

default:
    @just --list

db-up:
    docker compose up -d postgres

db-down:
    docker compose down

run:
    cd backend && cargo run -p server

worker:
    cd backend && cargo run -p server -- --worker

seed:
    cd backend && cargo run -p server -- seed

fmt:
    cd backend && cargo fmt --all

check:
    cd backend && cargo fmt --all --check
    cd backend && cargo clippy --all-targets -- -D warnings

# Test cần Postgres đang chạy (just db-up) và biến DATABASE_URL.
test:
    cd backend && cargo test

web-dev:
    cd web && npm run dev

web-build:
    cd web && npm install && npm run build
