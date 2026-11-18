use std::{future::IntoFuture, net::SocketAddr, time::Duration};

use anyhow::Context;
use tikkit_api::{
    AppState, ai_attempts, ai_guard, app_with, auth, autoclose, config::Config, db, metrics,
    notifications, retention, screenshots, telemetry,
};
use tokio::{net::TcpListener, sync::watch};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let migrate_only = match std::env::args_os().skip(1).collect::<Vec<_>>().as_slice() {
        [] => false,
        [command] if command == "migrate" => true,
        _ => anyhow::bail!("usage: tikkit-api [migrate]"),
    };

    // Development convenience: load ./.env (or a parent's) without overriding real
    // env vars. Production reads only the real environment.
    if std::env::var("ENVIRONMENT").as_deref() != Ok("production") {
        dotenvy::dotenv().ok();
    }
    let config = Config::from_env()?;
    telemetry::init_logging(config.json_logs);

    let pool = db::connect(&config.database_url).await.map_err(|error| {
        let sqlstate = error.as_database_error().and_then(|db| db.code());
        tracing::error!(
            error_kind = db::error_kind(&error),
            sqlstate = sqlstate.as_deref(),
            "database startup connection failed"
        );
        anyhow::anyhow!(
            "cannot connect to PostgreSQL; check DATABASE_URL and database service health"
        )
    })?;
    db::migrate(&pool)
        .await
        .map_err(|error| {
            db::log_migration_failure(&error);
            anyhow::anyhow!("database migration failed; check database health and the migration history for this build")
        })?;
    if migrate_only {
        pool.close().await;
        tracing::info!("Database migrations complete");
        return Ok(());
    }

    std::fs::create_dir_all(&config.screenshot_dir)
        .context("cannot create SCREENSHOT_DIR (see docs/configuration.md)")?;
    tokio::spawn(screenshots::cleanup_loop(
        pool.clone(),
        config.screenshot_dir.clone(),
    ));

    let listener = TcpListener::bind(config.api_addr)
        .await
        .with_context(|| format!("cannot listen on API_ADDR {}", config.api_addr))?;
    let metrics_listener = match config.metrics_port {
        Some(port) => Some(
            metrics::bind(port)
                .await
                .context("cannot listen on METRICS_PORT")?,
        ),
        None => None,
    };
    tracing::info!(address = %listener.local_addr()?, "Tikkit API listening");
    let state = AppState::new(pool.clone(), config.screenshot_dir.as_path().into());
    let settings = auth::Settings::from(&config);
    let notifier = tokio::spawn(notifications::worker_loop(state.clone(), settings.clone()));
    let auto_close = tokio::spawn(autoclose::worker_loop(state.clone()));
    let ai_retention = tokio::spawn(ai_guard::retention_loop(pool.clone()));
    let data_retention = tokio::spawn(retention::worker_loop(state.clone()));

    // Stop accepting on SIGTERM/Ctrl+C, then give in-flight requests a bounded drain window.
    let (stopping, mut stopped) = watch::channel(false);
    let mut metrics_stopped = stopped.clone();
    let metrics_state = state.clone();
    let metrics_server = async move {
        if let Some(listener) = metrics_listener {
            tracing::info!(address = %listener.local_addr()?, "private metrics listening");
            axum::serve(listener, metrics::router(metrics_state))
                .with_graceful_shutdown(async move {
                    let _ = metrics_stopped.wait_for(|stop| *stop).await;
                })
                .await?;
        }
        Ok::<_, std::io::Error>(())
    };
    // AI suggestion attempts. At shutdown the calls in flight are cancelled and
    // their attempts released for the next start to take over.
    let ai_worker = tokio::spawn(ai_attempts::worker_loop(ai_attempts::Worker::new(
        &state,
        settings.encryption_key,
    )));
    let app = app_with(state, settings);
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        tracing::info!(
            "Shutting down; draining requests for up to {:?}",
            config.shutdown_timeout
        );
        stopping.send_replace(true);
        notifications::stop_workers();
    })
    .into_future();
    let deadline = async {
        let _ = stopped.wait_for(|stop| *stop).await;
        tokio::time::sleep(config.shutdown_timeout).await;
    };
    tokio::select! {
        result = async { tokio::try_join!(server, metrics_server) } => { result?; },
        () = deadline => tracing::warn!("Drain timeout reached; aborting remaining requests"),
    }
    // Let a notification already being sent record its outcome, an auto-close
    // already running commit, and cancelled AI calls free their provider slots
    // and release their attempts, before the pool closes.
    if tokio::time::timeout(Duration::from_secs(5), async {
        let _ = tokio::join!(
            notifier,
            auto_close,
            ai_retention,
            data_retention,
            ai_worker
        );
    })
    .await
    .is_err()
    {
        tracing::warn!("Timed out waiting for the background workers");
    }
    // Bounded so a stuck connection cannot hold the process past the drain window.
    if tokio::time::timeout(Duration::from_secs(5), pool.close())
        .await
        .is_err()
    {
        tracing::warn!("Timed out closing database connections");
    }
    tracing::info!("Tikkit API stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!("Unable to listen for Ctrl+C: {error}");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!("Unable to listen for SIGTERM: {error}");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
