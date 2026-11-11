//! Montagem do roteador, health checks, métricas e `api serve`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use axum::Router;
use axum::error_handling::HandleErrorLayer;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{Next, from_fn, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;

use crate::auth::middleware::{Access, RouteDef, enforce, route};
use crate::config::{ApiConfig, DbConfig};
use crate::error::problem;
use crate::state::AppState;

pub const BODY_LIMIT: usize = 16 * 1024;

/// Todas as rotas da API, cada uma com o acesso exigido declarado.
pub fn route_defs() -> Vec<RouteDef> {
    let mut defs = vec![
        route(Method::GET, "/health/live", Access::Public, get(live)),
        route(Method::GET, "/health/ready", Access::Public, get(ready)),
    ];
    defs.extend(crate::auth::routes::routes());
    defs.extend(crate::platform::routes::routes());
    defs.extend(crate::users::routes::routes());
    defs.extend(crate::access::routes::routes());
    defs
}

pub fn router(state: AppState) -> Router {
    let mut r: Router<AppState> = Router::new();
    for d in route_defs() {
        let mr = d
            .handler
            .route_layer(from_fn_with_state((state.clone(), d.access), enforce));
        r = r.route(d.path, mr);
    }
    let mut r = r.route_layer(from_fn(track_metrics));
    // Swagger UI fora do `enforce` e das métricas: conteúdo estático e público,
    // servido só com API_DOCS=true.
    if state.cfg.api_docs {
        r = r.merge(crate::openapi::docs_router());
    }
    r.fallback(|| async { problem(StatusCode::NOT_FOUND, None, None) })
        .method_not_allowed_fallback(|| async { problem(StatusCode::METHOD_NOT_ALLOWED, None, None) })
        .layer(
            ServiceBuilder::new()
                .layer(CatchPanicLayer::custom(|_| {
                    metrics::counter!("http_panics_total").increment(1);
                    problem(StatusCode::INTERNAL_SERVER_ERROR, None, None)
                }))
                .layer(HandleErrorLayer::new(|_: tower::BoxError| async {
                    metrics::counter!("http_timeouts_total").increment(1);
                    problem(StatusCode::SERVICE_UNAVAILABLE, Some("request timeout"), None)
                }))
                .timeout(state.cfg.request_timeout)
                .layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .with_state(state)
}

/// Liveness: o processo está de pé.
#[utoipa::path(get, path = "/health/live", tag = "saúde",
    responses((status = 200, description = "Processo ativo", body = crate::openapi::Health)))]
pub(crate) async fn live() -> impl IntoResponse {
    axum::Json(serde_json::json!({ "status": "ok" }))
}

/// Readiness: verifica apenas o banco `control`.
#[utoipa::path(get, path = "/health/ready", tag = "saúde",
    responses(
        (status = 200, description = "Pronto para receber tráfego", body = crate::openapi::Health),
        (status = 503, description = "Banco `control` indisponível", body = crate::openapi::Problem, content_type = "application/problem+json"),
    ))]
pub(crate) async fn ready(State(st): State<AppState>) -> Response {
    let ok = tokio::time::timeout(Duration::from_secs(1), sqlx::query("SELECT 1").execute(&st.control))
        .await
        .is_ok_and(|r| r.is_ok());
    if ok {
        axum::Json(serde_json::json!({ "status": "ready" })).into_response()
    } else {
        problem(
            StatusCode::SERVICE_UNAVAILABLE,
            Some("control database unavailable"),
            None,
        )
    }
}

/// Latência por rota. O label é o padrão da rota (cardinalidade limitada),
/// nunca o tenant.
async fn track_metrics(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let path = req.extensions().get::<MatchedPath>().map(|p| p.as_str().to_owned());
    let method = req.method().as_str().to_owned();
    let resp = next.run(req).await;
    if let Some(path) = path {
        let status = resp.status().as_u16().to_string();
        metrics::histogram!("http_request_duration_seconds", "method" => method, "route" => path, "status" => status)
            .record(start.elapsed().as_secs_f64());
    }
    resp
}

pub fn install_metrics() -> anyhow::Result<PrometheusHandle> {
    let recorder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Suffix("duration_seconds".into()),
            &[
                0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
            ],
        )?
        .build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder)?;
    Ok(handle)
}

/// Instala o recorder e expõe `/metrics` somente na porta interna.
pub async fn spawn_metrics_endpoint(bind: SocketAddr) -> anyhow::Result<PrometheusHandle> {
    let handle = install_metrics()?;
    let h = handle.clone();
    let metrics_app = Router::new().route("/metrics", get(move || std::future::ready(h.render())));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, metrics_app).await {
            tracing::error!(error = %e, "servidor de métricas falhou");
        }
    });
    Ok(handle)
}

fn spawn_stats(state: AppState, handle: PrometheusHandle) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        loop {
            tick.tick().await;
            state.pools.run_pending_tasks().await;
            let (pools, open, busy) = state.pools.connection_stats();
            metrics::gauge!("tenant_pools_cached").set(pools as f64);
            metrics::gauge!("tenant_pool_connections", "state" => "open").set(open as f64);
            metrics::gauge!("tenant_pool_connections", "state" => "busy").set(busy as f64);
            let size = state.control.size();
            metrics::gauge!("control_pool_connections", "state" => "open").set(f64::from(size));
            metrics::gauge!("control_pool_connections", "state" => "busy")
                .set(f64::from(size).max(0.0) - state.control.num_idle() as f64);
            handle.run_upkeep();
        }
    });
}

pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("sinal de encerramento recebido");
}

pub async fn serve(cfg: ApiConfig, db: DbConfig) -> anyhow::Result<()> {
    let bind = cfg.bind;
    let metrics_bind = cfg.metrics_bind;
    let handle = spawn_metrics_endpoint(metrics_bind).await?;
    let state = AppState::new(cfg, db).await?;
    state.check_connection_budget().await?;
    state.spawn_listener()?;
    spawn_stats(state.clone(), handle);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, %metrics_bind, "api ouvindo");
    axum::serve(
        listener,
        router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    state.control.close().await;
    tracing::info!("api encerrada");
    Ok(())
}
