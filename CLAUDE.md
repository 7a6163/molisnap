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

Running locally needs a `moli` binary (`MOLI_BIN`); Valkey is optional. Port 6379 on this machine is taken by another project's Valkey, so start a test one on another port: `docker run -d --rm -p 6390:6379 valkey/valkey:9-alpine`. The compose file doesn't publish Valkey's port.

## Tests

Development is test-first (red → green, one slice at a time). Tests sit at three agreed seams; don't add tests against internals:

- `tests/http.rs`: the HTTP router via `app()` + `tower::ServiceExt::oneshot`, with a `FakeRenderer` injected in place of Moli (counts calls, optional delay). Covers headers, `X-Cache`, single-flight. Valkey-mode tests use a real Valkey through `VALKEY_TEST_URL`; each test uses a unique URL so runs don't collide.
- `tests/warm.rs`: `warm::warm()` (the `molisnap warm` sitemap prewarmer) with a `FakeWeb` implementing the `Fetch` trait; the real `HttpFetch` (reqwest) and CLI arg parsing in `main.rs` are untested wiring.
- `tests/moli.rs`: contract tests for `render::render` against a real Moli binary and the network (`MOLI_BIN`).
- Unit tests in `src/` only for pure helpers (allowlist, URL validation, stderr parsing).

CI (`.github/workflows/ci.yml`) runs fmt, clippy `-D warnings` and the full suite with a Valkey service container and the Linux Moli release. With `CI` set, the Valkey/Moli tests fail instead of skipping when their env var is missing. CI's `MOLI_VERSION` must match the Dockerfile's. After `test` passes, CI builds the image on native amd64 and arm64 runners, then `docker-merge` pushes a multi-arch manifest to `ghcr.io/7a6163/molisnap` (tags `main` + `sha-*` from main, `X.Y.Z` / `X.Y` / `latest` from `v*` git tags). PRs build the image but don't push.

## Releases

Record user-facing changes under `## [Unreleased]` in `CHANGELOG.md` (Keep a Changelog). To release: bump `version` in `Cargo.toml`, move Unreleased into a dated version section, commit, push a `vX.Y.Z` tag (CI publishes the `X.Y.Z`/`X.Y`/`latest` images), then `gh release create vX.Y.Z --verify-tag` with that section as notes.

## Architecture

`src/main.rs` only wires env config into `AppState::new(cfg, Option<Cache>, renderer)`; all logic is in `src/lib.rs`. The renderer is an injected `Renderer` closure (`render::moli` in production). The request path is `serve()`:

1. `validate_url`: http(s) only, host must match `ALLOWED_HOSTS`, fragment dropped, tracking parameters (`STRIP_QUERY_PARAMS`) removed from the raw query so the rest keep their order and encoding. Moli also runs with `--block-private-networks`. Together these stop SSRF. Don't loosen either one.
2. Cache key `molisnap:v1:{format}:{sha256(url)}`. Bump `v1` when the stored layout or render flags change.
3. Valkey (if configured) hit: if younger than `CACHE_FRESH_SECS`, return it (`HIT`). If older, return it anyway (`STALE`) and refresh in the background (`Peer::Skip`).
4. Miss (`Peer::Wait`): `render_once` is in-process single-flight. Concurrent requests for a key share one `Shared` future wrapping a `tokio::spawn`ed task, so a render finishes and fills the cache even if clients disconnect. A `Semaphore` caps concurrent Moli processes.
5. Across replicas, `coordinated_render` takes a Valkey render lock (`{key}:lock`, `SET NX EX` with a token, released by compare-and-delete). If another replica holds it, a refresh is skipped, and a miss polls the cache until the page lands. If the lock goes away without a page (uncacheable result, dead replica) or it times out, the miss renders the page itself instead of erroring.

- `src/render.rs` runs one `moli fetch` subprocess per render. Moli exits 0 on success. With `--fail-with-body` it exits 22 for an origin status >= 400, writes the body, and puts the status on stderr ("HTTP status 404"), which `parse_status` reads. HTML output uses `--strip-mode js`. Every render also runs `META_STATUS_CHECK` via `--eval`: if the page has `<meta name="prerender-status-code">` with a 4xx/5xx code, the script throws, Moli exits 1 with no dump, and `parse_meta_status` reads the code from stderr (empty body). It throws instead of reading the meta from the output because the Markdown and semantic dumps don't include `<head>`. Run `moli fetch --help` for the full flag list.
- `src/cache.rs`: each page is a Valkey hash (`body`, `status`, `at`) that expires after fresh + stale. All cache ops fail open: they log a warning and the request renders without the cache.
- Only 2xx/404/410 are cached, by Valkey and downstream. They get `Cache-Control: public, max-age=…, stale-while-revalidate=…, stale-if-error=…`, everything else `no-store`, so a flaky origin recovers.

`molisnap warm` (`src/warm.rs`) is a client, not part of the server: it reads a sitemap (one level of sitemap index) and requests each page either as `{via}/{page}` (warms molisnap's Valkey) or directly with UA `molisnap-warm/0.1` (warms nginx/CDN, which must treat that UA as a bot). It sends both `Accept` and `X-Render-Format` because nginx and molisnap's path mode each read a different one.

## Docker

The runtime image downloads the Linux Moli release (`MOLI_VERSION` build arg). That binary needs `libfontconfig1`. After bumping Moli, check for missing libs with `docker run --rm --entrypoint sh molisnap -c 'ldd /usr/local/bin/moli | grep "not found"'`.
