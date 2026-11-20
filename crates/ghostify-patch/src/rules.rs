//! Patch rules: data that describes how Spotify's UI bundle is modified.
//!
//! Rules are data, not code, so a fix for a new Spotify release ships as a
//! small signed file instead of a new application release.

use ghostify_core::{Error, Result};
use ghostify_spotify::{SpotifyVersion, VersionRange};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// What a rule does to a text file inside `xpui.spa`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Inserts `content` in front of the first occurrence of `marker`.
    InsertBefore { marker: String, content: String },
    /// Inserts `content` right after the first occurrence of `marker`.
    InsertAfter { marker: String, content: String },
    /// Replaces every occurrence of `find`.
    ReplaceLiteral { find: String, replace: String },
    /// Regular expression replacement; `$1` style group references work.
    ReplaceRegex {
        pattern: String,
        replace: String,
        #[serde(default)]
        all: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    /// Entry name inside `xpui.spa`, for example `index.html`.
    pub file: String,
    /// A required rule that does not match aborts the whole patch.
    #[serde(default = "default_required")]
    pub required: bool,
    pub action: Action,
}

fn default_required() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    /// Human readable version, for example `2026.11.14`.
    pub version: String,
    /// Monotonic counter. A signed rule set with a lower sequence than the one
    /// applied before is rejected, which prevents downgrade attacks.
    pub sequence: u64,
    /// Spotify versions this rule set was written for.
    #[serde(default)]
    pub spotify_versions: VersionRange,
    pub rules: Vec<Rule>,
}

impl Rule {
    /// Applies the rule. `Ok(None)` means the rule did not match.
    pub fn apply(&self, text: &str) -> Result<Option<String>> {
        Ok(match &self.action {
            Action::InsertBefore { marker, content } => text.find(marker.as_str()).map(|at| {
                let mut out = String::with_capacity(text.len() + content.len());
                out.push_str(&text[..at]);
                out.push_str(content);
                out.push_str(&text[at..]);
                out
            }),
            Action::InsertAfter { marker, content } => text.find(marker.as_str()).map(|at| {
                let end = at + marker.len();
                let mut out = String::with_capacity(text.len() + content.len());
                out.push_str(&text[..end]);
                out.push_str(content);
                out.push_str(&text[end..]);
                out
            }),
            Action::ReplaceLiteral { find, replace } => text
                .contains(find.as_str())
                .then(|| text.replace(find.as_str(), replace)),
            Action::ReplaceRegex {
                pattern,
                replace,
                all,
            } => {
                let regex = RegexBuilder::new(pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .build()
                    .map_err(|e| Error::Invalid(format!("rule `{}`: bad pattern: {e}", self.id)))?;
                if !regex.is_match(text) {
                    None
                } else if *all {
                    Some(regex.replace_all(text, replace.as_str()).into_owned())
                } else {
                    Some(regex.replace(text, replace.as_str()).into_owned())
                }
            }
        })
    }
}

impl RuleSet {
    /// Checks structure: unique ids, safe entry names, compilable patterns.
    pub fn validate(&self) -> Result<()> {
        if self.version.trim().is_empty() {
            return Err(Error::Invalid("rule set version must not be empty".into()));
        }
        if self.rules.is_empty() {
            return Err(Error::Invalid("rule set contains no rules".into()));
        }
        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if rule.id.trim().is_empty() || !ids.insert(rule.id.as_str()) {
                return Err(Error::Invalid(format!(
                    "rule id `{}` is empty or used twice",
                    rule.id
                )));
            }
            validate_entry_name(&rule.file)
                .map_err(|e| Error::Invalid(format!("rule `{}`: {e}", rule.id)))?;
            match &rule.action {
                Action::InsertBefore { marker, .. } | Action::InsertAfter { marker, .. }
                    if marker.is_empty() =>
                {
                    return Err(Error::Invalid(format!("rule `{}`: empty marker", rule.id)));
                }
                Action::ReplaceLiteral { find, .. } if find.is_empty() => {
                    return Err(Error::Invalid(format!(
                        "rule `{}`: empty search text",
                        rule.id
                    )));
                }
                Action::ReplaceRegex { pattern, .. } => {
                    RegexBuilder::new(pattern)
                        .size_limit(REGEX_SIZE_LIMIT)
                        .build()
                        .map_err(|e| {
                            Error::Invalid(format!("rule `{}`: bad pattern: {e}", rule.id))
                        })?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Whether the rule set is meant for this Spotify version. An unknown
    /// version is only accepted by a rule set without version bounds.
    pub fn applies_to(&self, version: Option<&SpotifyVersion>) -> bool {
        match version {
            Some(version) => self.spotify_versions.contains(version),
            None => self.spotify_versions.is_unbounded(),
        }
    }
}

/// Entry names must stay inside the archive.
pub(crate) fn validate_entry_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("empty file name".into());
    }
    if name.starts_with('/') || name.contains('\\') || name.contains('\0') {
        return Err(format!("unsafe file name `{name}`"));
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("unsafe file name `{name}`"));
    }
    Ok(())
}

/// Minimal rule set that every Ghostify build ships with: it only adds the
/// Ghostify loader to `index.html`. Rules for deeper integration come as
/// signed rule sets.
pub fn builtin() -> RuleSet {
    RuleSet {
        version: "builtin-2".into(),
        sequence: 0,
        spotify_versions: VersionRange::default(),
        rules: vec![Rule {
            id: "inject-loader".into(),
            file: "index.html".into(),
            required: true,
            action: Action::InsertBefore {
                marker: "</head>".into(),
                content: "<link rel=\"stylesheet\" href=\"ghostify/loader.css\"><script defer src=\"ghostify/config.js\"></script><script defer src=\"ghostify/loader.js\"></script>".into(),
            },
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(action: Action) -> Rule {
        Rule {
            id: "r".into(),
            file: "index.html".into(),
            required: true,
            action,
        }
    }

    #[test]
    fn insert_before_and_after_use_the_first_marker() {
        let before = rule(Action::InsertBefore {
            marker: "<b>".into(),
            content: "X".into(),
        });
        assert_eq!(before.apply("a<b>c<b>d").unwrap().unwrap(), "aX<b>c<b>d");
        let after = rule(Action::InsertAfter {
            marker: "<b>".into(),
            content: "X".into(),
        });
        assert_eq!(after.apply("a<b>c<b>d").unwrap().unwrap(), "a<b>Xc<b>d");
        assert_eq!(after.apply("nothing").unwrap(), None);
    }

    #[test]
    fn literal_replacement_replaces_all_occurrences() {
        let r = rule(Action::ReplaceLiteral {
            find: "foo".into(),
            replace: "bar".into(),
        });
        assert_eq!(r.apply("foo foo").unwrap().unwrap(), "bar bar");
        assert_eq!(r.apply("baz").unwrap(), None);
    }

    #[test]
    fn regex_replacement_supports_groups_and_all_flag() {
        let first = rule(Action::ReplaceRegex {
            pattern: r"v(\d+)".into(),
            replace: "version-$1".into(),
            all: false,
        });
        assert_eq!(first.apply("v1 v2").unwrap().unwrap(), "version-1 v2");
        let all = rule(Action::ReplaceRegex {
            pattern: r"v(\d+)".into(),
            replace: "version-$1".into(),
            all: true,
        });
        assert_eq!(all.apply("v1 v2").unwrap().unwrap(), "version-1 version-2");
        assert_eq!(all.apply("none").unwrap(), None);
        let broken = rule(Action::ReplaceRegex {
            pattern: "(".into(),
            replace: "".into(),
            all: false,
        });
        assert_eq!(broken.apply("x").unwrap_err().code(), "invalid_input");
    }

    #[test]
    fn builtin_rule_set_is_valid_and_works_on_a_head_element() {
        let set = builtin();
        set.validate().unwrap();
        let patched = set.rules[0]
            .apply("<html><head><title>x</title></head><body></body></html>")
            .unwrap()
            .unwrap();
        assert!(patched.contains("ghostify/loader.js\"></script></head>"));
    }

    #[test]
    fn validation_rejects_bad_rule_sets() {
        let mut set = builtin();
        set.rules.push(set.rules[0].clone());
        assert!(set.validate().is_err(), "duplicate ids");

        for file in ["", "/abs", "../up", "a/../b", "a\\b", "a//b"] {
            let mut set = builtin();
            set.rules[0].file = file.into();
            assert!(set.validate().is_err(), "file `{file}`");
        }

        let mut set = builtin();
        set.rules[0].action = Action::InsertBefore {
            marker: "".into(),
            content: "x".into(),
        };
        assert!(set.validate().is_err());

        let mut set = builtin();
        set.rules[0].action = Action::ReplaceRegex {
            pattern: "[".into(),
            replace: "".into(),
            all: false,
        };
        assert!(set.validate().is_err());

        let mut set = builtin();
        set.rules.clear();
        assert!(set.validate().is_err());
    }

    #[test]
    fn applicability_follows_version_bounds() {
        let v = |s: &str| SpotifyVersion::parse(s).unwrap();
        let mut set = builtin();
        assert!(set.applies_to(None));
        assert!(set.applies_to(Some(&v("1.2.3"))));
        set.spotify_versions = VersionRange {
            min: Some(v("1.2.30")),
            max: Some(v("1.2.40")),
        };
        assert!(
            !set.applies_to(None),
            "unknown version needs an unbounded rule set"
        );
        assert!(set.applies_to(Some(&v("1.2.35.1"))));
        assert!(!set.applies_to(Some(&v("1.2.50"))));
    }

    #[test]
    fn json_format_is_stable() {
        let json = serde_json::to_string(&builtin()).unwrap();
        assert!(json.contains(r#""type":"insert_before""#));
        let back: RuleSet = serde_json::from_str(&json).unwrap();
        assert_eq!(back, builtin());
        let optional: Rule = serde_json::from_str(
            r#"{"id":"x","file":"a.js","action":{"type":"replace_literal","find":"a","replace":"b"}}"#,
        )
        .unwrap();
        assert!(
            optional.required,
            "rules are required unless stated otherwise"
        );
    }
}
