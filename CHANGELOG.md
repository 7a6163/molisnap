# Changelog

All notable changes to this project are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- Iframes are no longer loaded during a render (`--disable-subframes`). Embeds like chat widgets, video players and live code editors kept the network from going idle and used most of the time and memory on heavy pages: react.dev pages went from 4.3 s and ~550 MB to 1.4 s and ~145 MB per render. The `<iframe>` tags stay in the HTML. Cached pages stay valid, so the cache key is unchanged.

## [0.2.0] - 2026-10-18

### Added

- `RENDER_WAIT_SCRIPT`: a JS expression (e.g. `window.prerenderReady === true`) the page makes truthy when it is ready. Captures then wait for the page's own signal instead of network idle, and a page that never signals fails instead of being cached half-loaded.
- Tracking parameters (`utm_*`, `fbclid`, `gclid` and similar) are stripped before a URL is rendered or cached, so links that differ only in click ids share one render and one cache entry. Configure with `STRIP_QUERY_PARAMS`; set it to empty to turn stripping off.
- With Valkey, a page that is not cached yet renders once across all replicas: the first replica takes a lock in Valkey and the others wait for its result instead of rendering it again. If that render isn't cacheable, or its replica dies, a waiting replica renders the page itself.

### Fixed

- SPAs that load their content from an API were captured before the data arrived, so crawlers got the loading shell (and it was cached). Pages are now captured once the network is idle.

### Upgrading

- The Valkey cache key moves to `molisnap:v2`, so shells cached by 0.1.0 are no longer served. The cache starts cold after the upgrade; old `molisnap:v1:*` entries expire on their own. With nginx or a CDN caching instead, purge it to drop the shells.
- Waiting for network idle makes renders slower (about 1.5–2 s more on a page measured during development). A page that always has a request open runs the full `RENDER_TIMEOUT_MS`; use `RENDER_WAIT_SCRIPT` for those.

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

[Unreleased]: https://github.com/7a6163/molisnap/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/7a6163/molisnap/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/7a6163/molisnap/releases/tag/v0.1.0
