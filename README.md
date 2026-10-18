# molisnap

[![CI](https://github.com/7a6163/molisnap/actions/workflows/ci.yml/badge.svg)](https://github.com/7a6163/molisnap/actions/workflows/ci.yml)

A prerendering service that lets search engine crawlers and AI bots read SPAs. Pages are rendered with the [Moli](https://github.com/lexmount/moli) headless browser.

Pick one of two caching modes:

- **`VALKEY_URL` unset** (recommended when nginx or a CDN sits in front): the service is stateless and sends `Cache-Control` headers, so nginx `proxy_cache` or the CDN does the caching.
- **`VALKEY_URL` set**: the service caches in Valkey itself. Use this when several replicas need to share one cache (a page then renders once across all of them), when you need to purge entries on demand, or when the service is exposed directly.

## Quick start

```sh
ALLOWED_HOSTS=example.com docker compose up --build
curl "http://localhost:3000/render?url=https://example.com/"
curl -H 'Accept: text/markdown' "http://localhost:3000/render?url=https://example.com/"
```

## Docker image

Multi-arch (amd64, arm64) images are published to GitHub Container Registry:

```sh
docker pull ghcr.io/7a6163/molisnap:main     # latest commit on main
docker pull ghcr.io/7a6163/molisnap:latest   # latest release (v* tag)
docker run -p 3000:3000 -e ALLOWED_HOSTS=example.com ghcr.io/7a6163/molisnap:main
```

## API

| Request | Description |
|---|---|
| `GET /render?url=<url>&format=html\|markdown\|semantic` | Without `format`, `Accept: text/markdown` returns Markdown; anything else returns HTML |
| `GET /<full url>` | Prerender-style path for nginx to proxy to. The format comes from the `X-Render-Format` or `Accept` header |
| `GET /healthz` | Health check |

- The response status matches the origin's (for example 404).
- An SPA can't send a real 404, so a not-found view can add `<meta name="prerender-status-code" content="404">` after rendering (any 4xx or 5xx works). The response then has that status and an empty body.
- `X-Cache` is `HIT`, `STALE` or `MISS` (always `MISS` without Valkey). `Age` is the number of seconds since the cached copy was rendered.
- Cacheable responses (2xx, 404, 410) carry `Cache-Control: public, max-age=<fresh>, stale-while-revalidate=<stale>, stale-if-error=<stale>`. Every other status gets `no-store`.
- HTML output has its `<script>` tags removed, so crawlers get a static snapshot.

## Configuration (environment variables)

| Variable | Default | Description |
|---|---|---|
| `ALLOWED_HOSTS` | **required** | Comma-separated hosts that may be rendered. `*.example.com` matches every subdomain |
| `VALKEY_URL` | unset | Valkey is not used when unset |
| `MOLI_BIN` | `moli` | |
| `LISTEN_ADDR` | `0.0.0.0:3000` | |
| `CACHE_FRESH_SECS` | `3600` | Cached pages are served as-is for this long. Also used as `max-age` |
| `CACHE_STALE_SECS` | `86400` | After the fresh window, the old page is still served for this long while it is re-rendered in the background. Also used as `stale-while-revalidate` |
| `RENDER_TIMEOUT_MS` | `20000` | |
| `MAX_CONCURRENT_RENDERS` | `8` | |
| `STRIP_QUERY_PARAMS` | built-in tracking list | Comma-separated query parameters removed before a URL is rendered or cached (case-insensitive). Set to empty to strip nothing |

### Tracking parameters

Every distinct URL is its own render and cache entry, so one link shared on Facebook or LINE with a click id can turn into many copies of the same page. Tracking parameters are removed first: `/a?id=7&fbclid=abc&utm_source=fb` is rendered and cached as `/a?id=7`, and `/a?utm_source=fb` as `/a`. The other parameters keep their order and encoding. The default list covers `utm_*`, `gclid`/`gbraid`/`wbraid`, `fbclid`, `igshid`, `ttclid`, `twclid`, `msclkid`, `yclid`, `li_fat_id`, `mc_cid`/`mc_eid`, `mkt_tok`, `_hsenc` and similar (see `DEFAULT_STRIPPED_PARAMS` in `src/lib.rs`).

With nginx caching, also leave these parameters out of `proxy_cache_key`, or nginx keeps one copy per click id even though molisnap renders the page once.

## Sitemap prewarming

```sh
# Warm molisnap's Valkey cache
molisnap warm https://example.com/sitemap.xml --via http://molisnap:3000

# Without --via: request pages directly as a bot (User-Agent: molisnap-warm/0.1) to warm the nginx or CDN cache in front
molisnap warm https://example.com/sitemap.xml --format markdown --concurrency 8
```

- Sitemap indexes are supported (one level deep, as the sitemap protocol allows). `.xml.gz` sitemaps are not.
- Exit codes: 0 when every page was warmed; 1 when the sitemap can't be fetched or any page fails (failed pages are listed on stderr); 2 for bad arguments. Suitable for cron.
- Without `--via`, nginx's bot rule must include `molisnap-warm` (the example below already does).

## Routing bots with nginx in front of the SPA

Bots go to molisnap; everyone else gets the SPA as usual:

```nginx
map $http_user_agent $is_bot {
    default 0;
    ~*(googlebot|bingbot|applebot|duckduckbot|yandex|baiduspider|gptbot|chatgpt-user|oai-searchbot|claudebot|claude-user|claude-searchbot|perplexitybot|ccbot|facebookexternalhit|twitterbot|linkedinbot|slackbot|discordbot|molisnap-warm) 1;
}

server {
    # Serve static assets directly (regex locations take precedence over /)
    location ~* \.(js|css|png|jpe?g|gif|svg|webp|ico|woff2?|map|json|txt|xml)$ {
        try_files $uri =404;
    }

    location / {
        error_page 418 = @render;
        if ($is_bot) { return 418; }
        try_files $uri /index.html;
    }

    location @render {
        proxy_set_header Accept $http_accept;
        proxy_pass http://molisnap:3000/$scheme://$host$request_uri;
    }
}
```

### Without Valkey: let nginx cache

Replace `@render` with the block below. nginx follows the `Cache-Control` that molisnap sends to decide how long to keep each page. Expired pages are served stale while being refreshed in the background, only one request per URL reaches molisnap at a time, and nginx keeps serving the stale copy if molisnap fails.

```nginx
proxy_cache_path /var/cache/nginx/render levels=1:2 keys_zone=render:50m
                 max_size=5g inactive=7d use_temp_path=off;

# Accept headers vary a lot; collapse them into two values so one page isn't cached many times
map $http_accept $render_fmt {
    default          html;
    ~text/markdown   markdown;
}

location @render {
    proxy_set_header X-Render-Format $render_fmt;
    proxy_pass http://molisnap:3000/$scheme://$host$request_uri;

    proxy_cache            render;
    proxy_cache_key        "$scheme://$host$request_uri|$render_fmt";
    proxy_cache_use_stale  updating error timeout http_500 http_502 http_503 http_504;
    proxy_cache_background_update on;
    proxy_cache_lock       on;
    proxy_cache_lock_timeout 25s;
    add_header X-Cache $upstream_cache_status;
}
```

## License

MIT, see [LICENSE](LICENSE). The Moli binary bundled in the Docker image is distributed under its Apache-2.0 or MIT license, with its notices in `/usr/share/doc/moli/` inside the image.
