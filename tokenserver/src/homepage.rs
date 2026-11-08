// Port of bgutil-ytdlp-pot-provider 2.0.0's session_manager.ts homepage parsing.
// Copyright (c) Brainicism and contributors, GPL-3.0.

use serde_json::Value;

pub const INTERPRETER_HOSTS: &[&str] = &["www.google.com"];

pub struct Challenge {
    pub program: String,
    pub global_name: String,
    pub interpreter_hash: String,
    pub interpreter_url: String,
}

/// The first `ytcfg.set({...})` object on the page.
pub fn parse_ytcfg(html: &str) -> Option<Value> {
    object_after(html, "ytcfg.set(")
}

/// The `bgChallenge` of the first `window.ytAtN({...})` call.
pub fn parse_challenge(html: &str) -> Option<Challenge> {
    let obj = object_after(html, "window.ytAtN(")?;
    challenge_from_json(obj.get("bgChallenge")?)
}

/// Reads the `bgChallenge` shape, which is also what the plugin sends.
pub fn challenge_from_json(v: &Value) -> Option<Challenge> {
    let text = |x: &Value| x.as_str().map(str::to_owned);
    Some(Challenge {
        program: text(v.get("program")?)?,
        global_name: text(v.get("globalName")?)?,
        interpreter_hash: text(v.get("interpreterHash")?)?,
        interpreter_url: text(
            v.get("interpreterUrl")?
                .get("privateDoNotAccessOrElseTrustedResourceUrlWrappedValue")?,
        )?,
    })
}

/// True for `https://HOST/...` or `//HOST/...` with HOST exactly one of INTERPRETER_HOSTS.
pub fn interpreter_allowed(url: &str) -> bool {
    let rest = match url.strip_prefix("https://").or_else(|| url.strip_prefix("//")) {
        Some(r) => r,
        None => return false,
    };
    match rest.split_once('/') {
        Some((host, _)) => INTERPRETER_HOSTS.contains(&host),
        None => false,
    }
}

fn object_after(html: &str, marker: &str) -> Option<Value> {
    let mut from = 0;
    while let Some(i) = html[from..].find(marker) {
        let start = from + i + marker.len();
        if html[start..].starts_with('{') {
            if let Some(len) = balanced_object_len(&html[start..]) {
                if let Ok(v) = serde_json::from_str(&html[start..start + len]) {
                    return Some(v);
                }
            }
        }
        from = start;
    }
    None
}

/// Length of the `{...}` at the start of `s`, skipping braces inside JSON strings.
fn balanced_object_len(s: &str) -> Option<usize> {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (i, b) in s.bytes().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHALLENGE: &str = r#"{"bgChallenge":{"program":"PROG","globalName":"gn","interpreterHash":"h1","interpreterUrl":{"privateDoNotAccessOrElseTrustedResourceUrlWrappedValue":"//www.google.com/js/th/abc.js"}}}"#;

    #[test]
    fn reads_the_first_ytcfg_object_even_with_braces_in_strings() {
        let html = r#"<script>ytcfg.set({"EVENT_ID":"e1","X":"a});b","Q":"\"});"}); ytcfg.set({"EVENT_ID":"e2"});</script>"#;
        let v = parse_ytcfg(html).unwrap();
        assert_eq!(v["EVENT_ID"], "e1");
        assert_eq!(v["X"], "a});b");
    }

    #[test]
    fn reads_the_bg_challenge_from_ytatn() {
        let html = format!("<script>window.ytAtN({CHALLENGE});</script>");
        let c = parse_challenge(&html).unwrap();
        assert_eq!(c.program, "PROG");
        assert_eq!(c.global_name, "gn");
        assert_eq!(c.interpreter_hash, "h1");
        assert_eq!(c.interpreter_url, "//www.google.com/js/th/abc.js");
    }

    #[test]
    fn gives_none_without_ytatn_or_without_bg_challenge() {
        assert!(parse_challenge("<html></html>").is_none());
        assert!(parse_challenge(r#"window.ytAtN({"other":1});"#).is_none());
        assert!(parse_challenge(r#"window.ytAtN({"bgChallenge":{"program":"P"}});"#).is_none());
        assert!(parse_challenge(r#"window.ytAtN({"bgChallenge":"#).is_none());
        assert!(parse_ytcfg("nothing here").is_none());
    }

    #[test]
    fn reads_the_challenge_shape_the_plugin_sends() {
        let v: serde_json::Value = serde_json::from_str(CHALLENGE).unwrap();
        let c = challenge_from_json(&v["bgChallenge"]).unwrap();
        assert_eq!(c.program, "PROG");
        assert!(challenge_from_json(&v).is_none());
    }

    #[test]
    fn takes_interpreter_scripts_only_from_google() {
        assert!(interpreter_allowed("//www.google.com/js/th/abc.js"));
        assert!(interpreter_allowed("https://www.google.com/js/th/abc.js"));
        assert!(!interpreter_allowed("http://www.google.com/js/th/abc.js"));
        assert!(!interpreter_allowed("//www.google.com.evil.example/x.js"));
        assert!(!interpreter_allowed("//evil.example/x.js"));
        assert!(!interpreter_allowed("//www.google.com@evil.example/x.js"));
        assert!(!interpreter_allowed("https://www.google.com:8443/x.js"));
        assert!(!interpreter_allowed("//www.google.com"));
    }
}
