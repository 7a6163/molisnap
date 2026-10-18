//! Drives the HTTP router with a fake renderer in place of Moli.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response};
use futures::FutureExt;
use molisnap::{AppState, Cache, Config, Page, Renderer, app, now_secs, strip_query_params};
use tower::ServiceExt;

fn config(fresh_secs: u64, stale_secs: u64) -> Config {
    Config {
        listen_addr: String::new(),
        valkey_url: None,
        moli_bin: String::new(),
        allowed_hosts: vec!["example.com".into()],
        fresh_secs,
        stale_secs,
        render_timeout_ms: 5_000,
        max_concurrent_renders: 4,
        strip_query_params: strip_query_params(None),
    }
}

/// Answers every render with `status` and counts how often it was called.
#[derive(Clone)]
struct FakeRenderer {
    status: u16,
    delay: Duration,
    calls: Arc<AtomicUsize>,
}

impl FakeRenderer {
    fn new(status: u16) -> Self {
        Self {
            status,
            delay: Duration::ZERO,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Slow renders let concurrent requests overlap.
    fn slow(status: u16) -> Self {
        Self {
            delay: Duration::from_millis(100),
            ..Self::new(status)
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn renderer(&self) -> Renderer {
        let me = self.clone();
        Arc::new(move |url, _format| {
            let me = me.clone();
            async move {
                me.calls.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(me.delay).await;
                Ok(Page {
                    status: me.status,
                    body: format!("rendered {url}").into(),
                    rendered_at: now_secs(),
                })
            }
            .boxed()
        })
    }
}

async fn body(res: Response<Body>) -> String {
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

async fn get(app: &Router, uri: &str) -> Response<Body> {
    app.clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn without_valkey_responses_tell_downstream_caches_how_long_to_keep_them() {
    let fake = FakeRenderer::new(200);
    let app = app(AppState::new(config(3600, 86400), None, fake.renderer()));

    let res = get(&app, "/render?url=https://example.com/a").await;

    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()["cache-control"],
        "public, max-age=3600, stale-while-revalidate=86400, stale-if-error=86400"
    );
}

#[tokio::test]
async fn origin_errors_are_not_cached_downstream() {
    let fake = FakeRenderer::new(503);
    let app = app(AppState::new(config(3600, 86400), None, fake.renderer()));

    let res = get(&app, "/render?url=https://example.com/a").await;

    assert_eq!(res.status(), 503);
    assert_eq!(res.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn concurrent_requests_for_one_page_share_a_single_render() {
    let fake = FakeRenderer::slow(200);
    let app = app(AppState::new(config(3600, 86400), None, fake.renderer()));

    let responses =
        futures::future::join_all((0..5).map(|_| get(&app, "/render?url=https://example.com/a")))
            .await;

    assert!(responses.iter().all(|r| r.status() == 200));
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn tracking_parameters_are_stripped_before_rendering() {
    let fake = FakeRenderer::new(200);
    let app = app(AppState::new(config(3600, 86400), None, fake.renderer()));

    let res = get(&app, "/https://example.com/a?id=7&fbclid=abc&UTM_Source=fb").await;

    assert_eq!(body(res).await, "rendered https://example.com/a?id=7");
}

#[tokio::test]
async fn a_url_with_only_tracking_parameters_loses_its_question_mark() {
    let fake = FakeRenderer::new(200);
    let app = app(AppState::new(config(3600, 86400), None, fake.renderer()));

    let res = get(&app, "/https://example.com/a?utm_source=fb&gclid=1").await;

    assert_eq!(body(res).await, "rendered https://example.com/a");
}

#[tokio::test]
async fn untracked_parameters_keep_their_order_and_encoding() {
    let fake = FakeRenderer::new(200);
    let app = app(AppState::new(config(3600, 86400), None, fake.renderer()));

    let res = get(
        &app,
        "/https://example.com/s?q=a%20b&fbclid=x&sort=new&q=c+d",
    )
    .await;

    assert_eq!(
        body(res).await,
        "rendered https://example.com/s?q=a%20b&sort=new&q=c+d"
    );
}

#[tokio::test]
async fn an_empty_strip_list_leaves_the_query_alone() {
    let fake = FakeRenderer::new(200);
    let cfg = Config {
        strip_query_params: strip_query_params(Some("")),
        ..config(3600, 86400)
    };
    let app = app(AppState::new(cfg, None, fake.renderer()));

    let res = get(&app, "/https://example.com/a?fbclid=abc").await;

    assert_eq!(body(res).await, "rendered https://example.com/a?fbclid=abc");
}

/// Valkey-backed tests need a real server: `VALKEY_TEST_URL=redis://127.0.0.1:6390 cargo test`.
/// Skipped locally when unset, but CI must never skip them silently.
async fn valkey() -> Option<Cache> {
    let Ok(url) = std::env::var("VALKEY_TEST_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "VALKEY_TEST_URL must be set in CI"
        );
        eprintln!("skipped: VALKEY_TEST_URL not set");
        return None;
    };
    Some(
        Cache::connect(&url)
            .await
            .expect("connect to VALKEY_TEST_URL"),
    )
}

/// A path no earlier run has cached, so tests don't see each other's entries.
fn fresh_page_uri() -> String {
    format!("/render?url=https://example.com/t/{}", unique())
}

fn unique() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[tokio::test]
async fn with_valkey_a_repeat_request_is_served_from_cache() {
    let Some(cache) = valkey().await else { return };
    let fake = FakeRenderer::new(200);
    let app = app(AppState::new(
        config(3600, 86400),
        Some(cache),
        fake.renderer(),
    ));
    let uri = fresh_page_uri();

    let first = get(&app, &uri).await;
    let second = get(&app, &uri).await;

    assert_eq!(first.headers()["x-cache"], "MISS");
    assert_eq!(second.headers()["x-cache"], "HIT");
    assert_eq!(fake.calls(), 1);
}

#[tokio::test]
async fn with_valkey_an_expired_page_is_served_stale_and_refreshed_in_the_background() {
    let Some(cache) = valkey().await else { return };
    let fake = FakeRenderer::new(200);
    // fresh_secs = 0: every cached copy is already past its fresh window.
    let app = app(AppState::new(
        config(0, 86400),
        Some(cache),
        fake.renderer(),
    ));
    let uri = fresh_page_uri();

    get(&app, &uri).await;
    let stale = get(&app, &uri).await;

    assert_eq!(stale.headers()["x-cache"], "STALE");
    let refreshed = async {
        while fake.calls() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(2), refreshed)
        .await
        .expect("background refresh never rendered");
}

#[tokio::test]
async fn with_valkey_links_differing_only_in_click_ids_share_one_cache_entry() {
    let Some(cache) = valkey().await else { return };
    let fake = FakeRenderer::new(200);
    let app = app(AppState::new(
        config(3600, 86400),
        Some(cache),
        fake.renderer(),
    ));
    let page = format!("/https://example.com/t/{}", unique());

    get(&app, &format!("{page}?fbclid=first")).await;
    let second = get(&app, &format!("{page}?fbclid=second")).await;

    assert_eq!(second.headers()["x-cache"], "HIT");
    assert_eq!(fake.calls(), 1);
}
