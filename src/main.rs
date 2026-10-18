use std::time::Duration;

use molisnap::warm::{HttpFetch, WarmOptions, warm};
use molisnap::{AppState, Cache, Config, Format, app, render};
use tracing::info;

const WARM_USAGE: &str = "usage: molisnap warm <sitemap-url> [--via <molisnap base url>] [--format html|markdown|semantic] [--concurrency N]";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("warm") {
        std::process::exit(warm_cli(&args[1..]).await);
    }

    let cfg = Config::from_env().unwrap_or_else(|e| {
        eprintln!("config error: {e}");
        std::process::exit(2);
    });
    let cache = match &cfg.valkey_url {
        Some(url) => Some(Cache::connect(url).await.unwrap_or_else(|e| {
            eprintln!("cannot connect to valkey at {url}: {e}");
            std::process::exit(1);
        })),
        None => None,
    };
    let listener = tokio::net::TcpListener::bind(&cfg.listen_addr).await.expect("bind LISTEN_ADDR");
    info!(addr = %cfg.listen_addr, hosts = ?cfg.allowed_hosts, valkey = cache.is_some(), "molisnap listening");

    let renderer = render::moli(cfg.moli_bin.clone(), cfg.render_timeout_ms);
    axum::serve(listener, app(AppState::new(cfg, cache, renderer)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("server error");
}

/// Exit codes: 0 all pages warmed, 1 sitemap error or any page failed, 2 bad arguments.
async fn warm_cli(args: &[String]) -> i32 {
    let mut sitemap = None;
    let mut opts = WarmOptions { via: None, format: Format::Html, concurrency: 4 };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let parsed = match arg.as_str() {
            "--via" => args.next().map(|v| opts.via = Some(v.clone())),
            "--format" => args.next().and_then(|v| Format::parse(v)).map(|f| opts.format = f),
            "--concurrency" => args.next().and_then(|v| v.parse().ok()).map(|n| opts.concurrency = n),
            other if !other.starts_with("--") && sitemap.is_none() => {
                sitemap = Some(other.to_string());
                Some(())
            }
            _ => None,
        };
        if parsed.is_none() {
            eprintln!("bad argument `{arg}`\n{WARM_USAGE}");
            return 2;
        }
    }
    let Some(sitemap) = sitemap else {
        eprintln!("{WARM_USAGE}");
        return 2;
    };

    let fetch = match HttpFetch::new(Duration::from_secs(60)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cannot build http client: {e}");
            return 1;
        }
    };
    match warm(&fetch, &sitemap, &opts).await {
        Ok(report) => {
            for (url, reason) in &report.failed {
                eprintln!("failed {url}: {reason}");
            }
            println!("warmed {} pages, {} failed", report.warmed, report.failed.len());
            if report.failed.is_empty() { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}
