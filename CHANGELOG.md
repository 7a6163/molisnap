# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.0] - 2026-10-18

First release.

### Added

- Prerendering of SPAs with the [Moli](https://github.com/lexmount/moli) headless browser, served as HTML (scripts stripped), Markdown or a semantic tree.
- `GET /render?url=…&format=…` API, and a prerender-style `GET /<full url>` path for nginx to proxy bots to. Without an explicit format, `Accept: text/markdown` selects Markdown.
- Origin status codes are passed through. SPAs can set their own status with `<meta name="prerender-status-code" content="404">` (any 4xx/5xx).
- Two caching modes:
  - Without `VALKEY_URL`: stateless, with `Cache-Control: public, max-age, stale-while-revalidate, stale-if-error` for nginx or a CDN to cache on. Errors get `no-store`.
  - With `VALKEY_URL`: Valkey cache with stale-while-revalidate and a cross-replica refresh lock. A Valkey outage falls back to rendering instead of failing requests.
- In-process single-flight, so concurrent requests for the same page share one render, and a cap on concurrent Moli processes.
- `ALLOWED_HOSTS` allowlist plus Moli's `--block-private-networks` to prevent SSRF.
- `molisnap warm` CLI that prewarms the cache from a sitemap or sitemap index, either through molisnap (`--via`) or directly as a bot to warm nginx or a CDN.
- Multi-arch (amd64, arm64) Docker image at `ghcr.io/7a6163/molisnap`, bundling Moli and its license notices.

[Unreleased]: https://github.com/7a6163/molisnap/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/7a6163/molisnap/releases/tag/v0.1.0
