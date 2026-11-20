//! Static scan for plugin code that tries to hide, execute strings or leak data.
//!
//! A scan is a safety net, not a proof of harmlessness. Real isolation comes
//! from the sandbox and the permission checks; the scan catches the obvious
//! and keeps known-bad constructs out of the store.

use ghostify_db::repo::misc::ScanVerdict;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

const MAX_FINDINGS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub rule: String,
    pub severity: Severity,
    pub file: String,
    pub line: usize,
    pub message: String,
}

impl Finding {
    /// Compact form stored with the scan result.
    pub fn summary(&self) -> String {
        format!(
            "{:?}:{}:{}:{}",
            self.severity, self.rule, self.file, self.line
        )
        .to_lowercase()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub verdict: ScanVerdict,
    pub findings: Vec<Finding>,
    pub truncated: bool,
}

struct LineRule {
    id: &'static str,
    severity: Severity,
    message: &'static str,
    pattern: Regex,
}

fn line_rules() -> &'static [LineRule] {
    static RULES: OnceLock<Vec<LineRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |id, severity, message, pattern: &str| LineRule {
            id,
            severity,
            message,
            pattern: Regex::new(pattern).expect("valid scanner pattern"),
        };
        vec![
            rule(
                "eval",
                Severity::Block,
                "runs code from a string (eval)",
                r"\beval\s*\(",
            ),
            rule(
                "new-function",
                Severity::Block,
                "builds code from a string (Function constructor)",
                r#"\bnew\s+Function\s*\(|\bFunction\s*\(\s*['"`]"#,
            ),
            rule(
                "constructor-escape",
                Severity::Block,
                "reaches the Function constructor indirectly",
                r#"\.constructor\s*\(\s*['"`]|\[\s*['"`]constructor['"`]\s*\]\s*\("#,
            ),
            rule(
                "cookie",
                Severity::Block,
                "reads or writes cookies",
                r"document\s*\.\s*cookie",
            ),
            rule(
                "import-scripts",
                Severity::Block,
                "loads scripts at runtime",
                r"\bimportScripts\s*\(",
            ),
            rule(
                "node-modules",
                Severity::Block,
                "uses Node.js system modules",
                r#"require\s*\(\s*['"](?:fs|child_process|net|os|http|https|path)['"]\s*\)"#,
            ),
            rule(
                "miner",
                Severity::Block,
                "mentions crypto-mining software",
                r"(?i)coinhive|cryptonight|stratum\+tcp|xmrig|coin-hive",
            ),
            rule(
                "websocket",
                Severity::Warn,
                "opens a WebSocket directly",
                r"\bnew\s+WebSocket\s*\(",
            ),
            rule(
                "xhr",
                Severity::Warn,
                "uses XMLHttpRequest directly",
                r"\bXMLHttpRequest\b",
            ),
            rule(
                "beacon",
                Severity::Warn,
                "sends data with sendBeacon",
                r"\bsendBeacon\s*\(",
            ),
            rule(
                "script-element",
                Severity::Warn,
                "creates script elements",
                r#"createElement\s*\(\s*['"]script['"]\s*\)"#,
            ),
            rule(
                "token-access",
                Severity::Warn,
                "touches access tokens or authorization headers",
                r"(?i)\baccess_?token\b|\bauthorization\b",
            ),
            rule(
                "document-write",
                Severity::Warn,
                "uses document.write",
                r"document\s*\.\s*write(?:ln)?\s*\(",
            ),
        ]
    })
}

fn obfuscated_name() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b_0x[0-9a-f]{3,6}\b").expect("valid regex"))
}

fn hex_escape() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\\x[0-9a-fA-F]{2}").expect("valid regex"))
}

fn long_base64() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[A-Za-z0-9+/=]{2000,}").expect("valid regex"))
}

fn is_script(path: &str) -> bool {
    path.ends_with(".js") || path.ends_with(".mjs")
}

/// Scans the plugin files `(relative path, content)`. Non-UTF-8 scripts are blocked.
pub fn scan(files: &[(String, Vec<u8>)]) -> ScanReport {
    let mut findings = Vec::new();
    let mut truncated = false;
    let mut push = |finding: Finding, findings: &mut Vec<Finding>| {
        if findings.len() >= MAX_FINDINGS {
            truncated = true;
        } else {
            findings.push(finding);
        }
    };
    for (path, bytes) in files {
        if !is_script(path) {
            continue;
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            push(
                Finding {
                    rule: "binary-script".into(),
                    severity: Severity::Block,
                    file: path.clone(),
                    line: 0,
                    message: "the script is not valid UTF-8".into(),
                },
                &mut findings,
            );
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            for rule in line_rules() {
                if rule.pattern.is_match(line) {
                    push(
                        Finding {
                            rule: rule.id.into(),
                            severity: rule.severity,
                            file: path.clone(),
                            line: index + 1,
                            message: rule.message.into(),
                        },
                        &mut findings,
                    );
                }
            }
        }
        let names = obfuscated_name().find_iter(text).count();
        if names >= 10 {
            push(
                Finding {
                    rule: "obfuscated".into(),
                    severity: Severity::Warn,
                    file: path.clone(),
                    line: 0,
                    message: format!("{names} machine-generated identifiers (obfuscated code)"),
                },
                &mut findings,
            );
        }
        let escapes = hex_escape().find_iter(text).count();
        if escapes >= 50 {
            push(
                Finding {
                    rule: "hex-escapes".into(),
                    severity: Severity::Warn,
                    file: path.clone(),
                    line: 0,
                    message: format!("{escapes} hex escapes in strings"),
                },
                &mut findings,
            );
        }
        if long_base64().is_match(text) {
            push(
                Finding {
                    rule: "embedded-blob".into(),
                    severity: Severity::Warn,
                    file: path.clone(),
                    line: 0,
                    message: "contains a very long encoded blob".into(),
                },
                &mut findings,
            );
        }
    }
    let verdict = match findings.iter().map(|f| f.severity).max() {
        Some(Severity::Block) => ScanVerdict::Blocked,
        Some(Severity::Warn) => ScanVerdict::Suspicious,
        _ => ScanVerdict::Clean,
    };
    ScanReport {
        verdict,
        findings,
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_js(code: &str) -> ScanReport {
        scan(&[("index.js".to_owned(), code.as_bytes().to_vec())])
    }

    fn rules(report: &ScanReport) -> Vec<&str> {
        report.findings.iter().map(|f| f.rule.as_str()).collect()
    }

    #[test]
    fn clean_code_passes() {
        let report = scan_js("const a = [1,2].map(x => x * 2);\nconsole.log('hi', a);\n// evaluate the result\nfunction evaluate() {}");
        assert_eq!(report.verdict, ScanVerdict::Clean);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn blocking_constructs_are_found_with_line_numbers() {
        let code = "ok();\nconst x = eval ('1');\nnew Function('return 1')();\n(0).constructor.constructor('return this')();\nx['constructor']('a');\ndocument.cookie='a';\nimportScripts('x');\nrequire('child_process');\nvar m='XMRig';";
        let report = scan_js(code);
        assert_eq!(report.verdict, ScanVerdict::Blocked);
        let found: Vec<(&str, usize)> = report
            .findings
            .iter()
            .map(|f| (f.rule.as_str(), f.line))
            .collect();
        for expected in [
            ("eval", 2),
            ("new-function", 3),
            ("constructor-escape", 4),
            ("constructor-escape", 5),
            ("cookie", 6),
            ("import-scripts", 7),
            ("node-modules", 8),
            ("miner", 9),
        ] {
            assert!(found.contains(&expected), "{expected:?} in {found:?}");
        }
    }

    #[test]
    fn identifiers_that_only_contain_the_words_are_fine() {
        let report = scan_js(
            "evaluate(); myeval(x); const retrieval = 1; object.eval_count = 2; Functional(1);",
        );
        assert_eq!(report.verdict, ScanVerdict::Clean, "{:?}", rules(&report));
    }

    #[test]
    fn warnings_make_a_plugin_suspicious() {
        for (code, rule) in [
            ("new WebSocket('wss://x')", "websocket"),
            ("new XMLHttpRequest()", "xhr"),
            ("navigator.sendBeacon(u, d)", "beacon"),
            ("document.createElement('script')", "script-element"),
            ("headers.Authorization = t", "token-access"),
            ("const t = data.access_token", "token-access"),
            ("document.write('x')", "document-write"),
        ] {
            let report = scan_js(code);
            assert_eq!(report.verdict, ScanVerdict::Suspicious, "{code}");
            assert_eq!(rules(&report), [rule]);
        }
    }

    #[test]
    fn obfuscation_and_blobs_are_flagged() {
        let obf = (0..12)
            .map(|i| format!("var _0x{i:04x}=1;"))
            .collect::<String>();
        assert_eq!(rules(&scan_js(&obf)), ["obfuscated"]);
        let esc = "\\x41".repeat(60);
        assert_eq!(rules(&scan_js(&format!("var s='{esc}';"))), ["hex-escapes"]);
        let blob = "A".repeat(2500);
        assert_eq!(
            rules(&scan_js(&format!("var s='{blob}';"))),
            ["embedded-blob"]
        );
        assert_eq!(
            scan_js(&"var _0xabc1=1;".repeat(9)).verdict,
            ScanVerdict::Clean
        );
    }

    #[test]
    fn only_scripts_are_scanned_and_binary_scripts_are_blocked() {
        let report = scan(&[
            ("style.css".into(), b"eval(".to_vec()),
            ("data.json".into(), b"document.cookie".to_vec()),
        ]);
        assert_eq!(report.verdict, ScanVerdict::Clean);
        let report = scan(&[("a.js".into(), vec![0xff, 0xfe, 0x00])]);
        assert_eq!(report.verdict, ScanVerdict::Blocked);
        assert_eq!(report.findings[0].rule, "binary-script");
        let report = scan(&[("lib/a.mjs".into(), b"eval(1)".to_vec())]);
        assert_eq!(report.verdict, ScanVerdict::Blocked);
    }

    #[test]
    fn strings_cannot_hide_code_from_the_scan() {
        // A "comment" marker inside a string must not hide what follows on the line.
        let report = scan_js("var u = 'http://x'; eval(payload);");
        assert_eq!(report.verdict, ScanVerdict::Blocked);
    }

    #[test]
    fn findings_are_capped_and_summarised() {
        let code = "eval(1);\n".repeat(500);
        let report = scan_js(&code);
        assert_eq!(report.findings.len(), 200);
        assert!(report.truncated);
        assert_eq!(report.findings[0].summary(), "block:eval:index.js:1");
    }
}
