use std::collections::HashMap;
use std::str::FromStr;

use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tracing::warn;

use crate::Page;

/// Valkey-backed page cache. Each page is one hash: `body`, `status`, `at` (unix secs).
///
/// Every operation fails open: a Valkey outage degrades to rendering on every request
/// instead of failing requests.
#[derive(Clone)]
pub struct Cache {
    conn: ConnectionManager,
}

impl Cache {
    pub async fn connect(url: &str) -> redis::RedisResult<Self> {
        let client = redis::Client::open(url)?;
        Ok(Self { conn: ConnectionManager::new(client).await? })
    }

    pub async fn get(&self, key: &str) -> Option<Page> {
        let mut c = self.conn.clone();
        let m: HashMap<String, Vec<u8>> = match c.hgetall(key).await {
            Ok(m) => m,
            Err(e) => {
                warn!(%key, error = %e, "valkey get failed");
                return None;
            }
        };
        Some(Page {
            status: field(&m, "status")?,
            rendered_at: field(&m, "at")?,
            body: m.get("body")?.clone().into(),
        })
    }

    /// Stores the page; the key expires after `ttl_secs` (fresh + stale window).
    pub async fn set(&self, key: &str, page: &Page, ttl_secs: u64) {
        let mut c = self.conn.clone();
        let res: redis::RedisResult<()> = redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(key)
            .arg("body")
            .arg(&page.body[..])
            .arg("status")
            .arg(page.status)
            .arg("at")
            .arg(page.rendered_at)
            .ignore()
            .expire(key, ttl_secs as i64)
            .ignore()
            .query_async(&mut c)
            .await;
        if let Err(e) = res {
            warn!(%key, error = %e, "valkey set failed");
        }
    }

    /// Cross-instance guard so only one replica refreshes a stale page. Expires on its own.
    pub async fn try_lock(&self, key: &str, ttl_secs: u64) -> bool {
        let mut c = self.conn.clone();
        let res: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(format!("{key}:lock"))
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut c)
            .await;
        match res {
            Ok(v) => v.is_some(),
            Err(e) => {
                warn!(%key, error = %e, "valkey lock failed");
                true
            }
        }
    }
}

fn field<T: FromStr>(m: &HashMap<String, Vec<u8>>, name: &str) -> Option<T> {
    std::str::from_utf8(m.get(name)?).ok()?.parse().ok()
}
