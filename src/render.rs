use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use futures::FutureExt;

use crate::{Format, Page, Renderer, now_secs};

/// The production [`Renderer`]: one `moli fetch` subprocess per render.
pub fn moli(moli_bin: String, timeout_ms: u64) -> Renderer {
    std::sync::Arc::new(move |url, format| {
        let moli_bin = moli_bin.clone();
        async move { render(&moli_bin, timeout_ms, &url, format).await }.boxed()
    })
}

/// SPAs can't send a real 404, so a not-found view sets
/// `<meta name="prerender-status-code" content="404">`. Throwing makes Moli fail without a
/// dump, and `parse_meta_status` reads the code back from stderr. The Markdown and semantic
/// dumps drop `<head>`, so this is the one signal that works for every format in one render.
const META_STATUS_CHECK: &str = r#"(() => {
  const m = document.querySelector('meta[name="prerender-status-code"]');
  const code = m && parseInt(m.content, 10);
  if (code >= 400 && code <= 599) throw new Error("molisnap-status=" + code);
})()"#;

/// Runs `moli fetch` once and returns the rendered page.
///
/// Exit 0 means a 2xx/3xx document. With `--fail-with-body`, an origin status >= 400
/// exits 22 but still writes the body, and the status is reported on stderr.
pub async fn render(moli_bin: &str, timeout_ms: u64, url: &str, format: Format) -> Result<Page, String> {
    let mut cmd = Command::new(moli_bin);
    cmd.args(["fetch", "--dump", format.moli_dump(), "--wait", "done"])
        .args(["--timeout", &timeout_ms.to_string()])
        .args(["--fail-with-body", "--block-private-networks"])
        .args(["--eval", META_STATUS_CHECK]);
    if format == Format::Html {
        // Crawlers get a static snapshot; re-running the SPA's JS on top of it would double-render.
        cmd.args(["--strip-mode", "js"]);
    }
    cmd.arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Moli enforces its own readiness deadline; this outer one only catches a hung process.
    let out = tokio::time::timeout(Duration::from_millis(timeout_ms + 5_000), cmd.output())
        .await
        .map_err(|_| "moli timed out".to_string())?
        .map_err(|e| format!("failed to spawn {moli_bin}: {e}"))?;

    let stderr = String::from_utf8_lossy(&out.stderr);
    let status = match out.status.code() {
        Some(0) => 200,
        Some(22) => parse_status(&stderr).unwrap_or(502),
        Some(1) if let Some(status) = parse_meta_status(&stderr) => {
            return Ok(Page { status, body: Default::default(), rendered_at: now_secs() });
        }
        _ => return Err(format!("moli failed ({}): {}", out.status, stderr.trim())),
    };
    Ok(Page { status, body: out.stdout.into(), rendered_at: now_secs() })
}

/// Pulls the status out of e.g. "final main document returned HTTP status 404 at `...`".
fn parse_status(stderr: &str) -> Option<u16> {
    let i = stderr.find("HTTP status ")?;
    stderr[i + "HTTP status ".len()..].split_whitespace().next()?.parse().ok()
}

fn parse_meta_status(stderr: &str) -> Option<u16> {
    let i = stderr.find("molisnap-status=")?;
    let digits: String = stderr[i + "molisnap-status=".len()..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_from_moli_stderr() {
        let s = "Error: failed to fetch `https://x/y`\nReason: final main document returned HTTP status 404 at `https://x/y`";
        assert_eq!(parse_status(s), Some(404));
        assert_eq!(parse_status("Error: something else"), None);
    }
}
