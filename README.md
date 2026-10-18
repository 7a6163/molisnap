# molisnap

給搜尋引擎爬蟲與 AI bot 用的 SPA 預渲染服務。使用 [Moli](https://github.com/lexmount/moli) headless 瀏覽器渲染。

快取有兩種做法，二選一：

- **不設 `VALKEY_URL`**（建議前面有 nginx 或 CDN 時使用）：服務本身不保存狀態，回應帶 `Cache-Control`，由 nginx `proxy_cache` 或 CDN 快取。
- **設 `VALKEY_URL`**：服務自己用 Valkey 快取。適合多台機器要共用快取、需要主動清除快取，或服務直接對外的情況。

## 快速開始

```sh
ALLOWED_HOSTS=example.com docker compose up --build
curl "http://localhost:3000/render?url=https://example.com/"
curl -H 'Accept: text/markdown' "http://localhost:3000/render?url=https://example.com/"
```

## API

| 請求 | 說明 |
|---|---|
| `GET /render?url=<url>&format=html\|markdown\|semantic` | 沒給 `format` 時，`Accept: text/markdown` 回 Markdown，其他情況回 HTML |
| `GET /<完整 url>` | Prerender 風格，給 nginx 轉發用。格式由 `X-Render-Format` 或 `Accept` header 決定 |
| `GET /healthz` | 健康檢查 |

- 回應的 HTTP 狀態碼與原站相同（例如 404）。
- SPA 沒辦法回真正的 404，所以「找不到」頁面可以在渲染後加上 `<meta name="prerender-status-code" content="404">`（4xx、5xx 都可以），回應就會是該狀態碼，內容為空。
- `X-Cache` 的值是 `HIT`、`STALE` 或 `MISS`（沒有 Valkey 時一律是 `MISS`）。`Age` 是快取內容渲染後經過的秒數。
- 可快取的回應（2xx、404、410）帶 `Cache-Control: public, max-age=<fresh>, stale-while-revalidate=<stale>, stale-if-error=<stale>`，其他狀態碼帶 `no-store`。
- HTML 輸出會移除 `<script>`，給爬蟲的是靜態快照。

## 設定（環境變數）

| 變數 | 預設 | 說明 |
|---|---|---|
| `ALLOWED_HOSTS` | **必填** | 允許渲染的網域，以逗號分隔。`*.example.com` 代表所有子網域 |
| `VALKEY_URL` | 不設 | 不設就不使用 Valkey |
| `MOLI_BIN` | `moli` | |
| `LISTEN_ADDR` | `0.0.0.0:3000` | |
| `CACHE_FRESH_SECS` | `3600` | 在這段時間內直接回傳快取，也是 `max-age` |
| `CACHE_STALE_SECS` | `86400` | 超過 fresh 期間後的這段時間內，先回舊內容，同時在背景重新渲染，也是 `stale-while-revalidate` |
| `RENDER_TIMEOUT_MS` | `20000` | |
| `MAX_CONCURRENT_RENDERS` | `8` | |

## sitemap 預熱

```sh
# 預熱 molisnap 的 Valkey 快取
molisnap warm https://example.com/sitemap.xml --via http://molisnap:3000

# 沒有 --via：直接以 bot 身分（User-Agent: molisnap-warm/0.1）請求頁面，預熱前面的 nginx 或 CDN
molisnap warm https://example.com/sitemap.xml --format markdown --concurrency 8
```

- 支援 sitemap index（只往下一層，依 sitemap 規範）。不支援 `.xml.gz`。
- 回傳值：全部成功是 0；sitemap 抓不到或有頁面失敗是 1（失敗的頁面會列在 stderr）；參數錯誤是 2。適合放進 cron。
- 不用 `--via` 時，nginx 的 bot 判斷要包含 `molisnap-warm`（下面的範例已經加上）。

## 在 SPA 前面用 nginx 分流

bot 轉給 molisnap，一般使用者照常拿 SPA：

```nginx
map $http_user_agent $is_bot {
    default 0;
    ~*(googlebot|bingbot|applebot|duckduckbot|yandex|baiduspider|gptbot|chatgpt-user|oai-searchbot|claudebot|claude-user|claude-searchbot|perplexitybot|ccbot|facebookexternalhit|twitterbot|linkedinbot|slackbot|discordbot|molisnap-warm) 1;
}

server {
    # 靜態資源直接回（regex location 優先於 /）
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

### 不用 Valkey：讓 nginx 快取

`@render` 換成下面這段。nginx 會依 molisnap 回的 `Cache-Control` 決定快取多久。過期時先回舊內容、在背景更新；同一網址只會有一個請求打到 molisnap；molisnap 出錯時也會繼續回舊快取。

```nginx
proxy_cache_path /var/cache/nginx/render levels=1:2 keys_zone=render:50m
                 max_size=5g inactive=7d use_temp_path=off;

# Accept header 的寫法太多種，先歸成兩類，避免同一頁快取出很多份
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

## 授權

MIT，見 [LICENSE](LICENSE)。Docker image 內附的 Moli 依其 Apache-2.0 或 MIT 授權散布，授權聲明放在 image 的 `/usr/share/doc/moli/`。
