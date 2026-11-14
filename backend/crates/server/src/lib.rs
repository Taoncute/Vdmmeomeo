pub mod api;
pub mod audit;
pub mod auth;
pub mod config;
pub mod error;
pub mod jobs;
pub mod ratelimit;
pub mod seed;
pub mod settings;
pub mod state;

/// Toàn bộ migration, nhúng vào binary và dùng chung cho test (`#[sqlx::test]`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");
