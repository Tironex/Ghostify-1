//! Parser for Spicetify's `color.ini` files.

use ghostify_core::{Error, Result};
use std::collections::BTreeMap;

pub const MAX_INI_BYTES: usize = 256 * 1024;

/// Ordered colour schemes: scheme name -> (key -> `rrggbb`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColorSchemes {
    order: Vec<String>,
    schemes: BTreeMap<String, BTreeMap<String, String>>,
}

impl ColorSchemes {
    pub fn parse(text: &str) -> Result<ColorSchemes> {
        if text.len() > MAX_INI_BYTES {
            return Err(Error::Invalid("color.ini is too large".into()));
        }
        let mut out = ColorSchemes::default();
        let mut current: Option<String> = None;
        for (number, raw) in text.lines().enumerate() {
            let line = raw.trim_start_matches('\u{feff}').trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                let name = name.trim();
                if !valid_scheme_name(name) {
                    return Err(Error::Invalid(format!(
                        "color.ini line {}: bad scheme name `{name}`",
                        number + 1
                    )));
                }
                if !out.schemes.contains_key(name) {
                    out.order.push(name.to_owned());
                    out.schemes.insert(name.to_owned(), BTreeMap::new());
                }
                current = Some(name.to_owned());
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                Error::Invalid(format!(
                    "color.ini line {}: expected key = value",
                    number + 1
                ))
            })?;
            let key = key.trim().to_ascii_lowercase();
            if key.is_empty()
                || !key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(Error::Invalid(format!(
                    "color.ini line {}: bad key `{key}`",
                    number + 1
                )));
            }
            let value = normalize_hex(value.trim()).ok_or_else(|| {
                Error::Invalid(format!(
                    "color.ini line {}: `{}` is not a hex colour",
                    number + 1,
                    value.trim()
                ))
            })?;
            let scheme = current.as_ref().ok_or_else(|| {
                Error::Invalid(format!(
                    "color.ini line {}: value outside of a [scheme]",
                    number + 1
                ))
            })?;
            if let Some(map) = out.schemes.get_mut(scheme) {
                map.insert(key, value);
            }
        }
        if out.order.is_empty() {
            return Err(Error::Invalid("color.ini has no schemes".into()));
        }
        Ok(out)
    }

    pub fn names(&self) -> &[String] {
        &self.order
    }

    pub fn first(&self) -> Option<&str> {
        self.order.first().map(String::as_str)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.schemes.contains_key(name)
    }

    /// `:root{--spice-<key>:#rrggbb;--spice-rgb-<key>:r,g,b;...}` for one scheme.
    pub fn to_css(&self, name: &str) -> Result<String> {
        let map = self
            .schemes
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("color scheme `{name}`")))?;
        let mut css = String::from(":root{");
        for (key, hex) in map {
            let (r, g, b) = rgb(hex);
            css.push_str(&format!(
                "--spice-{key}:#{hex};--spice-rgb-{key}:{r},{g},{b};"
            ));
        }
        css.push('}');
        Ok(css)
    }
}

fn valid_scheme_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// Accepts `rrggbb`, `#rrggbb`, `rgb`, `#rgb`; returns six lowercase hex digits.
fn normalize_hex(value: &str) -> Option<String> {
    let digits = value.strip_prefix('#').unwrap_or(value);
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match digits.len() {
        6 => Some(digits.to_ascii_lowercase()),
        3 => Some(
            digits
                .chars()
                .flat_map(|c| [c, c])
                .collect::<String>()
                .to_ascii_lowercase(),
        ),
        _ => None,
    }
}

fn rgb(hex: &str) -> (u8, u8, u8) {
    let part = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    (part(0), part(2), part(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}; comment\n[Base]\nmain = 121212\ntext = #FFF\n\n[Light]\nmain = ffffff\nText=000000\n";

    #[test]
    fn parses_schemes_in_order() {
        let schemes = ColorSchemes::parse(SAMPLE).unwrap();
        assert_eq!(schemes.names(), ["Base", "Light"]);
        assert_eq!(schemes.first(), Some("Base"));
        assert!(schemes.contains("Light") && !schemes.contains("x"));
    }

    #[test]
    fn generates_hex_and_rgb_variables() {
        let schemes = ColorSchemes::parse(SAMPLE).unwrap();
        assert_eq!(
            schemes.to_css("Base").unwrap(),
            ":root{--spice-main:#121212;--spice-rgb-main:18,18,18;--spice-text:#ffffff;--spice-rgb-text:255,255,255;}"
        );
        assert!(schemes
            .to_css("Light")
            .unwrap()
            .contains("--spice-text:#000000"));
        assert_eq!(schemes.to_css("nope").unwrap_err().code(), "not_found");
    }

    #[test]
    fn rejects_malformed_files() {
        for bad in [
            "",
            "[A]\nmain = zzzzzz",
            "[A]\nmain 121212",
            "main = 121212",
            "[A]\nmain = 12345",
            "[A;b]\nx = 111111",
            "[A]\nma in = 111111",
            "[A]\nx = 111111}body{",
        ] {
            assert!(ColorSchemes::parse(bad).is_err(), "{bad:?}");
        }
        assert!(ColorSchemes::parse(&"a".repeat(MAX_INI_BYTES + 1)).is_err());
    }
}
