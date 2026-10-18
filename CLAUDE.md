# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

SPA prerendering service for crawlers and AI bots: renders pages with the [Moli](https://github.com/lexmount/moli) headless browser (a Rust browser kernel, not Chromium). Caching is either in Valkey (`VALKEY_URL` set) or left to nginx/CDN via `Cache-Control` (unset). See README.md for the API and env vars.

## Commands

`cargo` lives in `~/.cargo/bin`, which may not be on PATH (`export PATH=$HOME/.cargo/bin:$PATH`).

```sh
cargo clippy --all-targets          # lint
cargo test                          # all tests; Valkey/Moli tests skip themselves when their env var is unset
cargo test --test http concurrent   # single test (name substring)
VALKEY_TEST_URL=redis://127.0.0.1:6390 MOLI_BIN=/path/to/moli cargo test   # full suite incl. real Valkey + Moli
docker compose up --build           # full stack; needs ALLOWED_HOSTS (defaults to example.com)
```

Running locally needs a `moli` binary (`MOLI_BIN`); Valkey is optional. Port 6379 on this machine is taken by another project's Valkey, so start a test one on another port: `docker run -d --rm -p 6390:6379 valkey/valkey:8-alpine`. The compose file doesn't publish Valkey's port.

## Tests

Development is test-first (red → green, one slice at a time). Tests sit at three agreed seams; don't add tests against internals:

- `tests/http.rs`: the HTTP router via `app()` + `tower::ServiceExt::oneshot`, with a `FakeRenderer` injected in place of Moli (counts calls, optional delay). Covers headers, `X-Cache`, single-flight. Valkey-mode tests use a real Valkey through `VALKEY_TEST_URL`; each test uses a unique URL so runs don't collide.
- `tests/warm.rs`: `warm::warm()` (the `molisnap warm` sitemap prewarmer) with a `FakeWeb` implementing the `Fetch` trait; the real `HttpFetch` (reqwest) and CLI arg parsing in `main.rs` are untested wiring.
- `tests/moli.rs`: contract tests for `render::render` against a real Moli binary and the network (`MOLI_BIN`).
- Unit tests in `src/` only for pure helpers (allowlist, URL validation, stderr parsing).

## Architecture

`src/main.rs` only wires env config into `AppState::new(cfg, Option<Cache>, renderer)`; all logic is in `src/lib.rs`. The renderer is an injected `Renderer` closure (`render::moli` in production). The request path is `serve()`:

1. `validate_url`: http(s) only, host must match `ALLOWED_HOSTS`, fragment dropped. Moli also runs with `--block-private-networks`. Together these stop SSRF. Don't loosen either one.
2. Cache key `molisnap:v1:{format}:{sha256(url)}`. Bump `v1` when the stored layout or render flags change.
3. Valkey (if configured) hit: if younger than `CACHE_FRESH_SECS`, return it (`HIT`). If older, return it anyway (`STALE`) and refresh in the background behind a Valkey `SET NX EX` lock (`{key}:lock`), so only one replica refreshes.
4. Miss: `render_once` is in-process single-flight. Concurrent requests for a key share one `Shared` future wrapping a `tokio::spawn`ed task, so a render finishes and fills the cache even if clients disconnect. A `Semaphore` caps concurrent Moli processes.

- `src/render.rs` runs one `moli fetch` subprocess per render. Moli exits 0 on success. With `--fail-with-body` it exits 22 for an origin status >= 400, writes the body, and puts the status on stderr ("HTTP status 404"), which `parse_status` reads. HTML output uses `--strip-mode js`. Every render also runs `META_STATUS_CHECK` via `--eval`: if the page has `<meta name="prerender-status-code">` with a 4xx/5xx code, the script throws, Moli exits 1 with no dump, and `parse_meta_status` reads the code from stderr (empty body). It throws instead of reading the meta from the output because the Markdown and semantic dumps don't include `<head>`. Run `moli fetch --help` for the full flag list.
- `src/cache.rs`: each page is a Valkey hash (`body`, `status`, `at`) that expires after fresh + stale. All cache ops fail open: they log a warning and the request renders without the cache.
- Only 2xx/404/410 are cached, by Valkey and downstream. They get `Cache-Control: public, max-age=…, stale-while-revalidate=…, stale-if-error=…`, everything else `no-store`, so a flaky origin recovers.

`molisnap warm` (`src/warm.rs`) is a client, not part of the server: it reads a sitemap (one level of sitemap index) and requests each page either as `{via}/{page}` (warms molisnap's Valkey) or directly with UA `molisnap-warm/0.1` (warms nginx/CDN, which must treat that UA as a bot). It sends both `Accept` and `X-Render-Format` because nginx and molisnap's path mode each read a different one.

## Docker

The runtime image downloads the Linux Moli release (`MOLI_VERSION` build arg). That binary needs `libfontconfig1`. After bumping Moli, check for missing libs with `docker run --rm --entrypoint sh molisnap -c 'ldd /usr/local/bin/moli | grep "not found"'`.
