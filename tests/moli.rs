//! Contract tests against a real Moli binary and the network:
//! `MOLI_BIN=/path/to/moli cargo test --test moli`. They catch flag or exit-code
//! changes when Moli is upgraded.

use molisnap::render::Moli;
use molisnap::{Format, render};

/// Skipped locally when unset, but CI must never skip these silently.
fn moli_bin() -> Option<String> {
    let bin = std::env::var("MOLI_BIN").ok();
    if bin.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "MOLI_BIN must be set in CI"
        );
        eprintln!("skipped: MOLI_BIN not set");
    }
    bin
}

fn moli(bin: String) -> Moli {
    Moli {
        bin,
        timeout_ms: 20_000,
        wait_script: None,
    }
}

#[tokio::test]
async fn renders_a_page_as_html_and_markdown() {
    let Some(bin) = moli_bin() else { return };

    let html = render::render(&moli(bin.clone()), "https://example.com/", Format::Html)
        .await
        .unwrap();
    let md = render::render(&moli(bin.clone()), "https://example.com/", Format::Markdown)
        .await
        .unwrap();

    assert_eq!(html.status, 200);
    assert!(String::from_utf8_lossy(&html.body).contains("<title>Example Domain</title>"));
    assert!(
        String::from_utf8_lossy(&md.body)
            .contains("This domain is for use in documentation examples")
    );
}

#[tokio::test]
async fn reports_the_origin_status_for_a_missing_page() {
    let Some(bin) = moli_bin() else { return };

    let page = render::render(
        &moli(bin),
        "https://example.com/does-not-exist",
        Format::Html,
    )
    .await
    .unwrap();

    assert_eq!(page.status, 404);
}

#[tokio::test]
async fn a_spa_can_set_its_status_with_a_prerender_meta_tag() {
    let Some(bin) = moli_bin() else { return };
    // Like a client-side router's not-found view: the meta tag only exists after JS runs.
    let spa_404 = r#"data:text/html,<script>document.head.insertAdjacentHTML("beforeend","<meta name=prerender-status-code content=404>")</script><p>gone</p>"#;

    let html = render::render(&moli(bin.clone()), spa_404, Format::Html)
        .await
        .unwrap();
    let md = render::render(&moli(bin.clone()), spa_404, Format::Markdown)
        .await
        .unwrap();

    assert_eq!(html.status, 404);
    assert_eq!(md.status, 404);
}

/// Like an SPA loading its content from an API: nothing useful is in the DOM until the
/// request settles. (A data: URL because `--block-private-networks` rules out a local server.)
const FETCHES_ITS_CONTENT: &str = r#"data:text/html,<div id=app>LOADING</div><script>fetch("https://example.com/").catch(()=>{}).finally(()=>{app.textContent="LOADED"})</script>"#;

/// A page whose content arrives with no request in flight, so only its own signal tells
/// when it is done.
const SIGNALS_WHEN_READY: &str = r#"data:text/html,<div id=app>LOADING</div><script>setTimeout(()=>{app.textContent="LOADED";window.prerenderReady=true},1500)</script>"#;

#[tokio::test]
async fn an_spa_is_captured_after_its_api_requests_settle() {
    let Some(bin) = moli_bin() else { return };

    let page = render::render(&moli(bin), FETCHES_ITS_CONTENT, Format::Html)
        .await
        .unwrap();

    assert!(String::from_utf8_lossy(&page.body).contains("LOADED"));
}

#[tokio::test]
async fn a_wait_script_holds_the_capture_until_the_page_says_it_is_ready() {
    let Some(bin) = moli_bin() else { return };
    let opts = Moli {
        wait_script: Some("window.prerenderReady === true".into()),
        ..moli(bin)
    };

    let page = render::render(&opts, SIGNALS_WHEN_READY, Format::Markdown)
        .await
        .unwrap();

    assert!(String::from_utf8_lossy(&page.body).contains("LOADED"));
}

#[tokio::test]
async fn a_page_that_never_signals_ready_fails_instead_of_returning_its_shell() {
    let Some(bin) = moli_bin() else { return };
    let opts = Moli {
        timeout_ms: 2_000,
        wait_script: Some("window.prerenderReady === true".into()),
        ..moli(bin)
    };

    let res = render::render(&opts, FETCHES_ITS_CONTENT, Format::Html).await;

    assert!(res.is_err(), "got a page: {res:?}");
}
