//! HTTP client for CareerAxis with transparent session renewal.
//!
//! Redirects are followed by hand so every hop's `Set-Cookie` lands in our
//! jar (including the ADFS hops during a SAML exchange).

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Result};
use reqwest::{Method, Url};
use scraper::{Html, Selector};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::session::{now, Jar};

pub const BASE: &str = "https://careeraxis.ntu.edu.sg";
pub const HOST: &str = "careeraxis.ntu.edu.sg";
const UA: &str = concat!("careeraxis-mcp/", env!("CARGO_PKG_VERSION"));

/// Marker text so callers (MCP + CLI) can tell auth failures apart.
pub const NEEDS_LOGIN: &str = "NEEDS_LOGIN";

pub struct Resp {
    pub status: u16,
    pub url: Url,
    pub body: String,
}

impl Resp {
    fn looks_logged_out(&self) -> bool {
        let u = self.url.as_str();
        self.status == 401
            || self.url.host_str() == Some("loginfs.ntu.edu.sg")
            || u.contains("/students/login")
            || u.contains("/providers/saml/")
    }
}

pub struct Client {
    http: reqwest::Client,
    jar: Mutex<Jar>,
    renew_lock: Mutex<()>,
    /// Bumped on every successful renewal; lets concurrent callers that all
    /// saw a 401 share one renewal instead of running several.
    generation: AtomicU64,
    persist: bool,
}

impl Client {
    pub fn new(jar: Jar, persist: bool) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(UA)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Client {
            http,
            jar: Mutex::new(jar),
            renew_lock: Mutex::new(()),
            generation: AtomicU64::new(0),
            persist,
        })
    }

    pub async fn jar(&self) -> Jar {
        self.jar.lock().await.clone()
    }

    /// Swap in a freshly captured jar (after an interactive login).
    pub async fn install(&self, jar: Jar) {
        *self.jar.lock().await = jar;
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    async fn save(&self) {
        if self.persist {
            if let Err(e) = self.jar.lock().await.save() {
                eprintln!("careeraxis-mcp: could not save session: {e:#}");
            }
        }
    }

    /// One request, following up to 10 redirects, recording cookies per hop.
    pub async fn raw(
        &self,
        method: Method,
        url: &str,
        form: Option<Vec<(String, String)>>,
        accept: &str,
    ) -> Result<Resp> {
        let mut url = Url::parse(url)?;
        let mut method = method;
        let mut form = form;
        for _ in 0..10 {
            let host = url.host_str().unwrap_or_default().to_string();
            let cookie = self.jar.lock().await.header_for(&host, url.path());
            let mut req = self.http.request(method.clone(), url.clone()).header("Accept", accept);
            if let Some(c) = cookie {
                req = req.header("Cookie", c);
            }
            if let Some(f) = &form {
                req = req.form(f);
            }
            let resp = req.send().await?;
            {
                let mut jar = self.jar.lock().await;
                for v in resp.headers().get_all("set-cookie") {
                    if let Ok(s) = v.to_str() {
                        jar.apply_set_cookie(&host, s);
                    }
                }
            }
            let status = resp.status().as_u16();
            if (300..400).contains(&status) {
                if let Some(loc) = resp.headers().get("location").and_then(|l| l.to_str().ok()) {
                    url = url.join(loc)?;
                    if status != 307 && status != 308 {
                        method = Method::GET;
                        form = None;
                    }
                    continue;
                }
            }
            let body = resp.text().await.unwrap_or_default();
            return Ok(Resp { status, url, body });
        }
        bail!("too many redirects")
    }

    /// Mint a fresh CareerAxis session from the saved ADFS session, over plain
    /// HTTP: SP redirect page -> POST SAMLRequest to ADFS -> POST SAMLResponse
    /// back to CareerAxis.
    pub async fn renew(&self, seen_generation: u64) -> Result<()> {
        let _g = self.renew_lock.lock().await;
        if self.generation.load(Ordering::SeqCst) != seen_generation {
            return Ok(()); // someone else renewed while we waited
        }
        if !self.jar.lock().await.has_idp_session() {
            bail!("{NEEDS_LOGIN}: no saved NTU login. Run `careeraxis-mcp setup` (or call the careeraxis_login tool).");
        }
        self.jar.lock().await.clear_host(HOST);
        let mut r = self
            .raw(Method::GET, &format!("{BASE}/students/dashboard"), None, "text/html")
            .await?;
        for _ in 0..4 {
            match parse_saml_form(&r.body, &r.url) {
                Some((action, fields)) => {
                    r = self.raw(Method::POST, action.as_str(), Some(fields), "text/html").await?;
                }
                None => break,
            }
        }
        let ok = r.url.host_str() == Some(HOST)
            && !r.looks_logged_out()
            && self.jar.lock().await.has(HOST, ".CHAUTH");
        if !ok {
            // ADFS showed its username/password form: the IdP session is gone.
            bail!("{NEEDS_LOGIN}: the saved NTU login has expired. Run `careeraxis-mcp setup` (or call the careeraxis_login tool).");
        }
        self.jar.lock().await.renewed_at = Some(now());
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.save().await;
        Ok(())
    }

    async fn get_authed(&self, path: &str, accept: &str) -> Result<Resp> {
        let url = format!("{BASE}{path}");
        for attempt in 0..2 {
            let gen = self.generation.load(Ordering::SeqCst);
            let r = self.raw(Method::GET, &url, None, accept).await?;
            if !r.looks_logged_out() {
                if attempt > 0 {
                    self.save().await;
                }
                return Ok(r);
            }
            if attempt == 0 {
                self.renew(gen).await?;
            }
        }
        bail!("{NEEDS_LOGIN}: still logged out after renewing the session. Run `careeraxis-mcp setup`.")
    }

    /// GET a `/services/students/...` JSON endpoint.
    pub async fn get_json(&self, path: &str) -> Result<Value> {
        let r = self.get_authed(path, "application/json").await?;
        if r.status == 404 && !r.body.trim_start().starts_with(['{', '[']) {
            bail!("not available on CareerAxis: {}", r.body.trim());
        }
        if r.status >= 400 {
            bail!("HTTP {} from {path}: {}", r.status, truncate(r.body.trim(), 300));
        }
        if r.body.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&r.body).map_err(|e| anyhow!("bad JSON from {path}: {e}"))
    }

    /// GET a server-rendered student page.
    pub async fn get_html(&self, path: &str) -> Result<String> {
        let r = self.get_authed(path, "text/html").await?;
        if r.status >= 400 {
            bail!("HTTP {} from {path}", r.status);
        }
        if r.body.contains("Sorry, but whatever you're looking for") {
            bail!("not found: {path}");
        }
        Ok(r.body)
    }
}

/// Find an auto-submitting SAML form (`SAMLRequest`/`SAMLResponse`).
fn parse_saml_form(body: &str, base: &Url) -> Option<(Url, Vec<(String, String)>)> {
    if !body.contains("SAMLRequest") && !body.contains("SAMLResponse") {
        return None;
    }
    let doc = Html::parse_document(body);
    let form_sel = Selector::parse("form").ok()?;
    let input_sel = Selector::parse("input[name]").ok()?;
    for form in doc.select(&form_sel) {
        let fields: Vec<(String, String)> = form
            .select(&input_sel)
            .map(|i| {
                let a = i.value();
                (a.attr("name").unwrap_or_default().to_string(), a.attr("value").unwrap_or_default().to_string())
            })
            .collect();
        if fields.iter().any(|(n, _)| n.starts_with("SAML")) {
            let action = base.join(form.value().attr("action").unwrap_or("")).ok()?;
            return Some((action, fields));
        }
    }
    None
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}
