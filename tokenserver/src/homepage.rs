// Port of bgutil-ytdlp-pot-provider 2.0.0's session_manager.ts homepage parsing.
// Copyright (c) Brainicism and contributors, GPL-3.0.
// parse_loose_json and its helpers port bgutils-js's parseLooseJSON: Copyright (c) 2024 LuanRT, MIT License.

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

/// The `bgChallenge` of the first `window.ytAtN({...})` call. Read first as 2.0.0 reads
/// it (loose JSON, the challenge under `R`), then as strict JSON.
pub fn parse_challenge(html: &str) -> Option<Challenge> {
    ytatn_payload(html)
        .and_then(parse_loose_json)
        .and_then(|v| bg_challenge_in(&v))
        .or_else(|| bg_challenge_in(&object_after(html, "window.ytAtN(")?))
}

fn bg_challenge_in(v: &Value) -> Option<Challenge> {
    let bg = v.get("R").and_then(|r| r.get("bgChallenge"));
    challenge_from_json(bg.or_else(|| v.get("bgChallenge"))?)
}

/// Reads the `bgChallenge` shape, which is also what the plugin sends. `interpreterHash`
/// may be missing: 2.0.0 never reads it.
pub fn challenge_from_json(v: &Value) -> Option<Challenge> {
    let text = |x: &Value| x.as_str().map(str::to_owned);
    Some(Challenge {
        program: text(v.get("program")?)?,
        global_name: text(v.get("globalName")?)?,
        interpreter_hash: v.get("interpreterHash").and_then(text).unwrap_or_default(),
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

/// The payload 2.0.0's `/window\.ytAtN\(\s*({[\s\S]*?})\s*\)/` captures: from the `{` to
/// the first `}` that only whitespace separates from `)`.
pub(crate) fn ytatn_payload(html: &str) -> Option<&str> {
    const MARKER: &str = "window.ytAtN(";
    let mut from = 0;
    while let Some(i) = html[from..].find(MARKER) {
        from += i + MARKER.len();
        let body = html[from..].trim_start();
        if body.starts_with('{') {
            let mut search = 1;
            while let Some(j) = body[search..].find('}') {
                let end = search + j;
                if body[end + 1..].trim_start().starts_with(')') {
                    return Some(&body[..=end]);
                }
                search = end + 1;
            }
        }
    }
    None
}

/// Port of bgutils-js's `parseLooseJSON`, step for step, so the ytAtN payload reads as it does
/// in 2.0.0. Like the original, the rewrites also apply inside strings.
pub(crate) fn parse_loose_json(s: &str) -> Option<Value> {
    let s = quote_bare_keys(&requote_single_quoted(&drop_trailing_commas(&unescape_hex(s))));
    let mut v: Value = serde_json::from_str(&s).ok()?;
    if let Value::Object(map) = &mut v {
        for val in map.values_mut() {
            if let Value::String(text) = val {
                let t = text.trim_start();
                if t.starts_with('{') || t.starts_with('[') {
                    if let Ok(parsed) = serde_json::from_str(text) {
                        *val = parsed;
                    }
                }
            }
        }
    }
    Some(v)
}

/// `\xHH` becomes the character U+00HH.
fn unescape_hex(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |k: usize| b.get(k).is_some_and(u8::is_ascii_hexdigit);
        if b[i] == b'\\' && b.get(i + 1) == Some(&b'x') && hex(i + 2) && hex(i + 3) {
            let code = (hex_val(b[i + 2]) << 4) | hex_val(b[i + 3]);
            let mut buf = [0; 4];
            out.extend_from_slice(char::from(code).encode_utf8(&mut buf).as_bytes());
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(d: u8) -> u8 {
    match d {
        b'0'..=b'9' => d - b'0',
        b'a'..=b'f' => d - b'a' + 10,
        _ => d - b'A' + 10,
    }
}

/// `,\s*([\]}])` becomes `$1`.
fn drop_trailing_commas(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(',') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let next = after.trim_start();
        if next.starts_with(']') || next.starts_with('}') {
            rest = next;
        } else {
            out.push(',');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `'...'` (backslash escapes allowed) becomes a JSON string of its contents, `\'` read as `'`.
fn requote_single_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\'') {
        out.push_str(&rest[..i]);
        let body = &rest[i + 1..];
        match closing_quote(body) {
            Some(end) => {
                let inner = body[..end].replace("\\'", "'");
                out.push_str(&Value::String(inner).to_string());
                rest = &body[end + 1..];
            }
            None => {
                out.push('\'');
                rest = body;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Byte index of the first `'` that is not the second half of a backslash escape.
fn closing_quote(body: &str) -> Option<usize> {
    let b = body.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\'' => return Some(i),
            // A multi-byte escaped character leaves continuation bytes, which are never ' or \.
            b'\\' if i + 1 < b.len() => i += 2,
            b'\\' => return None,
            _ => i += 1,
        }
    }
    None
}

/// `([{,]\s*)([a-zA-Z0-9_$]+)\s*:` becomes `$1"$2":`.
fn quote_bare_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['{', ',']) {
        out.push_str(&rest[..=i]);
        let after = &rest[i + 1..];
        let key_at = &after[after.len() - after.trim_start().len()..];
        let key_len = key_at
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'$')
            .count();
        let tail = key_at[key_len..].trim_start();
        if key_len > 0 && tail.starts_with(':') {
            out.push_str(&after[..after.len() - key_at.len()]);
            out.push('"');
            out.push_str(&key_at[..key_len]);
            out.push_str("\":");
            rest = &tail[1..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
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

    const BG: &str = r#"{"program":"PROG","globalName":"gn","interpreterHash":"h1","interpreterUrl":{"privateDoNotAccessOrElseTrustedResourceUrlWrappedValue":"//www.google.com/js/th/abc.js"}}"#;

    #[test]
    fn reads_the_challenge_under_r_as_2_0_0_does() {
        let html = format!(r#"<script>window.ytAtN( {{"R":{{"bgChallenge":{BG}}}}} );</script>"#);
        let c = parse_challenge(&html).unwrap();
        assert_eq!(c.program, "PROG");
        assert_eq!(c.global_name, "gn");
        assert_eq!(c.interpreter_url, "//www.google.com/js/th/abc.js");
    }

    #[test]
    fn reads_a_loose_ytatn_payload_with_hex_escapes_single_quotes_and_bare_keys() {
        let inner = format!(r#"{{"bgChallenge":{BG}}}"#).replace('"', r"\x22");
        let html = format!("<script>window.ytAtN({{R:'{inner}', n: 'it\\'s', }});</script>");
        let c = parse_challenge(&html).unwrap();
        assert_eq!(c.program, "PROG");
        assert_eq!(c.interpreter_hash, "h1");
    }

    #[test]
    fn parses_loose_json_like_bgutils() {
        let v = parse_loose_json(r#"{a: 'x\'y', "b": [1, 2,], c: '\x7b"d":1}', }"#).unwrap();
        assert_eq!(v["a"], "x'y");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(v["c"]["d"], 1);
        assert!(parse_loose_json("{a: }").is_none());
    }

    #[test]
    fn a_challenge_without_interpreter_hash_still_counts() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"program":"P","globalName":"g","interpreterUrl":{"privateDoNotAccessOrElseTrustedResourceUrlWrappedValue":"//www.google.com/x.js"}}"#,
        )
        .unwrap();
        let c = challenge_from_json(&v).unwrap();
        assert_eq!(c.interpreter_hash, "");
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
