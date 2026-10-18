//! Drives the HTTP router with a fake renderer in place of Moli.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response};
use futures::FutureExt;
use molisnap::{AppState, Cache, Config, Page, Renderer, app, now_secs};
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

/// Valkey-backed tests need a real server: `VALKEY_TEST_URL=redis://127.0.0.1:6390 cargo test`.
async fn valkey() -> Option<Cache> {
    let Ok(url) = std::env::var("VALKEY_TEST_URL") else {
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("/render?url=https://example.com/t/{nanos}")
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
