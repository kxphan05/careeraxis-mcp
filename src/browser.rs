//! First-time (or expired) login: open a real browser window on a dedicated
//! profile, let the user sign in to NTU ADFS, and read the resulting cookies
//! over the Chrome DevTools Protocol. We never see or store the password.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use crate::client::{Client, BASE};
use crate::session::{now, profile_dir, Cookie, Jar};

fn on_path(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(bin))
            .find(|p| p.is_file())
    })
}

fn find_browser() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("CAREERAXIS_BROWSER") {
        return Ok(PathBuf::from(p));
    }
    for bin in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "microsoft-edge",
        "brave-browser",
    ] {
        if let Some(p) = on_path(bin) {
            return Ok(p);
        }
    }
    for p in [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ] {
        if std::path::Path::new(p).is_file() {
            return Ok(PathBuf::from(p));
        }
    }
    bail!("no Chrome/Chromium/Edge/Brave found; set CAREERAXIS_BROWSER to its path")
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Open a login window and wait (up to `timeout`) for a working session.
pub async fn interactive_login(timeout: Duration) -> Result<Jar> {
    let browser = find_browser()?;
    let port = free_port()?;
    let profile = profile_dir();
    std::fs::create_dir_all(&profile)?;
    let child = Command::new(&browser)
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--remote-debugging-port={port}"))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--password-store=basic",
            "--use-mock-keychain",
            "--new-window",
        ])
        .arg(format!("{BASE}/students/dashboard"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("launching {}", browser.display()))?;
    let mut child = KillOnDrop(child);

    let http = reqwest::Client::new();
    let start = Instant::now();
    let ws_url = loop {
        if let Ok(r) = http.get(format!("http://127.0.0.1:{port}/json/version")).send().await {
            if let Ok(v) = r.json::<Value>().await {
                if let Some(u) = v["webSocketDebuggerUrl"].as_str() {
                    break u.to_string();
                }
            }
        }
        if let Ok(Some(_)) = child.0.try_wait() {
            bail!("the browser exited at once — is a CareerAxis login window already open? Close it and retry");
        }
        if start.elapsed() > Duration::from_secs(30) {
            bail!("browser DevTools endpoint never came up");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url.as_str()).await?;
    let mut id = 0u64;

    loop {
        if start.elapsed() > timeout {
            bail!("timed out waiting for the CareerAxis login to finish");
        }
        if let Ok(Some(_)) = child.0.try_wait() {
            bail!("the login window was closed before the login finished");
        }
        id += 1;
        ws.send(Message::text(json!({"id": id, "method": "Storage.getCookies"}).to_string()))
            .await?;
        let cookies = loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), ws.next())
                .await
                .map_err(|_| anyhow!("DevTools stopped responding"))?
                .ok_or_else(|| anyhow!("DevTools connection closed"))??;
            if let Message::Text(t) = msg {
                let v: Value = serde_json::from_str(&t)?;
                if v["id"].as_u64() == Some(id) {
                    break v["result"]["cookies"].as_array().cloned().unwrap_or_default();
                }
            }
        };
        let jar = Jar {
            cookies: cookies
                .iter()
                .filter(|c| c["domain"].as_str().is_some_and(|d| d.ends_with("ntu.edu.sg")))
                .map(|c| Cookie {
                    name: c["name"].as_str().unwrap_or_default().into(),
                    value: c["value"].as_str().unwrap_or_default().into(),
                    domain: c["domain"].as_str().unwrap_or_default().trim_start_matches('.').into(),
                    path: c["path"].as_str().unwrap_or("/").into(),
                })
                .collect(),
            login_at: Some(now()),
            renewed_at: None,
        };
        if jar.has(crate::client::HOST, ".CHAUTH") {
            let probe = Client::new(jar.clone(), false)?;
            let r = probe
                .raw(
                    reqwest::Method::GET,
                    &format!("{BASE}/services/students/jobs/search?studentSiteId=1&pageIndex=0&pageSize=1&saveAsRecent=false"),
                    None,
                    "application/json",
                )
                .await;
            if matches!(&r, Ok(r) if r.status == 200) {
                let jar = probe.jar().await;
                let _ = ws
                    .send(Message::text(json!({"id": id + 1, "method": "Browser.close"}).to_string()))
                    .await;
                return Ok(jar);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
