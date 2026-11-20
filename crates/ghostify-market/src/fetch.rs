//! Downloading from GitHub. The trait keeps the service testable without a network.

use crate::index::check_download_url;
use ghostify_core::{Error, Result};
use std::io::Read;
use std::time::Duration;

pub trait Fetcher: Send + Sync {
    /// Returns the body of `url`, failing if it is larger than `max_bytes`.
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>>;
}

const MAX_REDIRECTS: usize = 3;

/// Resolves a `Location` header against the current url. Only absolute https
/// urls and absolute paths are accepted, and the result must again be an
/// allowed download address.
pub fn resolve_redirect(current: &str, location: &str, extra_hosts: &[String]) -> Result<String> {
    let next = if location.starts_with("https://") {
        location.to_owned()
    } else if location.starts_with('/') && !location.starts_with("//") {
        let rest = current.strip_prefix("https://").unwrap_or(current);
        let host = rest.split('/').next().unwrap_or("");
        format!("https://{host}{location}")
    } else {
        return Err(Error::Invalid(
            "the server redirected to an unsupported address".into(),
        ));
    };
    check_download_url(&next).or_else(|_| check_extra_host(&next, extra_hosts))?;
    Ok(next)
}

pub struct UreqFetcher {
    agent: ureq::Agent,
    extra_hosts: Vec<String>,
}

impl UreqFetcher {
    /// Besides the GitHub hosts, `hosts` (exact names) may be contacted. Used for
    /// optional features such as lyrics lookup.
    pub fn with_extra_hosts(hosts: &[&str]) -> UreqFetcher {
        let mut fetcher = UreqFetcher::new();
        fetcher.extra_hosts = hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
        fetcher
    }

    pub fn new() -> UreqFetcher {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .user_agent(concat!("ghostify/", env!("CARGO_PKG_VERSION")))
            .build();
        UreqFetcher {
            agent: config.into(),
            extra_hosts: Vec::new(),
        }
    }
}

impl Default for UreqFetcher {
    fn default() -> Self {
        UreqFetcher::new()
    }
}

impl Fetcher for UreqFetcher {
    fn get(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let mut current = url.to_owned();
        for _ in 0..=MAX_REDIRECTS {
            check_download_url(&current)
                .or_else(|_| check_index_url(&current))
                .or_else(|_| check_extra_host(&current, &self.extra_hosts))?;
            let mut response = self.agent.get(&current).call().map_err(|e| {
                Error::Io(std::io::Error::other(format!(
                    "request to {current} failed: {e}"
                )))
            })?;
            let status = response.status().as_u16();
            if (300..400).contains(&status) {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| Error::Invalid("redirect without a location".into()))?
                    .to_owned();
                current = resolve_redirect(&current, &location, &self.extra_hosts)?;
                continue;
            }
            if status != 200 {
                return Err(Error::Io(std::io::Error::other(format!(
                    "{current} answered with status {status}"
                ))));
            }
            let mut body = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(max_bytes + 1)
                .read_to_end(&mut body)?;
            if body.len() as u64 > max_bytes {
                return Err(Error::Invalid(format!(
                    "the download is larger than {max_bytes} bytes"
                )));
            }
            return Ok(body);
        }
        Err(Error::Invalid("too many redirects".into()))
    }
}

/// `https://<host>/...` where `<host>` is one of the explicitly allowed names.
pub fn check_extra_host(url: &str, hosts: &[String]) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::Invalid("only https is allowed".into()))?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if !host.contains(['@', ':']) && hosts.contains(&host) {
        Ok(())
    } else {
        Err(Error::Invalid(format!("`{host}` is not an allowed host")))
    }
}

/// The index lives on GitHub Pages (`*.github.io`) or a GitHub host.
pub fn check_index_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| Error::Invalid("the index url must use https".into()))?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let ok = !host.contains(['@', ':'])
        && (host.ends_with(".github.io")
            || crate::index::ALLOWED_DOWNLOAD_HOSTS.contains(&host.as_str()));
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "`{host}` is not an allowed index host"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_stay_on_allowed_https_hosts() {
        let from = "https://github.com/a/b/releases/download/v1/x.zip";
        assert_eq!(
            resolve_redirect(from, "https://objects.githubusercontent.com/x?sig=1", &[]).unwrap(),
            "https://objects.githubusercontent.com/x?sig=1"
        );
        assert_eq!(
            resolve_redirect(from, "/a/b/other.zip", &[]).unwrap(),
            "https://github.com/a/b/other.zip"
        );
        for bad in [
            "http://github.com/x",
            "https://evil.example/x",
            "//evil.example/x",
            "relative/path",
            "",
            "ftp://github.com/x",
        ] {
            assert!(resolve_redirect(from, bad, &[]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn extra_hosts_must_match_exactly() {
        let hosts = vec!["lrclib.net".to_owned()];
        assert!(check_extra_host("https://lrclib.net/api/get?x=1", &hosts).is_ok());
        assert!(check_extra_host("https://LRCLIB.net/api", &hosts).is_ok());
        for bad in [
            "http://lrclib.net/",
            "https://lrclib.net.evil.example/",
            "https://evil.example/lrclib.net",
            "https://a@lrclib.net/",
            "https://lrclib.net:444/",
        ] {
            assert!(check_extra_host(bad, &hosts).is_err(), "{bad}");
        }
        assert!(check_extra_host("https://lrclib.net/", &[]).is_err());
        assert_eq!(
            resolve_redirect("https://lrclib.net/a", "/b", &hosts).unwrap(),
            "https://lrclib.net/b"
        );
        assert!(
            resolve_redirect("https://lrclib.net/a", "https://evil.example/b", &hosts).is_err()
        );
    }

    #[test]
    fn index_urls_may_be_github_pages() {
        assert!(check_index_url("https://ghostify.github.io/registry/index.json").is_ok());
        assert!(check_index_url("https://raw.githubusercontent.com/o/r/main/index.json").is_ok());
        for bad in [
            "http://x.github.io/i.json",
            "https://evil.example/i.json",
            "https://github.io.evil.example/i.json",
            "https://a@x.github.io/i",
        ] {
            assert!(check_index_url(bad).is_err(), "{bad}");
        }
    }
}
