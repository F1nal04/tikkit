use std::{process::ExitCode, time::Duration};

use tikkit_api::{
    AppState, Config,
    config::Environment,
    db,
    server::{self, ShutdownOutcome},
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
Usage: tikkit-api [COMMAND]

Commands:
  serve         Run the HTTP API (default)
  migrate       Apply pending database migrations and exit
  check-config  Validate configuration, print a redacted summary, and exit
  help          Show this message

Configuration is read from environment variables; see docs/configuration.md.";

enum Command {
    Serve,
    Migrate,
    CheckConfig,
}

/// Exit code for invalid configuration or usage.
const EXIT_CONFIG: u8 = 2;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["serve"] => Command::Serve,
        ["migrate"] => Command::Migrate,
        ["check-config"] => Command::CheckConfig,
        ["help" | "--help" | "-h"] => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprintln!("error: unknown arguments {args:?}\n\n{USAGE}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            // Logging is not configured yet; the message is already redacted.
            eprintln!("error: {error}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };

    if let Command::CheckConfig = command {
        println!("Configuration is valid.\n{}", config.summary());
        return ExitCode::SUCCESS;
    }

    init_logging(&config);
    match command {
        Command::Migrate => run_migrate(config).await,
        Command::Serve => run_serve(config).await,
        Command::CheckConfig => unreachable!(),
    }
}

fn init_logging(config: &Config) {
    let filter = EnvFilter::builder()
        .parse(&config.log_filter)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    match config.environment {
        Environment::Production => builder.json().with_current_span(false).init(),
        Environment::Development => builder.init(),
    }
}

async fn run_migrate(config: Config) -> ExitCode {
    let pool = match db::connect_with_retry(&config).await {
        Ok(pool) => pool,
        Err(error) => {
            tracing::error!(%error, "migration failed");
            return ExitCode::FAILURE;
        }
    };
    let result = db::migrate(&pool).await;
    pool.close().await;
    match result {
        Ok(status) => {
            tracing::info!(
                applied = status.applied.len(),
                latest = status.applied.last().copied(),
                "database schema is up to date"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "migration failed");
            ExitCode::FAILURE
        }
    }
}

async fn run_serve(config: Config) -> ExitCode {
    tracing::info!(
        environment = config.environment.as_str(),
        database = %db::describe_target(&config),
        api_addr = %config.api_addr,
        public_origin = %config.public_origin,
        ai = "disabled",
        smtp = "disabled",
        "starting Tikkit API"
    );
    if config.database_url_is_default {
        tracing::warn!(
            "DATABASE_URL is not set; using the development default (start it with `docker compose -f compose.dev.yml up -d`)"
        );
    }
    if !config.environment.is_production() {
        tracing::warn!("running in development mode; set TIKKIT_ENV=production for deployments");
    }

    let shutdown = server::shutdown_signal();
    tokio::pin!(shutdown);

    // Startup can be interrupted while waiting for the database.
    let pool = tokio::select! {
        result = db::connect_with_retry(&config) => match result {
            Ok(pool) => pool,
            Err(error) => {
                tracing::error!(%error, "startup failed");
                return ExitCode::FAILURE;
            }
        },
        () = &mut shutdown => return ExitCode::SUCCESS,
    };

    match db::prepare_schema(&pool, config.database_auto_migrate).await {
        Ok(status) => tracing::info!(
            migrations = status.applied.len(),
            "database schema is compatible"
        ),
        Err(error) => {
            tracing::error!(%error, "startup failed");
            pool.close().await;
            return ExitCode::FAILURE;
        }
    }

    let listener = match TcpListener::bind(config.api_addr).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, addr = %config.api_addr, "unable to bind API_ADDR");
            pool.close().await;
            return ExitCode::FAILURE;
        }
    };
    match listener.local_addr() {
        Ok(addr) => tracing::info!("Tikkit API listening on http://{addr}"),
        Err(_) => tracing::info!("Tikkit API listening"),
    }

    let drain_timeout = config.shutdown_timeout;
    let state = AppState::new(pool.clone(), config);
    let outcome = server::serve(listener, tikkit_api::app(state), shutdown, drain_timeout).await;
    // Requests abandoned after the drain timeout may still hold connections;
    // do not let closing the pool extend shutdown beyond a short bound.
    if tokio::time::timeout(Duration::from_secs(2), pool.close())
        .await
        .is_err()
    {
        tracing::warn!("database pool did not close in time");
    }
    match outcome {
        Ok(ShutdownOutcome::Drained | ShutdownOutcome::Stopped) => {
            tracing::info!("shutdown complete");
            ExitCode::SUCCESS
        }
        Ok(ShutdownOutcome::TimedOut) => {
            tracing::warn!("shutdown complete after drain timeout");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "server error");
            ExitCode::FAILURE
        }
    }
}
