//! Contract tests against a real Moli binary and the network:
//! `MOLI_BIN=/path/to/moli cargo test --test moli`. They catch flag or exit-code
//! changes when Moli is upgraded.

use molisnap::{Format, render};

fn moli_bin() -> Option<String> {
    let bin = std::env::var("MOLI_BIN").ok();
    if bin.is_none() {
        eprintln!("skipped: MOLI_BIN not set");
    }
    bin
}

#[tokio::test]
async fn renders_a_page_as_html_and_markdown() {
    let Some(bin) = moli_bin() else { return };

    let html = render::render(&bin, 20_000, "https://example.com/", Format::Html).await.unwrap();
    let md = render::render(&bin, 20_000, "https://example.com/", Format::Markdown).await.unwrap();

    assert_eq!(html.status, 200);
    assert!(String::from_utf8_lossy(&html.body).contains("<title>Example Domain</title>"));
    assert!(String::from_utf8_lossy(&md.body).contains("This domain is for use in documentation examples"));
}

#[tokio::test]
async fn reports_the_origin_status_for_a_missing_page() {
    let Some(bin) = moli_bin() else { return };

    let page = render::render(&bin, 20_000, "https://example.com/does-not-exist", Format::Html).await.unwrap();

    assert_eq!(page.status, 404);
}

#[tokio::test]
async fn a_spa_can_set_its_status_with_a_prerender_meta_tag() {
    let Some(bin) = moli_bin() else { return };
    // Like a client-side router's not-found view: the meta tag only exists after JS runs.
    let spa_404 = r#"data:text/html,<script>document.head.insertAdjacentHTML("beforeend","<meta name=prerender-status-code content=404>")</script><p>gone</p>"#;

    let html = render::render(&bin, 20_000, spa_404, Format::Html).await.unwrap();
    let md = render::render(&bin, 20_000, spa_404, Format::Markdown).await.unwrap();

    assert_eq!(html.status, 404);
    assert_eq!(md.status, 404);
}
