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
    let (router, mut doc) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(api::projects::list, api::projects::create))
        .routes(routes!(
            api::projects::get,
            api::projects::patch,
            api::projects::delete
        ))
        .routes(routes!(api::launches::create, api::launches::list))
        .routes(routes!(
            api::launches::get,
            api::launches::delete,
            api::analysis::patch_launch
        ))
        .routes(routes!(api::launches::finish))
        .routes(routes!(api::results::ingest, api::results::list))
        .routes(routes!(api::results::tree))
        .routes(routes!(api::results::tree_tests))
        .routes(routes!(api::results::labels))
        .routes(routes!(api::results::facets))
        .routes(routes!(api::results::environments))
        .routes(routes!(api::results::detail))
        .routes(routes!(api::results::history))
        .routes(routes!(api::attachments::upload))
        .routes(routes!(
            api::attachments::content,
            api::attachments::content_head
        ))
        .routes(routes!(api::media::create))
        .routes(routes!(api::media::get, api::media::head))
        .routes(routes!(api::events::stream))
        .routes(routes!(api::logs::ingest, api::logs::list))
        .routes(routes!(api::logs::stream))
        .routes(routes!(api::search::logs))
        .routes(routes!(api::search::similar))
        .routes(routes!(api::admin::audit))
        .routes(routes!(api::admin::jobs))
        .routes(routes!(api::version::get))
        .routes(routes!(api::integrations::gate))
        .routes(routes!(api::integrations::secrets))
        .routes(routes!(
            api::integrations::put_secret,
            api::integrations::delete_secret
        ))
        .routes(routes!(api::integrations::deliveries))
        .routes(routes!(api::integrations::test_notification))
        .routes(routes!(api::integrations::mock_interactions))
        .routes(routes!(api::reruns::create))
        .routes(routes!(api::reruns::list))
        .routes(routes!(
            api::analysis::defect_types,
            api::analysis::create_defect_type
        ))
        .routes(routes!(
            api::analysis::patch_defect_type,
            api::analysis::delete_defect_type
        ))
        .routes(routes!(api::analysis::triage))
        .routes(routes!(api::analysis::clusters))
        .routes(routes!(api::analysis::rules, api::analysis::create_rule))
        .routes(routes!(api::analysis::test_rule))
        .routes(routes!(
            api::analysis::patch_rule,
            api::analysis::delete_rule
        ))
        .routes(routes!(api::analysis::apply_rule))
        .routes(routes!(api::analysis::task))
        .routes(routes!(api::analysis::flaky))
        .routes(routes!(
            api::analysis::get_settings,
            api::analysis::put_settings
        ))
        .routes(routes!(api::analysis::retention_preview))
        .routes(routes!(api::analysis::compare))
        .routes(routes!(api::analysis::dashboard))
        .routes(routes!(api::analysis::create_test_plan))
        .routes(routes!(api::analysis::testplan_json))
        .merge(uploads)
        .merge(api::cases::router())
        .merge(api::case_folders::router())
        .merge(api::case_fields::router())
        .merge(api::plans::router())
        .merge(api::dashboards::router())
        .merge(api::runs::router())
        .merge(api::imports::router())
        .merge(api::exports::router())
        .merge(api::launch_exports::router())
        .merge(api::merges::router())
        .split_for_parts();
    api::type_project_settings(&mut doc);
    api::admin::document_platform(&mut doc);
    (router, doc)
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

/// How the UI signs in: `{oidc}` tells whether single sign-on is configured; API tokens always
/// work. Public, like the sign-in page that reads it.
async fn auth_config(axum::extract::State(s): axum::extract::State<AppState>) -> Response {
    (
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        axum::Json(serde_json::json!({ "oidc": s.cfg.oidc.is_some() })),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct LoginQuery {
    #[serde(rename = "return")]
    to: Option<String>,
}

/// Without single sign-on, `/auth/login` sends the browser to the UI's sign-in page, where an API
/// token can be used; the return path is kept only when it is a safe local path.
async fn sign_in_redirect(axum::extract::Query(q): axum::extract::Query<LoginQuery>) -> Response {
    let target = match q
        .to
        .filter(|r| r.len() <= 2048 && franshub_oidc::safe_return(r, "/"))
    {
        Some(r) => format!(
            "/sign-in?return={}",
            url::form_urlencoded::byte_serialize(r.as_bytes()).collect::<String>()
        ),
        None => "/sign-in".to_string(),
    };
    (
        axum::http::StatusCode::SEE_OTHER,
        [
            (
                header::LOCATION,
                HeaderValue::from_str(&target).unwrap_or(HeaderValue::from_static("/sign-in")),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
    )
        .into_response()
}

/// The `/auth/*` routes without single sign-on: sign-in goes to the token sign-in page; the
/// callback and logout answer 404 `sso_disabled`.
fn token_sign_in_router() -> Router<AppState> {
    let off = || async {
        franshub_http::ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            "sso_disabled",
            "single sign-on is not configured",
        )
    };
    Router::new()
        .route("/auth/login", get(sign_in_redirect))
        .route("/auth/callback", get(off))
        .route("/auth/logout", axum::routing::post(off))
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
        .merge(if state.cfg.oidc.is_some() {
            state.platform.oidc_router()
        } else {
            token_sign_in_router()
        })
        .route("/auth/config", get(auth_config))
        .route("/api/openapi.json", get(openapi_json))
        .merge(crate::mcp::router(&state))
        .route("/healthz", get(|| async { "ok" }));
    if state.cfg.ui {
        app = app
            .merge(crate::trace::router::<AppState>())
            .merge(crate::ui::router::<AppState>());
    }
    let app = app.with_state(state.clone());
    let inner = app
        .merge(swagger)
        .layer(middleware::from_fn(crate::limits::catch_panic))
        .layer(middleware::from_fn_with_state(
            state.cfg.guards.body_idle_timeout,
            crate::limits::body_idle_timeout,
        ))
        .layer(middleware::from_fn_with_state(
            Arc::new(SecurityHeaders::default()),
            security_headers,
        ))
        .layer(middleware::from_fn(franshub_observe::http_metrics))
        .layer(middleware::from_fn(franshub_observe::request_id));
    Router::new()
        .fallback_service(inner)
        .layer(middleware::from_fn(api::media::redact_path))
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
    /// Stops the listeners and background tasks and waits for them, all at once and for at
    /// most [`crate::Config::shutdown_timeout`]: the listeners stop accepting, requests in
    /// flight finish, event streams end and workers finish the batch they hold.
    pub async fn shutdown(self) {
        let started = std::time::Instant::now();
        let timeout = self.state.cfg.shutdown_timeout;
        self.stop.cancel();
        let tasks = self.tasks.len();
        let all = futures::future::join_all(self.tasks);
        match tokio::time::timeout(timeout, all).await {
            Ok(_) => tracing::info!(ms = started.elapsed().as_millis() as u64, tasks, "stopped"),
            Err(_) => tracing::warn!(
                seconds = timeout.as_secs(),
                "stopping took longer than FASTPROOF_SHUTDOWN_TIMEOUT_SECONDS; exiting with work still running"
            ),
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

/// Listener options: open connections get the shutdown timeout, less a second for the rest of
/// the stop, to finish.
fn serve_options(cfg: &Config) -> ServeOptions {
    ServeOptions {
        drain: cfg
            .shutdown_timeout
            .saturating_sub(Duration::from_secs(1))
            .max(Duration::from_secs(1)),
        ..ServeOptions::default()
    }
}

/// The ops listener started before anything else, so probes get answers while start-up waits.
struct Boot {
    stop: CancellationToken,
    ops_addr: SocketAddr,
    ops_task: JoinHandle<()>,
    started: crate::ops::Started,
}

async fn boot(cfg: &Config, prometheus: Option<PrometheusHandle>) -> anyhow::Result<Boot> {
    let ops_listener = tokio::net::TcpListener::bind(cfg.ops_listen)
        .await
        .with_context(|| format!("FASTPROOF_OPS_LISTEN: binding {}", cfg.ops_listen))?;
    let ops_addr = ops_listener.local_addr()?;
    let stop = CancellationToken::new();
    let started = crate::ops::Started::default();
    let ops = crate::ops::router(started.clone(), prometheus);
    let opts = serve_options(cfg);
    let ops_task = {
        let stop = stop.clone();
        tokio::spawn(async move {
            franshub_http::serve(ops_listener, ops, None, stop.cancelled_owned(), opts).await;
        })
    };
    Ok(Boot {
        stop,
        ops_addr,
        ops_task,
        started,
    })
}

/// Connects (waiting for PostgreSQL up to `FASTPROOF_DATABASE_WAIT`),
/// migrates (or checks), builds the state and starts listeners, workers and the scheduler.
/// `prometheus` serves `/metrics` on the ops listener, which answers from the start.
pub async fn start(cfg: Config, prometheus: Option<PrometheusHandle>) -> anyhow::Result<Running> {
    fastproof_blob::install_crypto_provider();
    let boot = boot(&cfg, prometheus).await?;
    let env = |k: &str| std::env::var(k).ok();
    let pool = match cfg.db.connect_waiting(&env).await {
        Ok(p) => p,
        Err(e) => {
            boot.stop.cancel();
            return Err(e);
        }
    };
    start_booted(cfg, pool, boot).await
}

/// As [`start`], on an existing pool.
pub async fn start_with_pool(
    cfg: Config,
    pool: PgPool,
    prometheus: Option<PrometheusHandle>,
) -> anyhow::Result<Running> {
    fastproof_blob::install_crypto_provider();
    let boot = boot(&cfg, prometheus).await?;
    start_booted(cfg, pool, boot).await
}

async fn start_booted(cfg: Config, pool: PgPool, boot: Boot) -> anyhow::Result<Running> {
    let stop = boot.stop.clone();
    let r = start_inner(cfg, pool, boot).await;
    if r.is_err() {
        stop.cancel();
    }
    r
}

async fn start_inner(cfg: Config, pool: PgPool, boot: Boot) -> anyhow::Result<Running> {
    match cfg.migrate {
        MigrateMode::Auto => crate::db::migrate(&pool).await?,
        MigrateMode::Off => {
            crate::db::wait_for(
                "the database schema",
                "FASTPROOF_DATABASE_WAIT",
                cfg.db.connect_timeout,
                || async {
                    franshub_store_pg::check_schema(&pool).await?;
                    fastproof_store::check_schema(&pool).await
                },
            )
            .await?;
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
            match crate::db::unbounded(&pool).await {
                Ok(p) => {
                    fastproof_store::search::ensure_trigram(&p).await;
                    p.close().await;
                }
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "connecting for the trigram index failed")
                }
            }
        });
    }
    for sub in ["incoming", "work", "attachments", "media", "launch-exports"] {
        let d = cfg.spool_dir.join(sub);
        std::fs::create_dir_all(&d)
            .with_context(|| format!("FASTPROOF_SPOOL_DIR: creating {}", d.display()))?;
    }
    crate::launch_export::sweep_spool(&cfg.spool_dir);
    let blobs = BlobStore::open(&cfg.blob).context("opening the blob store")?;
    if let Err(e) = blobs.check().await {
        tracing::error!(error = %format!("{e:#}"), "the blob store is not reachable; readiness will fail until it is");
    }
    let store = Store::new(pool.clone()).with_log_chunk_events(cfg.log_chunk_events);
    let pg = Arc::new(PgPlatformStore::new(pool.clone()));
    let platform = platform(&cfg, &store, pg.clone()).await?;
    let sessions = platform.oidc().map(|rt| {
        (
            pg.clone() as Arc<dyn franshub_oidc::session::SessionStore>,
            franshub_oidc::session::Lifetimes {
                ttl: rt.oidc.settings.session_ttl,
                idle: rt.oidc.settings.session_idle,
            },
        )
    });
    let media = crate::api::media::Media::load(&store, &cfg, sessions).await?;
    crate::blob_guard::check(&store, &blobs, &cfg.blob, cfg.blob_migrated).await?;
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
    let kafka = match &cfg.kafka {
        Some(k) if cfg.ingest_mode == crate::config::IngestMode::Kafka => crate::db::wait_for(
            "Kafka",
            "FASTPROOF_KAFKA_CONNECT_TIMEOUT_SECONDS",
            k.connect_timeout,
            || crate::stream::connect_kafka(&cfg, &store),
        )
        .await
        .context("FASTPROOF_KAFKA_*")?,
        _ => None,
    };
    let search = crate::stream::search_backend(&cfg)?;
    let state = AppState(Arc::new(Inner {
        media,
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
        analysis: Arc::new(crate::integrations::analysis::Full),
        logs: crate::stream::live::LogHub::new(),
        kafka,
        search,
        guards: crate::limits::Guards::new(cfg.guards, cfg.db.heavy_slots()),
        stopping: boot.stop.clone(),
        cfg: Arc::new(cfg),
    }));
    let sched = if state.cfg.roles.scheduler {
        let mut sched = crate::scheduler::standard_jobs()?;
        sched.register_all(&state.store).await?;
        Some(sched)
    } else {
        None
    };
    let stop = boot.stop.clone();
    let mut tasks = vec![boot.ops_task];

    let listener = tokio::net::TcpListener::bind(state.cfg.listen)
        .await
        .with_context(|| format!("FASTPROOF_LISTEN: binding {}", state.cfg.listen))?;
    let addr = listener.local_addr()?;
    let ops_addr = boot.ops_addr;
    let tls = match &state.cfg.tls {
        Some(t) => Some(franshub_http::server_config(t).context("FASTPROOF_TLS_*")?),
        None => None,
    };
    let first = crate::live::first_subscription(&pool, crate::live::CHANNELS).await;
    tasks.push(tokio::spawn(crate::live::listen(
        pool.clone(),
        state.live.clone(),
        state.wake.clone(),
        stop.clone(),
        first,
    )));
    if state.cfg.roles.ingest && state.kafka.is_none() {
        for n in 0..state.cfg.ingest_workers {
            tasks.push(tokio::spawn(crate::ingest::worker(
                state.clone(),
                n,
                stop.clone(),
            )));
        }
    }
    if let Some(sched) = sched {
        tasks.push(tokio::spawn(crate::integrations::dispatch::run(
            state.clone(),
            stop.clone(),
        )));
        tasks.push(tokio::spawn(sched.run(state.clone(), stop.clone())));
    }
    tasks.push(tokio::spawn(crate::api::media::reload_keys(
        state.clone(),
        stop.clone(),
    )));
    tasks.extend(crate::stream::start(&state, &stop).await?);
    {
        let s = state.clone();
        let stop = stop.clone();
        tasks.push(tokio::spawn(
            async move { local_maintenance(s, stop).await },
        ));
    }
    // The API serves once the listeners are subscribed, so that a client never misses the
    // notifications of what it writes.
    let app = router(state.clone());
    if state.cfg.roles.api {
        let stop = stop.clone();
        let opts = serve_options(&state.cfg);
        tasks.push(tokio::spawn(async move {
            franshub_http::serve(listener, app, tls, stop.cancelled_owned(), opts).await;
        }));
    } else {
        drop(listener);
    }
    let _ = boot.started.set(state.clone());
    tracing::info!(%addr, %ops_addr, ui = crate::ui::embedded(), blobs = state.blobs.backend(), workers = state.cfg.ingest_workers,
        mode = ?state.cfg.ingest_mode, roles = ?state.cfg.roles, "FastProof is serving");
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
            crate::media_prepare::clean_spool(
                &s.cfg.spool_dir,
                s.cfg.media_prepare.timeout + Duration::from_secs(300),
            )
            .await;
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
