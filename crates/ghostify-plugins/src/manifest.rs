//! `plugin.json`: what a plugin is and which permissions it asks for.

use ghostify_core::{Error, Result};
use ghostify_db::repo::plugins::PluginRuntime;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MANIFEST_FILE: &str = "plugin.json";
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_ENTRY_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_HOSTS: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "player.read")]
    PlayerRead,
    #[serde(rename = "player.control")]
    PlayerControl,
    #[serde(rename = "playlist.read")]
    PlaylistRead,
    #[serde(rename = "playlist.write")]
    PlaylistWrite,
    #[serde(rename = "storage")]
    Storage,
    /// Outgoing HTTP(S) requests to the hosts listed in the manifest.
    #[serde(rename = "network")]
    Network,
    #[serde(rename = "ui")]
    Ui,
    #[serde(rename = "notifications")]
    Notifications,
    #[serde(rename = "clipboard")]
    Clipboard,
}

impl Permission {
    pub const ALL: [Permission; 9] = [
        Permission::PlayerRead,
        Permission::PlayerControl,
        Permission::PlaylistRead,
        Permission::PlaylistWrite,
        Permission::Storage,
        Permission::Network,
        Permission::Ui,
        Permission::Notifications,
        Permission::Clipboard,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Permission::PlayerRead => "player.read",
            Permission::PlayerControl => "player.control",
            Permission::PlaylistRead => "playlist.read",
            Permission::PlaylistWrite => "playlist.write",
            Permission::Storage => "storage",
            Permission::Network => "network",
            Permission::Ui => "ui",
            Permission::Notifications => "notifications",
            Permission::Clipboard => "clipboard",
        }
    }

    pub fn parse(text: &str) -> Result<Permission> {
        Permission::ALL
            .into_iter()
            .find(|p| p.as_str() == text)
            .ok_or_else(|| Error::Invalid(format!("unknown permission `{text}`")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// `name` or `author/name`; lowercase letters, digits, `.`, `_`, `-`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default = "default_runtime")]
    pub runtime: PluginRuntime,
    #[serde(default)]
    pub permissions: BTreeSet<Permission>,
    /// Hosts the plugin may contact, `example.com` or `*.example.com`.
    #[serde(default)]
    pub hosts: Vec<String>,
}

fn default_entry() -> String {
    "index.js".into()
}

fn default_runtime() -> PluginRuntime {
    PluginRuntime::Sandboxed
}

impl PluginManifest {
    pub fn parse(bytes: &[u8]) -> Result<PluginManifest> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Invalid("plugin.json is too large".into()));
        }
        let manifest: PluginManifest = serde_json::from_slice(bytes)
            .map_err(|e| Error::Invalid(format!("plugin.json is invalid: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<()> {
        validate_id(&self.id)?;
        for (label, value, max) in [
            ("name", &self.name, 80),
            ("version", &self.version, 32),
            ("author", &self.author, 80),
            ("description", &self.description, 500),
        ] {
            if value.len() > max || value.chars().any(char::is_control) {
                return Err(Error::Invalid(format!(
                    "plugin {label} is too long or contains control characters"
                )));
            }
        }
        if self.name.trim().is_empty() || self.version.trim().is_empty() {
            return Err(Error::Invalid(
                "plugin name and version must not be empty".into(),
            ));
        }
        validate_relative(&self.entry)?;
        if !self.entry.ends_with(".js") && !self.entry.ends_with(".mjs") {
            return Err(Error::Invalid("the entry must be a .js file".into()));
        }
        if self.hosts.len() > MAX_HOSTS {
            return Err(Error::Invalid(format!(
                "at most {MAX_HOSTS} hosts may be listed"
            )));
        }
        for host in &self.hosts {
            validate_host(host)?;
        }
        let network = self.permissions.contains(&Permission::Network);
        if network && self.hosts.is_empty() {
            return Err(Error::Invalid(
                "the network permission needs at least one host".into(),
            ));
        }
        if !network && !self.hosts.is_empty() {
            return Err(Error::Invalid(
                "hosts are listed but the network permission is not requested".into(),
            ));
        }
        Ok(())
    }

    /// Does the manifest allow requests to `host`?
    pub fn allows_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.hosts
            .iter()
            .any(|allowed| match allowed.strip_prefix("*.") {
                Some(suffix) => {
                    host.len() > suffix.len() + 1
                        && host.ends_with(suffix)
                        && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
                }
                None => *allowed == host,
            })
    }

    pub fn dir_name(&self) -> String {
        self.id.replace('/', "__")
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    let part_ok = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && part.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
    };
    let parts: Vec<&str> = id.split('/').collect();
    if id.len() > 64 || parts.len() > 2 || !parts.iter().all(|p| part_ok(p)) {
        return Err(Error::Invalid(format!("`{id}` is not a valid plugin id")));
    }
    Ok(())
}

fn validate_relative(path: &str) -> Result<()> {
    let bad = path.is_empty()
        || path.len() > 200
        || path.starts_with('/')
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == ".." || c.starts_with('.'));
    if bad {
        return Err(Error::Invalid(format!(
            "`{path}` is not a safe relative path"
        )));
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<()> {
    let name = host.strip_prefix("*.").unwrap_or(host);
    let labels: Vec<&str> = name.split('.').collect();
    let ok = host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && !labels
            .last()
            .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()));
    if !ok {
        return Err(Error::Invalid(format!("`{host}` is not a valid host (lowercase domain names only, no scheme, port, path or IP address)")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(extra: &str) -> Result<PluginManifest> {
        PluginManifest::parse(
            format!(r#"{{"id":"me/p","name":"P","version":"1.0.0"{extra}}}"#).as_bytes(),
        )
    }

    #[test]
    fn defaults_and_permissions() {
        let m = manifest("").unwrap();
        assert_eq!(m.entry, "index.js");
        assert_eq!(m.runtime, PluginRuntime::Sandboxed);
        assert!(m.permissions.is_empty());
        let m = manifest(r#","permissions":["storage","player.read","storage"]"#).unwrap();
        assert_eq!(m.permissions.len(), 2);
        assert!(manifest(r#","permissions":["root"]"#).is_err());
        for p in Permission::ALL {
            assert_eq!(Permission::parse(p.as_str()).unwrap(), p);
            assert_eq!(
                serde_json::to_string(&p).unwrap(),
                format!("\"{}\"", p.as_str())
            );
        }
        assert!(Permission::parse("nope").is_err());
    }

    #[test]
    fn network_and_hosts_go_together() {
        assert!(manifest(r#","permissions":["network"]"#).is_err());
        assert!(manifest(r#","hosts":["example.com"]"#).is_err());
        let m = manifest(r#","permissions":["network"],"hosts":["api.example.com","*.cdn.org"]"#)
            .unwrap();
        assert!(m.allows_host("api.example.com") && m.allows_host("API.EXAMPLE.COM"));
        assert!(m.allows_host("a.cdn.org") && m.allows_host("a.b.cdn.org"));
        assert!(
            !m.allows_host("cdn.org")
                && !m.allows_host("evilcdn.org")
                && !m.allows_host("example.com")
                && !m.allows_host("api.example.com.evil.net")
        );
    }

    #[test]
    fn invalid_hosts_are_rejected() {
        for host in [
            "http://a.com",
            "a.com:80",
            "a.com/x",
            "localhost",
            "127.0.0.1",
            "10.0.0.1",
            "*.com.",
            "*",
            "A.com",
            "-a.com",
            "a..com",
            "",
        ] {
            let json = format!(r#","permissions":["network"],"hosts":["{host}"]"#);
            assert!(manifest(&json).is_err(), "{host:?}");
        }
    }

    #[test]
    fn entry_and_id_are_validated() {
        for entry in ["../x.js", "/x.js", "x.txt", "a\\b.js", ".h.js", "a//b.js"] {
            assert!(
                manifest(&format!(r#","entry":"{entry}""#)).is_err(),
                "{entry}"
            );
        }
        assert_eq!(
            manifest(r#","entry":"dist/main.js""#).unwrap().entry,
            "dist/main.js"
        );
        assert!(PluginManifest::parse(br#"{"id":"A","name":"n","version":"1"}"#).is_err());
        assert!(PluginManifest::parse(br#"{"id":"a/b/c","name":"n","version":"1"}"#).is_err());
        assert!(PluginManifest::parse(b"{").is_err());
        assert!(PluginManifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
        assert_eq!(
            manifest(r#","runtime":"legacy""#).unwrap().runtime,
            PluginRuntime::Legacy
        );
        assert_eq!(manifest("").unwrap().dir_name(), "me__p");
    }
}
