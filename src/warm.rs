//! `molisnap warm`: request every page in a sitemap so the cache in front of it fills up.

use std::future::Future;

use futures::StreamExt;

use crate::Format;

/// Matched by the nginx bot rule, so direct requests get routed to molisnap.
pub const USER_AGENT: &str = "molisnap-warm/0.1";

pub struct WarmRequest {
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
}

impl WarmRequest {
    fn new(url: String) -> Self {
        Self {
            url,
            headers: vec![("user-agent", USER_AGENT.to_string())],
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

pub struct FetchResponse {
    pub status: u16,
    pub body: String,
}

/// The HTTP client boundary; production uses reqwest, tests a fake.
pub trait Fetch {
    fn get(&self, req: WarmRequest) -> impl Future<Output = Result<FetchResponse, String>> + Send;
}

pub struct WarmOptions {
    /// molisnap base URL; pages are requested as `{via}/{page url}`. Unset: pages are
    /// requested directly, so whatever sits in front of the site (nginx, CDN) caches them.
    pub via: Option<String>,
    pub format: Format,
    pub concurrency: usize,
}

#[derive(Debug, Default)]
pub struct WarmReport {
    pub warmed: usize,
    /// `(requested url, reason)` for each page that didn't come back 2xx.
    pub failed: Vec<(String, String)>,
}

pub async fn warm<F: Fetch + Sync>(
    fetch: &F,
    sitemap_url: &str,
    opts: &WarmOptions,
) -> Result<WarmReport, String> {
    let pages = page_urls(fetch, sitemap_url).await?;

    let results: Vec<_> = futures::stream::iter(pages)
        .map(|page| {
            let url = match &opts.via {
                Some(via) => format!("{}/{page}", via.trim_end_matches('/')),
                None => page,
            };
            let mut req = WarmRequest::new(url);
            // nginx picks the format from Accept; molisnap's path mode reads X-Render-Format.
            req.headers.push(("accept", opts.format.mime().to_string()));
            req.headers
                .push(("x-render-format", opts.format.as_str().to_string()));
            async move {
                let url = req.url.clone();
                match fetch.get(req).await {
                    Ok(res) if (200..300).contains(&res.status) => Ok(()),
                    Ok(res) => Err((url, format!("HTTP {}", res.status))),
                    Err(e) => Err((url, e)),
                }
            }
        })
        .buffer_unordered(opts.concurrency.max(1))
        .collect()
        .await;

    let mut report = WarmReport::default();
    for result in results {
        match result {
            Ok(()) => report.warmed += 1,
            Err(failure) => report.failed.push(failure),
        }
    }
    Ok(report)
}

/// Page URLs from a sitemap, or from every child of a sitemap index. The protocol
/// doesn't allow nested indexes, so this only goes one level down.
async fn page_urls<F: Fetch + Sync>(fetch: &F, sitemap_url: &str) -> Result<Vec<String>, String> {
    let sitemap = fetch_sitemap(fetch, sitemap_url).await?;
    if !sitemap.contains("<sitemapindex") {
        return Ok(locs(&sitemap));
    }
    let mut pages = Vec::new();
    for child in locs(&sitemap) {
        pages.extend(locs(&fetch_sitemap(fetch, &child).await?));
    }
    Ok(pages)
}

async fn fetch_sitemap<F: Fetch + Sync>(fetch: &F, url: &str) -> Result<String, String> {
    let res = fetch.get(WarmRequest::new(url.to_string())).await?;
    if !(200..300).contains(&res.status) {
        return Err(format!("sitemap {url} returned HTTP {}", res.status));
    }
    Ok(res.body)
}

/// Every `<loc>` value in a sitemap document.
fn locs(xml: &str) -> Vec<String> {
    xml.split("<loc>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</loc>"))
        .map(|(loc, _)| unescape_xml(loc.trim()))
        .collect()
}

/// The five predefined XML entities; `&amp;` last so `&amp;lt;` stays `&lt;`.
fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The production [`Fetch`].
pub struct HttpFetch(reqwest::Client);

impl HttpFetch {
    /// `timeout` should cover a cold render behind the target, not just the network.
    pub fn new(timeout: std::time::Duration) -> Result<Self, String> {
        reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map(Self)
            .map_err(|e| e.to_string())
    }
}

impl Fetch for HttpFetch {
    async fn get(&self, req: WarmRequest) -> Result<FetchResponse, String> {
        let mut builder = self.0.get(&req.url);
        for (name, value) in &req.headers {
            builder = builder.header(*name, value);
        }
        let res = builder.send().await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let body = res.text().await.map_err(|e| e.to_string())?;
        Ok(FetchResponse { status, body })
    }
}
