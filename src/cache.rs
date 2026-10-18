use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

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
        Ok(Self {
            conn: ConnectionManager::new(client).await?,
        })
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

    /// Cross-replica render lock (`{key}:lock`), so a page renders once across replicas.
    /// Expires on its own if the holder dies. Fails open to `Acquired`.
    pub async fn lock(&self, key: &str, ttl_secs: u64) -> Lock {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let token = format!(
            "{}-{}-{}",
            std::process::id(),
            crate::now_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let mut c = self.conn.clone();
        let res: redis::RedisResult<Option<String>> = redis::cmd("SET")
            .arg(lock_key(key))
            .arg(&token)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut c)
            .await;
        match res {
            Ok(Some(_)) => Lock::Acquired(token),
            Ok(None) => Lock::Held,
            Err(e) => {
                warn!(%key, error = %e, "valkey lock failed");
                Lock::Acquired(token)
            }
        }
    }

    /// Releases the lock only if it is still ours (it may have expired and been retaken).
    pub async fn unlock(&self, key: &str, token: &str) {
        let mut c = self.conn.clone();
        let res: redis::RedisResult<i64> = redis::Script::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) end return 0",
        )
        .key(lock_key(key))
        .arg(token)
        .invoke_async(&mut c)
        .await;
        if let Err(e) = res {
            warn!(%key, error = %e, "valkey unlock failed");
        }
    }

    /// Whether some replica holds the render lock. Errors read as unlocked, so waiters render.
    pub async fn locked(&self, key: &str) -> bool {
        let mut c = self.conn.clone();
        match c.exists(lock_key(key)).await {
            Ok(v) => v,
            Err(e) => {
                warn!(%key, error = %e, "valkey lock check failed");
                false
            }
        }
    }
}

pub enum Lock {
    /// Ours; pass the token to [`Cache::unlock`].
    Acquired(String),
    /// Another replica is rendering this key.
    Held,
}

fn lock_key(key: &str) -> String {
    format!("{key}:lock")
}

fn field<T: FromStr>(m: &HashMap<String, Vec<u8>>, name: &str) -> Option<T> {
    std::str::from_utf8(m.get(name)?).ok()?.parse().ok()
}
