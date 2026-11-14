use std::net::SocketAddr;

use server::config::Config;
use server::state::AppState;
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

fn init_tracing(cfg: &Config) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,tower_http=info,sqlx=warn"));
    if cfg.log_json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("nhận tín hiệu dừng, đang tắt server");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let config = Config::from_env()?;
    init_tracing(&config);

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await?;
    server::MIGRATOR.run(&pool).await?;

    let bind_addr = config.bind_addr.clone();
    let state = AppState::new(pool, config)?;

    match std::env::args().nth(1).as_deref() {
        Some("seed") => server::seed::run(&state).await,
        Some("--worker") => server::jobs::run_worker(state).await,
        _ => {
            let app = server::api::router(state);
            let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
            tracing::info!(addr = %bind_addr, "server đang chạy");
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await?;
            Ok(())
        }
    }
}
