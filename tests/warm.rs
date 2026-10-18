//! Drives `warm()` with a fake HTTP client that serves sitemaps and records page requests.

use std::collections::HashMap;
use std::sync::Mutex;

use molisnap::Format;
use molisnap::warm::{Fetch, FetchResponse, WarmOptions, WarmRequest, warm};

#[derive(Default)]
struct FakeWeb {
    pages: HashMap<String, (u16, String)>,
    requests: Mutex<Vec<WarmRequest>>,
}

impl FakeWeb {
    fn serve(mut self, url: &str, status: u16, body: &str) -> Self {
        self.pages.insert(url.to_string(), (status, body.to_string()));
        self
    }

    fn requested_urls(&self) -> Vec<String> {
        let mut urls: Vec<String> = self.requests.lock().unwrap().iter().map(|r| r.url.clone()).collect();
        urls.sort();
        urls
    }
}

impl Fetch for FakeWeb {
    async fn get(&self, req: WarmRequest) -> Result<FetchResponse, String> {
        let (status, body) = self.pages.get(&req.url).cloned().unwrap_or((200, "rendered".into()));
        self.requests.lock().unwrap().push(req);
        Ok(FetchResponse { status, body })
    }
}

fn via_molisnap() -> WarmOptions {
    WarmOptions { via: Some("http://molisnap:3000".into()), format: Format::Html, concurrency: 4 }
}

const SITEMAP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://example.com/</loc></url>
  <url><loc>https://example.com/about</loc><lastmod>2026-10-01</lastmod></url>
</urlset>"#;

#[tokio::test]
async fn warms_every_page_in_the_sitemap_through_molisnap() {
    let web = FakeWeb::default().serve("https://example.com/sitemap.xml", 200, SITEMAP);

    let report = warm(&web, "https://example.com/sitemap.xml", &via_molisnap()).await.unwrap();

    assert_eq!(report.warmed, 2);
    assert_eq!(
        web.requested_urls(),
        [
            "http://molisnap:3000/https://example.com/",
            "http://molisnap:3000/https://example.com/about",
            "https://example.com/sitemap.xml",
        ]
    );
}

#[tokio::test]
async fn without_via_pages_are_requested_directly_as_a_bot_so_nginx_renders_and_caches_them() {
    let web = FakeWeb::default().serve("https://example.com/sitemap.xml", 200, SITEMAP);
    let opts = WarmOptions { via: None, ..via_molisnap() };

    warm(&web, "https://example.com/sitemap.xml", &opts).await.unwrap();

    let requests = web.requests.lock().unwrap();
    let page = requests.iter().find(|r| r.url == "https://example.com/about").expect("page requested directly");
    assert_eq!(page.header("user-agent"), Some("molisnap-warm/0.1"));
}

#[tokio::test]
async fn markdown_warming_asks_for_markdown_both_ways_nginx_and_molisnap_understand() {
    let web = FakeWeb::default().serve("https://example.com/sitemap.xml", 200, SITEMAP);
    let opts = WarmOptions { format: Format::Markdown, ..via_molisnap() };

    warm(&web, "https://example.com/sitemap.xml", &opts).await.unwrap();

    let requests = web.requests.lock().unwrap();
    let page = requests.iter().find(|r| r.url.ends_with("/about")).unwrap();
    assert_eq!(page.header("accept"), Some("text/markdown"));
    assert_eq!(page.header("x-render-format"), Some("markdown"));
}

#[tokio::test]
async fn follows_a_sitemap_index_to_its_child_sitemaps() {
    let index = r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <sitemap><loc>https://example.com/sitemap-blog.xml</loc></sitemap>
  <sitemap><loc>https://example.com/sitemap-shop.xml</loc></sitemap>
</sitemapindex>"#;
    let web = FakeWeb::default()
        .serve("https://example.com/sitemap.xml", 200, index)
        .serve("https://example.com/sitemap-blog.xml", 200, "<urlset><url><loc>https://example.com/blog/1</loc></url></urlset>")
        .serve("https://example.com/sitemap-shop.xml", 200, "<urlset><url><loc>https://example.com/shop/1</loc></url></urlset>");

    let report = warm(&web, "https://example.com/sitemap.xml", &via_molisnap()).await.unwrap();

    assert_eq!(report.warmed, 2);
    let urls = web.requested_urls();
    assert!(urls.contains(&"http://molisnap:3000/https://example.com/blog/1".to_string()));
    assert!(urls.contains(&"http://molisnap:3000/https://example.com/shop/1".to_string()));
}

#[tokio::test]
async fn pages_that_fail_to_render_are_reported_not_counted_as_warmed() {
    let web = FakeWeb::default()
        .serve("https://example.com/sitemap.xml", 200, SITEMAP)
        .serve("http://molisnap:3000/https://example.com/about", 502, "moli failed");

    let report = warm(&web, "https://example.com/sitemap.xml", &via_molisnap()).await.unwrap();

    assert_eq!(report.warmed, 1);
    assert_eq!(report.failed, [("http://molisnap:3000/https://example.com/about".to_string(), "HTTP 502".to_string())]);
}

#[tokio::test]
async fn a_missing_sitemap_is_an_error_not_an_empty_warm() {
    let web = FakeWeb::default().serve("https://example.com/sitemap.xml", 404, "not found");

    let err = warm(&web, "https://example.com/sitemap.xml", &via_molisnap()).await.unwrap_err();

    assert_eq!(err, "sitemap https://example.com/sitemap.xml returned HTTP 404");
}

#[tokio::test]
async fn xml_escaped_urls_are_unescaped_before_requesting() {
    let sitemap = "<urlset><url><loc>https://example.com/search?q=a&amp;page=2</loc></url></urlset>";
    let web = FakeWeb::default().serve("https://example.com/sitemap.xml", 200, sitemap);
    let opts = WarmOptions { via: None, ..via_molisnap() };

    warm(&web, "https://example.com/sitemap.xml", &opts).await.unwrap();

    assert!(web.requested_urls().contains(&"https://example.com/search?q=a&page=2".to_string()));
}
