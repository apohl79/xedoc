//! Serves the short-lived local model-router browser report.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::HeaderValue;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use xedoc_app_server_protocol::JSONRPCErrorError;
use xedoc_app_server_protocol::ModelRouterReportOpenResponse;
use xedoc_app_server_protocol::ModelRouterReportReadParams;

use crate::error_code::internal_error;
use crate::request_processors::ModelRouterReportRequestProcessor;

const CAPABILITY_TTL: Duration = Duration::from_secs(/*secs*/ 300);
const CAPABILITY_HEADER: &str = "x-xedoc-report-capability";
const CACHE_CONTROL: HeaderValue = HeaderValue::from_static("no-store");
const CONTENT_SECURITY_POLICY: HeaderValue = HeaderValue::from_static(
    "default-src 'none'; base-uri 'none'; connect-src 'self'; form-action 'none'; frame-ancestors 'none'; script-src 'self'; style-src 'self'",
);
const REFERRER_POLICY: HeaderValue = HeaderValue::from_static("no-referrer");

pub(crate) struct ModelRouterReportServer {
    processor: ModelRouterReportRequestProcessor,
    public_url: Option<String>,
    running: Mutex<Option<RunningReportServer>>,
    shutdown: CancellationToken,
}

struct RunningReportServer {
    address: SocketAddr,
    capability: Arc<Mutex<Capability>>,
}

struct Capability {
    value: String,
    expires_at: Instant,
}

#[derive(Clone)]
struct ReportServerState {
    capability: Arc<Mutex<Capability>>,
    origin: HeaderValue,
    processor: ModelRouterReportRequestProcessor,
}

struct ReportQuery {
    from_day: Option<i64>,
    through_day: Option<i64>,
    recent_limit: Option<u32>,
}

impl ModelRouterReportServer {
    pub(crate) fn new(
        processor: ModelRouterReportRequestProcessor,
        public_url: Option<String>,
    ) -> Self {
        Self {
            processor,
            public_url,
            running: Mutex::new(None),
            shutdown: CancellationToken::new(),
        }
    }

    pub(crate) async fn open(&self) -> Result<ModelRouterReportOpenResponse, JSONRPCErrorError> {
        let capability = Uuid::now_v7().to_string();
        if let Some(public_url) = &self.public_url {
            return Ok(ModelRouterReportOpenResponse {
                url: report_url(public_url, &capability),
            });
        }
        if self.shutdown.is_cancelled() {
            return Err(internal_error(
                "model-router report server is shutting down",
            ));
        }
        let mut running = self.running.lock().await;
        let running_server = match running.as_mut() {
            Some(running_server) => running_server,
            None => {
                let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|err| {
                    internal_error(format!("failed to bind model-router report: {err}"))
                })?;
                let address = listener.local_addr().map_err(|err| {
                    internal_error(format!("failed to inspect model-router report: {err}"))
                })?;
                let capability_state = Arc::new(Mutex::new(Capability {
                    value: capability.clone(),
                    expires_at: Instant::now() + CAPABILITY_TTL,
                }));
                let app = report_router(
                    Arc::clone(&capability_state),
                    address,
                    self.processor.clone(),
                );
                let shutdown_for_server = self.shutdown.clone();
                tokio::spawn(async move {
                    if let Err(err) = axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            shutdown_for_server.cancelled().await;
                        })
                        .await
                    {
                        tracing::warn!("model-router report server stopped: {err}");
                    }
                });
                running.insert(RunningReportServer {
                    address,
                    capability: capability_state,
                })
            }
        };
        let mut active_capability = running_server.capability.lock().await;
        active_capability.value = capability.clone();
        active_capability.expires_at = Instant::now() + CAPABILITY_TTL;
        Ok(ModelRouterReportOpenResponse {
            url: format!("http://{}/#capability={capability}", running_server.address),
        })
    }

    pub(crate) fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

fn report_url(public_url: &str, capability: &str) -> String {
    let base_url = public_url
        .split_once('#')
        .map_or(public_url, |(base_url, _)| base_url);
    format!("{base_url}#capability={capability}")
}

fn report_router(
    capability: Arc<Mutex<Capability>>,
    address: SocketAddr,
    processor: ModelRouterReportRequestProcessor,
) -> Router {
    Router::new()
        .route("/", get(report_page))
        .route("/report.css", get(report_css))
        .route("/report.js", get(report_js))
        .route("/api/report", get(read_report))
        .with_state(ReportServerState {
            capability,
            origin: HeaderValue::from_str(&format!("http://{address}"))
                .unwrap_or_else(|_| HeaderValue::from_static("http://127.0.0.1")),
            processor,
        })
}

async fn report_page() -> Response {
    static_response(
        include_str!("model_router_report_assets/report.html"),
        "text/html; charset=utf-8",
    )
}

async fn report_css() -> Response {
    static_response(
        include_str!("model_router_report_assets/report.css"),
        "text/css; charset=utf-8",
    )
}

async fn report_js() -> Response {
    static_response(
        include_str!("model_router_report_assets/report.js"),
        "text/javascript; charset=utf-8",
    )
}

async fn read_report(
    State(state): State<ReportServerState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !authorized(&state, &headers).await {
        return secured_response(StatusCode::FORBIDDEN.into_response());
    }
    let Some(query) = parse_report_query(uri.query()) else {
        return secured_response(StatusCode::BAD_REQUEST.into_response());
    };
    match state
        .processor
        .read(ModelRouterReportReadParams {
            from_day: query.from_day,
            through_day: query.through_day,
            recent_limit: query.recent_limit,
        })
        .await
    {
        Ok(report) => secured_response(Json(report).into_response()),
        Err(_) => secured_response(StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    }
}

fn parse_report_query(query: Option<&str>) -> Option<ReportQuery> {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
        .try_fold(
            ReportQuery {
                from_day: None,
                through_day: None,
                recent_limit: None,
            },
            |mut query, part| {
                let (key, value) = part.split_once('=')?;
                match key {
                    "fromDay" if query.from_day.is_none() => {
                        query.from_day = value.parse().ok();
                    }
                    "throughDay" if query.through_day.is_none() => {
                        query.through_day = value.parse().ok();
                    }
                    "recentLimit" if query.recent_limit.is_none() => {
                        query.recent_limit = value.parse().ok();
                    }
                    _ => return None,
                }
                Some(query)
            },
        )
}

async fn authorized(state: &ReportServerState, headers: &HeaderMap) -> bool {
    if headers
        .get(header::ORIGIN)
        .is_some_and(|origin| origin != state.origin)
    {
        return false;
    }
    let Some(capability) = headers
        .get(CAPABILITY_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let capability_state = state.capability.lock().await;
    capability_state.expires_at > Instant::now() && capability == capability_state.value
}

fn static_response(contents: &'static str, content_type: &'static str) -> Response {
    let mut response = contents.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    secured_response(response)
}

fn secured_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, CACHE_CONTROL);
    headers.insert(header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY);
    headers.insert(header::REFERRER_POLICY, REFERRER_POLICY);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}
