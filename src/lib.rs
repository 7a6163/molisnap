pub mod cache;
pub mod render;
pub mod warm;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use tracing::{info, warn};
use url::Url;

pub use cache::Cache;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Html,
    Markdown,
    Semantic,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "html" => Some(Self::Html),
            "markdown" | "md" => Some(Self::Markdown),
            "semantic" => Some(Self::Semantic),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "markdown",
            Self::Semantic => "semantic",
        }
    }

    fn moli_dump(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "markdown",
            Self::Semantic => "semantic_tree_text",
        }
    }

    pub(crate) fn mime(self) -> &'static str {
        match self {
            Self::Html => "text/html",
            Self::Markdown => "text/markdown",
            Self::Semantic => "text/plain",
        }
    }

    fn content_type(self) -> String {
        format!("{}; charset=utf-8", self.mime())
    }

    /// Explicit name wins; otherwise `Accept: text/markdown` gets Markdown, everything else HTML.
    fn select(explicit: Option<&str>, headers: &HeaderMap) -> Result<Self, String> {
        if let Some(name) = explicit {
            return Self::parse(name).ok_or_else(|| format!("unknown format `{name}`"));
        }
        let accept = headers
            .get(header::ACCEPT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        Ok(if accept.contains("text/markdown") {
            Self::Markdown
        } else {
            Self::Html
        })
    }
}

#[derive(Clone, Debug)]
pub struct Page {
    pub status: u16,
    pub body: Bytes,
    pub rendered_at: u64,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct Config {
    pub listen_addr: String,
    /// Unset: no in-process cache; downstream caches (nginx, CDN) follow `Cache-Control`.
    pub valkey_url: Option<String>,
    pub moli_bin: String,
    /// `example.com` matches exactly; `*.example.com` matches any subdomain.
    pub allowed_hosts: Vec<String>,
    pub fresh_secs: u64,
    pub stale_secs: u64,
    pub render_timeout_ms: u64,
    pub max_concurrent_renders: usize,
    /// Lowercase query parameter names removed before a URL is rendered or cached.
    pub strip_query_params: Vec<String>,
}

/// Tracking parameters: each distinct value would otherwise be its own render and
/// cache entry for the same page.
pub const DEFAULT_STRIPPED_PARAMS: &[&str] = &[
    // Google / Analytics
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "utm_source_platform",
    "utm_creative_format",
    "utm_marketing_tactic",
    "_ga",
    "_gl",
    "gclid",
    "gclsrc",
    "dclid",
    "gbraid",
    "wbraid",
    // Meta / LINE / TikTok / X / LinkedIn / Microsoft / Yandex
    "fbclid",
    "fb_action_ids",
    "fb_action_types",
    "fb_source",
    "igshid",
    "igsh",
    "ttclid",
    "twclid",
    "li_fat_id",
    "msclkid",
    "yclid",
    "openexternalbrowser",
    // Mail / affiliate
    "mc_cid",
    "mc_eid",
    "mkt_tok",
    "vero_id",
    "vero_conv",
    "_hsenc",
    "_hsmi",
    "hsa_cam",
    "hsa_grp",
    "hsa_ad",
    "ref_src",
    "ref_url",
];

/// `STRIP_QUERY_PARAMS`: unset uses the defaults, empty strips nothing.
pub fn strip_query_params(raw: Option<&str>) -> Vec<String> {
    match raw {
        None => DEFAULT_STRIPPED_PARAMS
            .iter()
            .map(|p| p.to_string())
            .collect(),
        Some(list) => list
            .split(',')
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty())
            .collect(),
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        fn var(name: &str, default: &str) -> String {
            std::env::var(name).unwrap_or_else(|_| default.to_string())
        }
        fn num<T: std::str::FromStr>(name: &str, default: &str) -> Result<T, String> {
            var(name, default)
                .parse()
                .map_err(|_| format!("{name} must be a number"))
        }
        let allowed_hosts: Vec<String> = var("ALLOWED_HOSTS", "")
            .split(',')
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if allowed_hosts.is_empty() {
            return Err(
                "ALLOWED_HOSTS is required (comma-separated, e.g. `example.com,*.example.com`)"
                    .into(),
            );
        }
        Ok(Self {
            listen_addr: var("LISTEN_ADDR", "0.0.0.0:3000"),
            valkey_url: std::env::var("VALKEY_URL").ok().filter(|v| !v.is_empty()),
            moli_bin: var("MOLI_BIN", "moli"),
            allowed_hosts,
            fresh_secs: num("CACHE_FRESH_SECS", "3600")?,
            stale_secs: num("CACHE_STALE_SECS", "86400")?,
            render_timeout_ms: num("RENDER_TIMEOUT_MS", "20000")?,
            max_concurrent_renders: num("MAX_CONCURRENT_RENDERS", "8")?,
            strip_query_params: strip_query_params(
                std::env::var("STRIP_QUERY_PARAMS").ok().as_deref(),
            ),
        })
    }

    fn host_allowed(&self, host: &str) -> bool {
        self.allowed_hosts
            .iter()
            .any(|a| match a.strip_prefix("*.") {
                Some(base) => {
                    host.len() > base.len()
                        && host.ends_with(base)
                        && host[..host.len() - base.len()].ends_with('.')
                }
                None => host == a,
            })
    }
}

type RenderFuture = Shared<BoxFuture<'static, Result<Page, String>>>;

/// Turns a validated URL into a page. Production uses [`render::moli`]; tests inject fakes.
pub type Renderer =
    Arc<dyn Fn(String, Format) -> BoxFuture<'static, Result<Page, String>> + Send + Sync>;

pub struct AppState {
    cfg: Config,
    cache: Option<Cache>,
    renderer: Renderer,
    renders: Semaphore,
    /// Single-flight: concurrent requests for the same cache key share one render.
    inflight: Mutex<HashMap<String, RenderFuture>>,
}

impl AppState {
    pub fn new(cfg: Config, cache: Option<Cache>, renderer: Renderer) -> Arc<Self> {
        Arc::new(Self {
            renders: Semaphore::new(cfg.max_concurrent_renders),
            cfg,
            cache,
            renderer,
            inflight: Mutex::new(HashMap::new()),
        })
    }
}

type St = Arc<AppState>;

pub fn app(state: St) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/render", get(render_query))
        // Prerender-style: GET /https://example.com/path?x=1 (what an nginx bot rule proxies to).
        .fallback(get(render_path))
        .with_state(state)
}

#[derive(Deserialize)]
struct RenderParams {
    url: String,
    format: Option<String>,
}

async fn render_query(
    State(st): State<St>,
    headers: HeaderMap,
    Query(p): Query<RenderParams>,
) -> Response {
    match Format::select(p.format.as_deref(), &headers) {
        Ok(format) => serve(st, &p.url, format).await,
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

/// The target's own query string belongs to the target, so the format comes from
/// the `X-Render-Format` or `Accept` header.
async fn render_path(State(st): State<St>, headers: HeaderMap, uri: Uri) -> Response {
    let target = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("")
        .trim_start_matches('/');
    if !(target.starts_with("http://") || target.starts_with("https://")) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let explicit = headers.get("x-render-format").and_then(|v| v.to_str().ok());
    match Format::select(explicit, &headers) {
        Ok(format) => serve(st, target, format).await,
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn serve(st: St, raw_url: &str, format: Format) -> Response {
    let url = match validate_url(&st.cfg, raw_url) {
        Ok(u) => u,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    let key = cache_key(&url, format);

    if let Some(page) = cached(&st, &key).await {
        if now_secs().saturating_sub(page.rendered_at) < st.cfg.fresh_secs {
            return respond(&st.cfg, page, format, "HIT");
        }
        // Stale-while-revalidate: answer now, refresh in the background (once across replicas).
        let lock_secs = st.cfg.render_timeout_ms / 1000 + 10;
        let (st2, key2) = (st.clone(), key.clone());
        tokio::spawn(async move {
            if let Some(cache) = &st2.cache
                && cache.try_lock(&key2, lock_secs).await
            {
                let _ = render_once(&st2, key2, url, format).await;
            }
        });
        return respond(&st.cfg, page, format, "STALE");
    }

    match render_once(&st, key, url, format).await {
        Ok(page) => respond(&st.cfg, page, format, "MISS"),
        Err(e) => {
            warn!(error = %e, "render failed");
            (StatusCode::BAD_GATEWAY, e).into_response()
        }
    }
}

async fn cached(st: &AppState, key: &str) -> Option<Page> {
    st.cache.as_ref()?.get(key).await
}

fn validate_url(cfg: &Config, raw: &str) -> Result<Url, String> {
    let mut url = Url::parse(raw).map_err(|e| format!("invalid url: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http(s) urls are supported".into());
    }
    let host = url.host_str().ok_or("url has no host")?;
    if !cfg.host_allowed(host) {
        return Err(format!("host `{host}` is not in ALLOWED_HOSTS"));
    }
    url.set_fragment(None);
    strip_params(&mut url, &cfg.strip_query_params);
    Ok(url)
}

/// Works on the raw query so the parameters that stay keep their order and encoding.
fn strip_params(url: &mut Url, names: &[String]) {
    let Some(query) = url.query() else { return };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or("");
            !names.iter().any(|n| n.eq_ignore_ascii_case(name))
        })
        .collect();
    if kept.len() == query.split('&').count() {
        return;
    }
    let kept = kept.join("&");
    url.set_query((!kept.is_empty()).then_some(kept.as_str()));
}

fn cache_key(url: &Url, format: Format) -> String {
    let digest = Sha256::digest(url.as_str().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("molisnap:v1:{}:{hex}", format.as_str())
}

/// Starts (or joins) the render for `key`. The work runs in its own task, so it
/// finishes and fills the cache even if every waiting client disconnects.
fn render_once(st: &St, key: String, url: Url, format: Format) -> RenderFuture {
    let mut inflight = st.inflight.lock().unwrap();
    if let Some(fut) = inflight.get(&key) {
        return fut.clone();
    }
    let st2 = st.clone();
    let key2 = key.clone();
    let task = tokio::spawn(async move {
        let res = render_and_store(&st2, &key2, url.as_str(), format).await;
        st2.inflight.lock().unwrap().remove(&key2);
        res
    });
    let fut = async move { task.await.map_err(|e| format!("render task failed: {e}"))? }
        .boxed()
        .shared();
    inflight.insert(key, fut.clone());
    fut
}

async fn render_and_store(
    st: &AppState,
    key: &str,
    url: &str,
    format: Format,
) -> Result<Page, String> {
    let wait = Duration::from_millis(st.cfg.render_timeout_ms);
    let _permit = tokio::time::timeout(wait, st.renders.acquire())
        .await
        .map_err(|_| "render queue is full".to_string())?
        .map_err(|e| e.to_string())?;

    let started = std::time::Instant::now();
    let page = (st.renderer)(url.to_string(), format).await?;
    info!(%url, format = format.as_str(), status = page.status, ms = started.elapsed().as_millis() as u64, "rendered");

    // 5xx and other errors are not cached so a flaky origin heals on the next request.
    if let Some(cache) = &st.cache
        && cacheable(page.status)
    {
        cache
            .set(key, &page, st.cfg.fresh_secs + st.cfg.stale_secs)
            .await;
    }
    Ok(page)
}

fn cacheable(status: u16) -> bool {
    matches!(status, 200..=299 | 404 | 410)
}

fn respond(cfg: &Config, page: Page, format: Format, cache: &'static str) -> Response {
    let status = StatusCode::from_u16(page.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let age = now_secs().saturating_sub(page.rendered_at).to_string();
    let cache_control = if cacheable(page.status) {
        format!(
            "public, max-age={fresh}, stale-while-revalidate={stale}, stale-if-error={stale}",
            fresh = cfg.fresh_secs,
            stale = cfg.stale_secs
        )
    } else {
        "no-store".to_string()
    };
    (
        status,
        [
            (header::CONTENT_TYPE, format.content_type()),
            (
                header::HeaderName::from_static("x-cache"),
                cache.to_string(),
            ),
            (header::AGE, age),
            (header::CACHE_CONTROL, cache_control),
        ],
        page.body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(hosts: &[&str]) -> Config {
        Config {
            listen_addr: String::new(),
            valkey_url: None,
            moli_bin: String::new(),
            allowed_hosts: hosts.iter().map(|s| s.to_string()).collect(),
            fresh_secs: 0,
            stale_secs: 0,
            render_timeout_ms: 0,
            max_concurrent_renders: 1,
            strip_query_params: strip_query_params(None),
        }
    }

    #[test]
    fn strip_list_defaults_when_unset_and_is_empty_when_blank() {
        assert!(strip_query_params(None).contains(&"fbclid".to_string()));
        assert!(strip_query_params(Some("")).is_empty());
        assert_eq!(strip_query_params(Some(" Ref , ,sid")), ["ref", "sid"]);
    }

    #[test]
    fn host_allowlist() {
        let c = cfg(&["example.com", "*.shop.io"]);
        assert!(c.host_allowed("example.com"));
        assert!(!c.host_allowed("www.example.com"));
        assert!(c.host_allowed("a.shop.io"));
        assert!(!c.host_allowed("shop.io"));
        assert!(!c.host_allowed("evilshop.io"));
    }

    #[test]
    fn validate_rejects_unknown_hosts_and_drops_fragment() {
        let c = cfg(&["example.com"]);
        assert!(validate_url(&c, "http://169.254.169.254/").is_err());
        assert!(validate_url(&c, "file:///etc/passwd").is_err());
        assert_eq!(
            validate_url(&c, "https://example.com/a?b=1#x")
                .unwrap()
                .as_str(),
            "https://example.com/a?b=1"
        );
    }

    #[test]
    fn cache_key_varies_by_format() {
        let u = Url::parse("https://example.com/").unwrap();
        assert_ne!(cache_key(&u, Format::Html), cache_key(&u, Format::Markdown));
    }
}
