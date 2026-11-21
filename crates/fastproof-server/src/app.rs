//! Wiring: the platform, the routers, the listeners and the background tasks of `serve`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
use axum::http::{HeaderValue, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use fastproof_blob::BlobStore;
use fastproof_store::{PgHistoryStore, Store};
use franshub_access::AccessPolicy;
use franshub_auth::{Auth, Platform, PlatformSettings, StaticTokens};
use franshub_http::{ApiResult, SecurityHeaders, ServeOptions, security_headers};
use franshub_oidc::{Oidc, OidcRuntime, OidcSettings};
use franshub_store_pg::PgPlatformStore;
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::api::{self, ApiDoc};
use crate::config::{Config, MigrateMode};
use crate::live::LiveHub;
use crate::state::{AppState, Inner, Perms, ProjectScopes, catalog};

/// Concurrent event streams per replica.
pub const MAX_SSE: usize = 1000;

/// The API router and its OpenAPI document.
pub fn api_router() -> (Router<AppState>, utoipa::openapi::OpenApi) {
    let uploads = OpenApiRouter::<AppState>::new()
        .routes(routes!(api::uploads::create, api::uploads::list))
        .layer(DefaultBodyLimit::disable());
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(api::projects::list, api::projects::create))
        .routes(routes!(
            api::projects::get,
            api::projects::patch,
            api::projects::delete
        ))
        .routes(routes!(api::launches::create, api::launches::list))
        .routes(routes!(api::launches::get, api::launches::delete))
        .routes(routes!(api::launches::finish))
        .routes(routes!(api::results::ingest, api::results::list))
        .routes(routes!(api::results::tree))
        .routes(routes!(api::results::detail))
        .routes(routes!(api::results::history))
        .routes(routes!(api::attachments::upload))
        .routes(routes!(api::attachments::content))
        .routes(routes!(api::events::stream))
        .routes(routes!(api::admin::audit))
        .routes(routes!(api::admin::jobs))
        .routes(routes!(api::integrations::gate))
        .routes(routes!(api::integrations::secrets))
        .routes(routes!(
            api::integrations::put_secret,
            api::integrations::delete_secret
        ))
        .routes(routes!(api::integrations::deliveries))
        .routes(routes!(api::integrations::mock_interactions))
        .merge(uploads)
        .split_for_parts()
}

/// The OpenAPI document as committed in `schemas/openapi.json`.
pub fn openapi_text() -> String {
    let (_, doc) = api_router();
    let mut s = doc.to_pretty_json().unwrap_or_default();
    s.push('\n');
    s
}

static OPENAPI: std::sync::LazyLock<String> = std::sync::LazyLock::new(openapi_text);

async fn openapi_json(_auth: Auth) -> ApiResult<Response> {
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        OPENAPI.as_str(),
    )
        .into_response())
}

async fn docs_csp(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    r.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    );
    r
}

/// The main router: API, platform routes, OIDC, docs, `/healthz` and the UI.
pub fn router(state: AppState) -> Router {
    let (api, _) = api_router();
    let swagger: Router = Router::from(
        utoipa_swagger_ui::SwaggerUi::new("/api/docs")
            .config(utoipa_swagger_ui::Config::new(["/api/openapi.json"])),
    )
    .layer(middleware::from_fn(docs_csp));
    let mut app: Router<AppState> = Router::new()
        .merge(api)
        .merge(franshub_auth::routes::<AppState>())
        .merge(state.platform.oidc_router())
        .route("/api/openapi.json", get(openapi_json))
        .merge(crate::mcp::router(&state))
        .route("/healthz", get(|| async { "ok" }));
    if state.cfg.ui {
        app = app
            .merge(crate::trace::router::<AppState>())
            .merge(crate::ui::router::<AppState>());
    }
    let app = app.with_state(state.clone());
    app.merge(swagger)
        .layer(middleware::from_fn_with_state(
            Arc::new(SecurityHeaders::default()),
            security_headers,
        ))
        .layer(middleware::from_fn(franshub_observe::http_metrics))
        .layer(middleware::from_fn(franshub_observe::request_id))
}

/// A running server.
pub struct Running {
    pub addr: SocketAddr,
    pub ops_addr: SocketAddr,
    pub state: AppState,
    stop: CancellationToken,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    /// Stops the listeners and background tasks and waits for them.
    pub async fn shutdown(self) {
        self.stop.cancel();
        for t in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(20), t).await;
        }
    }

    /// Completes when the server was asked to stop.
    pub fn stopped(&self) -> CancellationToken {
        self.stop.clone()
    }
}

/// Builds the platform: access policy, static tokens, managed tokens, OIDC, audit, scopes.
pub async fn platform(
    cfg: &Config,
    store: &Store,
    pg: Arc<PgPlatformStore>,
) -> anyhow::Result<Arc<Platform>> {
    let catalog = Arc::new(catalog()?);
    let policy = Arc::new(match &cfg.access_file {
        Some(p) => AccessPolicy::load(catalog, p)
            .map_err(|e| anyhow::anyhow!("FASTPROOF_ACCESS_FILE: {e}"))?,
        None => AccessPolicy::new(catalog),
    });
    let mut b = Platform::builder(policy.clone())
        .managed_tokens(pg.clone(), "fp_")
        .audit(pg.clone())
        .scopes(Arc::new(ProjectScopes(store.clone())))
        .settings(PlatformSettings {
            allowed_origins: cfg
                .public_url
                .as_ref()
                .map(|u| vec![u.origin().ascii_serialization()])
                .unwrap_or_default(),
            ..PlatformSettings::default()
        });
    if let Some(f) = &cfg.admin_tokens_file {
        b = b.static_tokens(
            StaticTokens::from_file(f, &policy).context("FASTPROOF_ADMIN_TOKENS_FILE")?,
        );
    }
    if let Some(o) = &cfg.oidc {
        let settings = OidcSettings::new(o.clone()).context("FASTPROOF_OIDC_*")?;
        let oidc = Oidc::new(settings).context("FASTPROOF_OIDC_*")?;
        let rt = Arc::new(
            OidcRuntime::new(oidc, pg.clone(), policy.clone())
                .with_audit(pg.clone())
                .with_allowed_origins(
                    cfg.public_url
                        .as_ref()
                        .map(|u| vec![u.origin().ascii_serialization()])
                        .unwrap_or_default(),
                ),
        );
        rt.start().await.context("starting OIDC")?;
        b = b.oidc(rt);
    }
    Ok(Arc::new(b.build().context(
        "set FASTPROOF_ADMIN_TOKENS_FILE or FASTPROOF_OIDC_ISSUER",
    )?))
}

/// Connects, migrates (or checks), builds the state and starts listeners, workers and the
/// scheduler. `prometheus` serves `/metrics` on the ops listener.
pub async fn start(cfg: Config, prometheus: Option<PrometheusHandle>) -> anyhow::Result<Running> {
    fastproof_blob::install_crypto_provider();
    let env = |k: &str| std::env::var(k).ok();
    let pool = cfg.db.connect(&env).await?;
    start_with_pool(cfg, pool, prometheus).await
}

/// As [`start`], on an existing pool.
pub async fn start_with_pool(
    cfg: Config,
    pool: PgPool,
    prometheus: Option<PrometheusHandle>,
) -> anyhow::Result<Running> {
    fastproof_blob::install_crypto_provider();
    match cfg.migrate {
        MigrateMode::Auto => crate::db::migrate(&pool).await?,
        MigrateMode::Off => {
            franshub_store_pg::check_schema(&pool).await?;
            fastproof_store::check_schema(&pool).await?;
        }
    }
    if cfg.valkey_url.is_some() {
        tracing::warn!(
            "FASTPROOF_VALKEY_URL is set but Valkey fan-out is not implemented yet: live updates use PostgreSQL LISTEN/NOTIFY"
        );
    }
    {
        let pool = pool.clone();
        tokio::spawn(async move {
            fastproof_store::search::ensure_trigram(&pool).await;
        });
    }
    for sub in ["incoming", "work", "attachments"] {
        let d = cfg.spool_dir.join(sub);
        std::fs::create_dir_all(&d)
            .with_context(|| format!("FASTPROOF_SPOOL_DIR: creating {}", d.display()))?;
    }
    let blobs = BlobStore::open(&cfg.blob).context("opening the blob store")?;
    if let Err(e) = blobs.check().await {
        tracing::error!(error = %format!("{e:#}"), "the blob store is not reachable; readiness will fail until it is");
    }
    let store = Store::new(pool.clone());
    let pg = Arc::new(PgPlatformStore::new(pool.clone()));
    let platform = platform(&cfg, &store, pg).await?;
    let instance = format!(
        "{}-{:08x}",
        std::env::var("HOSTNAME")
            .unwrap_or_else(|_| "fastproof".into())
            .chars()
            .take(40)
            .collect::<String>(),
        rand::random::<u32>()
    );
    let limits = cfg.limits.clone();
    let state = AppState(Arc::new(Inner {
        perms: Perms::new(&platform),
        platform,
        history: Arc::new(PgHistoryStore::new(pool.clone())),
        store,
        blobs,
        limits,
        live: LiveHub::new(),
        local_uploads: Mutex::new(Default::default()),
        wake: Arc::new(Notify::new()),
        sse: Arc::new(Semaphore::new(MAX_SSE)),
        instance,
        egress: crate::integrations::egress::Egress::new(cfg.integrations.egress.clone())?,
        analysis: Arc::new(crate::integrations::analysis::Fallback),
        cfg: Arc::new(cfg),
    }));
    let stop = CancellationToken::new();
    let mut tasks = Vec::new();

    let listener = tokio::net::TcpListener::bind(state.cfg.listen)
        .await
        .with_context(|| format!("FASTPROOF_LISTEN: binding {}", state.cfg.listen))?;
    let ops_listener = tokio::net::TcpListener::bind(state.cfg.ops_listen)
        .await
        .with_context(|| format!("FASTPROOF_OPS_LISTEN: binding {}", state.cfg.ops_listen))?;
    let addr = listener.local_addr()?;
    let ops_addr = ops_listener.local_addr()?;
    let tls = match &state.cfg.tls {
        Some(t) => Some(franshub_http::server_config(t).context("FASTPROOF_TLS_*")?),
        None => None,
    };
    let app = router(state.clone());
    {
        let stop = stop.clone();
        tasks.push(tokio::spawn(async move {
            franshub_http::serve(
                listener,
                app,
                tls,
                stop.cancelled_owned(),
                ServeOptions::default(),
            )
            .await;
        }));
    }
    {
        let ops = crate::ops::router(state.clone(), prometheus);
        let stop = stop.clone();
        tasks.push(tokio::spawn(async move {
            franshub_http::serve(
                ops_listener,
                ops,
                None,
                stop.cancelled_owned(),
                ServeOptions::default(),
            )
            .await;
        }));
    }
    tasks.push(tokio::spawn(crate::live::listen(
        pool.clone(),
        state.live.clone(),
        state.wake.clone(),
        stop.clone(),
    )));
    for n in 0..state.cfg.ingest_workers {
        tasks.push(tokio::spawn(crate::ingest::worker(
            state.clone(),
            n,
            stop.clone(),
        )));
    }
    tasks.push(tokio::spawn(crate::integrations::dispatch::run(
        state.clone(),
        stop.clone(),
    )));
    let sched = crate::scheduler::standard_jobs()?;
    tasks.push(tokio::spawn(sched.run(state.clone(), stop.clone())));
    {
        let s = state.clone();
        let stop = stop.clone();
        tasks.push(tokio::spawn(
            async move { local_maintenance(s, stop).await },
        ));
    }
    tracing::info!(%addr, %ops_addr, ui = crate::ui::embedded(), blobs = state.blobs.backend(), workers = state.cfg.ingest_workers, "FastProof is serving");
    Ok(Running {
        addr,
        ops_addr,
        state,
        stop,
        tasks,
    })
}

/// Per-replica housekeeping: queue gauges, managed token retention, stale spool files.
async fn local_maintenance(s: AppState, stop: CancellationToken) {
    let mut tick = 0u64;
    loop {
        if tick.is_multiple_of(360) {
            s.platform.maintain().await;
            crate::ingest::clean_local_spool(&s.cfg.spool_dir, Duration::from_secs(6 * 3600)).await;
        }
        if let Ok(d) = s.store.queue_depth().await {
            metrics::gauge!("fastproof_ingest_jobs", "state" => "queued").set(d.queued as f64);
            metrics::gauge!("fastproof_ingest_jobs", "state" => "running").set(d.running as f64);
            metrics::gauge!("fastproof_ingest_jobs", "state" => "failed").set(d.failed as f64);
        }
        tick += 1;
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    }
}
