//! Cookie jar persisted to disk.
//!
//! Everything CareerAxis and NTU's ADFS (loginfs.ntu.edu.sg) hand out is a
//! *session* cookie, so a browser would forget them on restart. We keep them
//! ourselves in `~/.config/careeraxis-mcp/jar.json` (mode 0600). The ADFS
//! cookies (`MSISAuth*`) are what let `Client::renew` mint a fresh CareerAxis
//! session over plain HTTP, without a browser or a password.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Normalised: no leading dot.
    pub domain: String,
    #[serde(default = "root")]
    pub path: String,
}

fn root() -> String {
    "/".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Jar {
    pub cookies: Vec<Cookie>,
    /// Unix seconds of the last successful interactive login.
    #[serde(default)]
    pub login_at: Option<u64>,
    /// Unix seconds of the last successful silent renewal.
    #[serde(default)]
    pub renewed_at: Option<u64>,
}

pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("CAREERAXIS_HOME") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("careeraxis-mcp")
}

pub fn jar_path() -> PathBuf {
    home().join("jar.json")
}

pub fn profile_dir() -> PathBuf {
    home().join("profile")
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn domain_matches(cookie_domain: &str, host: &str) -> bool {
    host == cookie_domain || host.ends_with(&format!(".{cookie_domain}"))
}

impl Jar {
    pub fn load() -> Result<Jar> {
        let p = jar_path();
        let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        serde_json::from_str(&s).context("parsing jar.json")
    }

    pub fn load_or_default() -> Jar {
        Jar::load().unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let dir = home();
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let tmp = dir.join("jar.json.tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut f = opts.open(&tmp)?;
            use std::io::Write;
            f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        }
        std::fs::rename(&tmp, jar_path())?;
        Ok(())
    }

    pub fn header_for(&self, host: &str, path: &str) -> Option<String> {
        let parts: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| domain_matches(&c.domain, host) && path.starts_with(&c.path))
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    pub fn has(&self, host: &str, name: &str) -> bool {
        self.cookies
            .iter()
            .any(|c| c.name == name && domain_matches(&c.domain, host))
    }

    pub fn has_idp_session(&self) -> bool {
        self.cookies
            .iter()
            .any(|c| c.domain.ends_with("loginfs.ntu.edu.sg") && c.name.starts_with("MSISAuth"))
    }

    pub fn set(&mut self, c: Cookie) {
        self.remove(&c.name, &c.domain, &c.path);
        self.cookies.push(c);
    }

    fn remove(&mut self, name: &str, domain: &str, path: &str) {
        self.cookies
            .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
    }

    /// Forget every cookie belonging to `host` (used before a renewal so the
    /// SP starts a clean SAML exchange).
    pub fn clear_host(&mut self, host: &str) {
        self.cookies.retain(|c| !domain_matches(&c.domain, host));
    }

    /// Apply one `Set-Cookie` header received from `host`.
    pub fn apply_set_cookie(&mut self, host: &str, header: &str) {
        let mut parts = header.split(';');
        let Some((name, value)) = parts.next().and_then(|nv| nv.split_once('=')) else {
            return;
        };
        let (name, value) = (name.trim().to_string(), value.trim().to_string());
        let mut domain = host.to_string();
        let mut path = "/".to_string();
        let mut delete = false;
        for attr in parts {
            let (k, v) = attr.split_once('=').unwrap_or((attr, ""));
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            match k.as_str() {
                "domain" if !v.is_empty() => domain = v.trim_start_matches('.').to_ascii_lowercase(),
                "path" if !v.is_empty() => path = v.to_string(),
                "max-age" => delete |= v.parse::<i64>().map(|n| n <= 0).unwrap_or(false),
                "expires" => delete |= expires_in_past(v),
                _ => {}
            }
        }
        if delete || value.is_empty() {
            self.remove(&name, &domain, &path);
        } else {
            self.set(Cookie { name, value, domain, path });
        }
    }
}

/// Cheap check that only needs the year: servers expire cookies with dates
/// like `Mon, 11-Oct-1999 ...`, never with a date later this year.
fn expires_in_past(v: &str) -> bool {
    let this_year = 1970 + (now() / 31_556_952) as i64;
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|t| t.len() == 4)
        .filter_map(|t| t.parse::<i64>().ok())
        .any(|y| y < this_year)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_cookie_roundtrip() {
        let mut j = Jar::default();
        j.apply_set_cookie("careeraxis.ntu.edu.sg", ".CHAUTH=abc; path=/; secure; HttpOnly");
        j.apply_set_cookie("loginfs.ntu.edu.sg", "MSISAuth=xyz; path=/adfs; HttpOnly");
        assert_eq!(j.header_for("careeraxis.ntu.edu.sg", "/x").as_deref(), Some(".CHAUTH=abc"));
        assert_eq!(j.header_for("loginfs.ntu.edu.sg", "/adfs/ls/").as_deref(), Some("MSISAuth=xyz"));
        assert!(j.header_for("loginfs.ntu.edu.sg", "/other").is_none());
        j.apply_set_cookie("careeraxis.ntu.edu.sg", ".CHAUTH=; expires=Mon, 11-Oct-1999 21:00:00 GMT; path=/");
        assert!(!j.has("careeraxis.ntu.edu.sg", ".CHAUTH"));
        assert!(j.has_idp_session());
    }
}
