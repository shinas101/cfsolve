//! High-performance HTTP API Server for cfsolve.
//!
//! Provides a fast, zero-overhead REST API for solving Cloudflare Turnstile widgets
//! and clearing challenge pages over the internet.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use axum::Router;
use cfsolve::{Browser, Options, Outcome, PageSource, Sink, Turnstile, Url, Widget};
use serde::{Deserialize, Serialize};
use tower_http::cors::{Any, CorsLayer};

/// Optional collector for when callers explicitly ask for debug logs.
#[derive(Default)]
struct CollectingSink {
    logs: Mutex<Vec<String>>,
}

impl Sink for CollectingSink {
    fn artifact(&self, _name: &str, _contents: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }

    fn progress(&self, message: &str) {
        if let Ok(mut logs) = self.logs.lock() {
            logs.push(message.to_string());
        }
    }
}

/// Shared application state across requests.
#[derive(Clone)]
struct AppState {
    browser: Arc<Browser>,
    default_proxy: Option<String>,
    profile_path: PathBuf,
    user_agent: String,
    started_at: Instant,
}

#[derive(Serialize)]
struct CookieItem {
    name: String,
    value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

#[inline]
fn cookie_items_and_header(cookies: &[cfsolve::Cookie<'static>]) -> (Vec<CookieItem>, String) {
    let mut items = Vec::with_capacity(cookies.len());
    let mut header_parts = Vec::with_capacity(cookies.len());
    for c in cookies {
        items.push(CookieItem {
            name: c.name().to_string(),
            value: c.value().to_string(),
            domain: c.domain().map(ToString::to_string),
            path: c.path().map(ToString::to_string),
        });
        header_parts.push(format!("{}={}", c.name(), c.value()));
    }
    let header = header_parts.join("; ");
    (items, header)
}

#[inline]
fn build_options(
    request_proxy: Option<String>,
    default_proxy: Option<&str>,
    sink: Option<Arc<dyn Sink>>,
) -> Options {
    let proxy = request_proxy.or_else(|| default_proxy.map(String::from));
    Options {
        proxy,
        trusted_root: None,
        sink,
    }
}

// -----------------------------------------------------------------------------
// Request / Response Models
// -----------------------------------------------------------------------------

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    profile: String,
    user_agent: String,
    uptime_seconds: u64,
    endpoints: &'static [&'static str],
}

#[derive(Deserialize)]
struct SolveRequest {
    url: String,
    proxy: Option<String>,
    #[serde(default)]
    verbose: bool,
}

#[derive(Serialize)]
struct SolveResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    r#type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cookies: Option<Vec<CookieItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cookie_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    logs: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct TurnstileRequest {
    url: String,
    sitekey: Option<String>,
    action: Option<String>,
    cdata: Option<String>,
    #[serde(default = "default_mode")]
    mode: String,
    html: Option<String>,
    proxy: Option<String>,
    #[serde(default)]
    verbose: bool,
}

fn default_mode() -> String {
    "navigate".to_string()
}

#[derive(Serialize)]
struct TurnstileResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    logs: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ChallengeRequest {
    url: String,
    proxy: Option<String>,
    #[serde(default)]
    verbose: bool,
}

#[derive(Serialize)]
struct ChallengeResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    clearance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cookies: Option<Vec<CookieItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cookie_header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    logs: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct FlareSolverrRequest {
    cmd: String,
    url: Option<String>,
    proxy: Option<FlareSolverrProxy>,
    #[serde(rename = "maxTimeout")]
    _max_timeout: Option<u64>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FlareSolverrProxy {
    Url(String),
    Object { url: String },
}

impl FlareSolverrProxy {
    fn into_url(self) -> String {
        match self {
            FlareSolverrProxy::Url(u) => u,
            FlareSolverrProxy::Object { url } => url,
        }
    }
}

#[derive(Serialize)]
struct FlareSolverrResponse {
    status: &'static str,
    message: String,
    #[serde(rename = "startTimestamp")]
    start_timestamp: u64,
    #[serde(rename = "endTimestamp")]
    end_timestamp: u64,
    version: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    solution: Option<FlareSolverrSolution>,
}

#[derive(Serialize)]
struct FlareSolverrSolution {
    url: String,
    status: u16,
    headers: serde_json::Value,
    response: String,
    cookies: Vec<CookieItem>,
    #[serde(rename = "userAgent")]
    user_agent: String,
}

// -----------------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------------

async fn health_handler(State(state): State<AppState>) -> Json<HealthResponse> {
    const ENDPOINTS: &[&str] = &[
        "GET  /health",
        "POST /solve          (Universal challenge & Turnstile solver)",
        "POST /turnstile      (Dedicated Turnstile widget solver)",
        "POST /challenge      (Dedicated challenge page clearer)",
        "POST /v1             (FlareSolverr compatible solver)",
    ];
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        profile: state.profile_path.display().to_string(),
        user_agent: state.user_agent.clone(),
        uptime_seconds: state.started_at.elapsed().as_secs(),
        endpoints: ENDPOINTS,
    })
}

async fn solve_handler(
    State(state): State<AppState>,
    Json(req): Json<SolveRequest>,
) -> (StatusCode, Json<SolveResponse>) {
    let started = Instant::now();
    let sink: Option<Arc<CollectingSink>> = if req.verbose {
        Some(Arc::new(CollectingSink::default()))
    } else {
        None
    };

    let options = build_options(
        req.proxy,
        state.default_proxy.as_deref(),
        sink.clone().map(|s| s as Arc<dyn Sink>),
    );

    let parsed_url = match Url::parse(&req.url) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(SolveResponse {
                    success: false,
                    r#type: None,
                    token: None,
                    clearance: None,
                    cookies: None,
                    cookie_header: None,
                    page_url: None,
                    html: None,
                    user_agent: None,
                    error: Some(format!("Invalid URL: {e}")),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
                }),
            );
        }
    };

    match state.browser.solve(&parsed_url, &options).await {
        Ok(Outcome::Token(token)) => (
            StatusCode::OK,
            Json(SolveResponse {
                success: true,
                r#type: Some("turnstile"),
                token: Some(token),
                clearance: None,
                cookies: None,
                cookie_header: None,
                page_url: Some(req.url),
                html: None,
                user_agent: Some(state.user_agent.clone()),
                error: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
                logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
            }),
        ),
        Ok(Outcome::Cleared(clearance)) => {
            let (cookie_items, cookie_header) = cookie_items_and_header(&clearance.cookies);
            (
                StatusCode::OK,
                Json(SolveResponse {
                    success: true,
                    r#type: Some("clearance"),
                    token: None,
                    clearance: Some(clearance.clearance),
                    cookies: Some(cookie_items),
                    cookie_header: Some(cookie_header),
                    page_url: Some(clearance.page.url.to_string()),
                    html: Some(clearance.page.html),
                    user_agent: Some(state.user_agent.clone()),
                    error: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
                }),
            )
        }
        Err(err) => (
            StatusCode::OK,
            Json(SolveResponse {
                success: false,
                r#type: None,
                token: None,
                clearance: None,
                cookies: None,
                cookie_header: None,
                page_url: None,
                html: None,
                user_agent: Some(state.user_agent.clone()),
                error: Some(format!("{err:#}")),
                elapsed_ms: started.elapsed().as_millis() as u64,
                logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
            }),
        ),
    }
}

async fn turnstile_handler(
    State(state): State<AppState>,
    Json(req): Json<TurnstileRequest>,
) -> (StatusCode, Json<TurnstileResponse>) {
    let started = Instant::now();
    let sink: Option<Arc<CollectingSink>> = if req.verbose {
        Some(Arc::new(CollectingSink::default()))
    } else {
        None
    };

    let options = build_options(
        req.proxy,
        state.default_proxy.as_deref(),
        sink.clone().map(|s| s as Arc<dyn Sink>),
    );

    let parsed_url = match Url::parse(&req.url) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(TurnstileResponse {
                    success: false,
                    token: None,
                    error: Some(format!("Invalid URL: {e}")),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
                }),
            );
        }
    };

    let widget = req.sitekey.map(|sitekey| Widget {
        sitekey,
        action: req.action,
        cdata: req.cdata,
    });

    let page = if req.mode.eq_ignore_ascii_case("blank") {
        if widget.is_none() {
            return (
                StatusCode::BAD_REQUEST,
                Json(TurnstileResponse {
                    success: false,
                    token: None,
                    error: Some("Blank page mode requires a sitekey".to_string()),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
                }),
            );
        }
        PageSource::Blank
    } else if let Some(html) = req.html {
        PageSource::Html {
            html,
            referrer_policy: None,
        }
    } else {
        PageSource::Navigate
    };

    let turnstile = Turnstile {
        url: parsed_url,
        page,
        widget,
    };

    match state.browser.solve_turnstile(turnstile, &options).await {
        Ok(token) => (
            StatusCode::OK,
            Json(TurnstileResponse {
                success: true,
                token: Some(token),
                error: None,
                elapsed_ms: started.elapsed().as_millis() as u64,
                logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
            }),
        ),
        Err(err) => (
            StatusCode::OK,
            Json(TurnstileResponse {
                success: false,
                token: None,
                error: Some(format!("{err:#}")),
                elapsed_ms: started.elapsed().as_millis() as u64,
                logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
            }),
        ),
    }
}

async fn challenge_handler(
    State(state): State<AppState>,
    Json(req): Json<ChallengeRequest>,
) -> (StatusCode, Json<ChallengeResponse>) {
    let started = Instant::now();
    let sink: Option<Arc<CollectingSink>> = if req.verbose {
        Some(Arc::new(CollectingSink::default()))
    } else {
        None
    };

    let options = build_options(
        req.proxy,
        state.default_proxy.as_deref(),
        sink.clone().map(|s| s as Arc<dyn Sink>),
    );

    let parsed_url = match Url::parse(&req.url) {
        Ok(u) => u,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ChallengeResponse {
                    success: false,
                    clearance: None,
                    cookies: None,
                    cookie_header: None,
                    page_url: None,
                    html: None,
                    user_agent: None,
                    error: Some(format!("Invalid URL: {e}")),
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
                }),
            );
        }
    };

    match state.browser.clear(&parsed_url, &options).await {
        Ok(clearance) => {
            let (cookie_items, cookie_header) = cookie_items_and_header(&clearance.cookies);
            (
                StatusCode::OK,
                Json(ChallengeResponse {
                    success: true,
                    clearance: Some(clearance.clearance),
                    cookies: Some(cookie_items),
                    cookie_header: Some(cookie_header),
                    page_url: Some(clearance.page.url.to_string()),
                    html: Some(clearance.page.html),
                    user_agent: Some(state.user_agent.clone()),
                    error: None,
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
                }),
            )
        }
        Err(err) => (
            StatusCode::OK,
            Json(ChallengeResponse {
                success: false,
                clearance: None,
                cookies: None,
                cookie_header: None,
                page_url: None,
                html: None,
                user_agent: Some(state.user_agent.clone()),
                error: Some(format!("{err:#}")),
                elapsed_ms: started.elapsed().as_millis() as u64,
                logs: sink.and_then(|s| s.logs.lock().ok().map(|l| l.clone())),
            }),
        ),
    }
}

async fn flaresolverr_handler(
    State(state): State<AppState>,
    Json(req): Json<FlareSolverrRequest>,
) -> Json<FlareSolverrResponse> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    if req.cmd != "request.get" && req.cmd != "request.post" {
        return Json(FlareSolverrResponse {
            status: "error",
            message: format!("Unsupported FlareSolverr command '{}'", req.cmd),
            start_timestamp: now,
            end_timestamp: now,
            version: "v3.3.21",
            solution: None,
        });
    }

    let Some(raw_url) = req.url else {
        return Json(FlareSolverrResponse {
            status: "error",
            message: "Missing 'url' parameter".to_string(),
            start_timestamp: now,
            end_timestamp: now,
            version: "v3.3.21",
            solution: None,
        });
    };

    let parsed_url = match Url::parse(&raw_url) {
        Ok(u) => u,
        Err(e) => {
            return Json(FlareSolverrResponse {
                status: "error",
                message: format!("Invalid URL: {e}"),
                start_timestamp: now,
                end_timestamp: now,
                version: "v3.3.21",
                solution: None,
            });
        }
    };

    let proxy = req.proxy.map(FlareSolverrProxy::into_url);
    let options = build_options(proxy, state.default_proxy.as_deref(), None);

    match state.browser.solve(&parsed_url, &options).await {
        Ok(Outcome::Cleared(clearance)) => {
            let (cookie_items, _) = cookie_items_and_header(&clearance.cookies);
            let end = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            Json(FlareSolverrResponse {
                status: "ok",
                message: "Challenge solved!".to_string(),
                start_timestamp: now,
                end_timestamp: end,
                version: "v3.3.21",
                solution: Some(FlareSolverrSolution {
                    url: clearance.page.url.to_string(),
                    status: clearance.page.status.as_u16(),
                    headers: serde_json::json!({}),
                    response: clearance.page.html,
                    cookies: cookie_items,
                    user_agent: state.user_agent.clone(),
                }),
            })
        }
        Ok(Outcome::Token(token)) => {
            let end = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            Json(FlareSolverrResponse {
                status: "ok",
                message: format!("Turnstile solved: {token}"),
                start_timestamp: now,
                end_timestamp: end,
                version: "v3.3.21",
                solution: Some(FlareSolverrSolution {
                    url: raw_url,
                    status: 200,
                    headers: serde_json::json!({ "cf-turnstile-response": token }),
                    response: format!(r#"{{"token":"{token}"}}"#),
                    cookies: Vec::new(),
                    user_agent: state.user_agent.clone(),
                }),
            })
        }
        Err(err) => {
            let end = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            Json(FlareSolverrResponse {
                status: "error",
                message: format!("{err:#}"),
                start_timestamp: now,
                end_timestamp: end,
                version: "v3.3.21",
                solution: None,
            })
        }
    }
}

// -----------------------------------------------------------------------------
// CLI & Server Entry Point
// -----------------------------------------------------------------------------

struct ServerConfig {
    host: String,
    port: u16,
    fingerprint: PathBuf,
    proxy: Option<String>,
}

fn parse_args() -> Result<ServerConfig, String> {
    let mut host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let mut port = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let mut fingerprint = std::env::var("FINGERPRINT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("profiles/safari-26-ios.json"));
    let mut proxy = std::env::var("PROXY").ok();

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--host" => {
                host = args
                    .next()
                    .ok_or_else(|| "--host requires an IP address".to_string())?;
            }
            "--port" | "-p" => {
                let p_str = args
                    .next()
                    .ok_or_else(|| "--port requires a port number".to_string())?;
                port = p_str
                    .parse()
                    .map_err(|e| format!("Invalid port '{p_str}': {e}"))?;
            }
            "--fingerprint" | "-f" => {
                let f_str = args
                    .next()
                    .ok_or_else(|| "--fingerprint requires a file path".to_string())?;
                fingerprint = PathBuf::from(f_str);
            }
            "--proxy" => {
                proxy = Some(
                    args.next()
                        .ok_or_else(|| "--proxy requires a URL".to_string())?,
                );
            }
            "-h" | "--help" => {
                println!(
                    r#"cfsolve-server: Fast Cloudflare Turnstile & Challenge HTTP API

Usage:
  cfsolve-server [OPTIONS]

Options:
  --host <IP>             Host interface to bind to [default: 0.0.0.0 or $HOST]
  -p, --port <PORT>       Port to listen on [default: 8080 or $PORT]
  -f, --fingerprint <FILE> Path to browser profile JSON [default: profiles/safari-26-ios.json]
  --proxy <URL>           Default proxy for requests (http, https, socks5, socks5h)
  -h, --help              Print this help message
"#
                );
                std::process::exit(0);
            }
            other => {
                return Err(format!("Unknown option '{other}'. Run with --help for usage."));
            }
        }
    }

    Ok(ServerConfig {
        host,
        port,
        fingerprint,
        proxy,
    })
}

fn extract_user_agent_from_profile(profile_path: &Path) -> String {
    let default_ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.6.3 Mobile/15E148 Safari/604.1";
    let Ok(content) = std::fs::read_to_string(profile_path) else {
        return default_ua.to_string();
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) else {
        return default_ua.to_string();
    };
    val.pointer("/transport/headers/User-Agent")
        .and_then(|v| v.as_str())
        .unwrap_or(default_ua)
        .to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    };

    // Resolve profile path
    let profile_path = if config.fingerprint.exists() {
        config.fingerprint
    } else {
        let fallback = PathBuf::from("profiles/safari-26-ios.json");
        if fallback.exists() {
            fallback
        } else {
            anyhow::bail!(
                "Browser profile not found at '{}' or '{}'",
                config.fingerprint.display(),
                fallback.display()
            );
        }
    };

    eprintln!("Loading browser profile: {}", profile_path.display());
    let browser = Browser::read(&profile_path)
        .map_err(|e| anyhow::anyhow!("Failed to read browser profile: {e}"))?;
    let user_agent = extract_user_agent_from_profile(&profile_path);

    let state = AppState {
        browser: Arc::new(browser),
        default_proxy: config.proxy.clone(),
        profile_path,
        user_agent,
        started_at: Instant::now(),
    };

    // Fast CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(health_handler))
        .route("/health", get(health_handler))
        .route("/api/health", get(health_handler))
        .route("/solve", post(solve_handler))
        .route("/api/solve", post(solve_handler))
        .route("/turnstile", post(turnstile_handler))
        .route("/api/turnstile", post(turnstile_handler))
        .route("/challenge", post(challenge_handler))
        .route("/api/challenge", post(challenge_handler))
        .route("/v1", post(flaresolverr_handler))
        .layer(cors)
        .with_state(state);

    let bind_addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;

    eprintln!("cfsolve API listening on http://{}:{}", config.host, config.port);

    axum::serve(listener, app).await?;
    Ok(())
}
