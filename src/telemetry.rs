//! Logs JSON com escrita não bloqueante. Sem log por requisição em `info`.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// O guard precisa viver até o fim do processo para descarregar o buffer.
pub fn init() -> WorkerGuard {
    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_current_span(false)
        .init();
    guard
}
