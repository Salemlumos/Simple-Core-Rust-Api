//! Binário único: `api <bootstrap|migrate|seed|serve|worker|healthcheck>`.

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::process::ExitCode;

use simple_core::config::{ApiConfig, DbConfig};
use simple_core::{app, cli, telemetry};

const USAGE: &str = "uso: api <bootstrap|migrate|seed|serve|worker|healthcheck>";

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    if cmd == "healthcheck" {
        // Sem logs: só o código de saída importa.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
        return match rt.map(|rt| rt.block_on(cli::healthcheck::run())) {
            Ok(Ok(())) => ExitCode::SUCCESS,
            Ok(Err(e)) => {
                eprintln!("{e:#}");
                ExitCode::FAILURE
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    let _guard = telemetry::init();
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "falha ao iniciar o runtime");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(&cmd)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(command = %cmd, error = format!("{e:#}"), "comando falhou");
            ExitCode::FAILURE
        }
    }
}

async fn run(cmd: &str) -> anyhow::Result<()> {
    match cmd {
        "bootstrap" => cli::bootstrap::run(&DbConfig::from_env()?).await,
        "migrate" => cli::migrate::run(&DbConfig::from_env()?).await,
        "seed" => cli::seed::run(&DbConfig::from_env()?).await,
        "worker" => {
            let db = DbConfig::from_env()?;
            app::spawn_metrics_endpoint(simple_core::config::env_parse(
                "WORKER_METRICS_BIND",
                "127.0.0.1:9091".parse::<std::net::SocketAddr>()?,
            )?)
            .await?;
            cli::worker::run(&db).await
        }
        "serve" => app::serve(ApiConfig::from_env()?, DbConfig::from_env()?).await,
        _ => anyhow::bail!("{USAGE}"),
    }
}
