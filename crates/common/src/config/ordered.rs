//! A JSON value that keeps object keys in file order, read and written like `JSON.parse` and
//! `JSON.stringify(v, null, 2)`. Not serde_json's `preserve_order`: features unify across the binary.

use std::fmt::{self, Write};

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Every number is an f64, as in JavaScript: `20.0` and `2e1` are both `20`.
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// A key that appears twice keeps its first place and its last value, as with `JSON.parse`.
    pub fn parse(text: &str) -> Result<Value, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(o) => o.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Sets `key` on an object: in place when it exists, else at the end.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Object(o) = self {
            assign(o, vec![(key.to_string(), value)]);
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        match self {
            Value::Object(o) => o.iter().position(|(k, _)| k == key).map(|i| o.remove(i).1),
            _ => None,
        }
    }

    /// `JSON.stringify(value, null, 2)`, with no trailing newline.
    pub fn to_pretty(&self) -> String {
        let mut out = String::new();
        write_pretty(self, 0, &mut out);
        out
    }
}

/// `Object.assign(target, entries)`: a key already in `target` keeps its place and takes the new value.
pub(crate) fn assign(target: &mut Vec<(String, Value)>, entries: Vec<(String, Value)>) {
    for (key, value) in entries {
        match target.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => target.push((key, value)),
        }
    }
}

impl From<&serde_json::Value> for Value {
    fn from(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
            serde_json::Value::String(s) => Value::String(s.clone()),
            serde_json::Value::Array(a) => Value::Array(a.iter().map(Value::from).collect()),
            serde_json::Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), Value::from(v))).collect()),
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        d.deserialize_any(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, b: bool) -> Result<Value, E> {
        Ok(Value::Bool(b))
    }

    fn visit_i64<E>(self, n: i64) -> Result<Value, E> {
        Ok(Value::Number(n as f64))
    }

    fn visit_u64<E>(self, n: u64) -> Result<Value, E> {
        Ok(Value::Number(n as f64))
    }

    fn visit_f64<E>(self, n: f64) -> Result<Value, E> {
        Ok(Value::Number(n))
    }

    fn visit_str<E>(self, s: &str) -> Result<Value, E> {
        Ok(Value::String(s.to_string()))
    }

    fn visit_string<E>(self, s: String) -> Result<Value, E> {
        Ok(Value::String(s))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = vec![];
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries = vec![];
        while let Some(entry) = map.next_entry::<String, Value>()? {
            assign(&mut entries, vec![entry]);
        }
        Ok(Value::Object(entries))
    }
}

fn write_pretty(v: &Value, indent: usize, out: &mut String) {
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n(' ', depth));
    };
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&js_number(*n)),
        Value::String(s) => quote(s, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent + 2);
                write_pretty(item, indent + 2, out);
            }
            newline(out, indent);
            out.push(']');
        }
        Value::Object(entries) if entries.is_empty() => out.push_str("{}"),
        Value::Object(entries) => {
            out.push('{');
            for (i, (key, value)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                newline(out, indent + 2);
                quote(key, out);
                out.push_str(": ");
                write_pretty(value, indent + 2, out);
            }
            newline(out, indent);
            out.push('}');
        }
    }
}

/// `JSON.stringify` of a string: only the quote, the backslash and control characters are escaped.
fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `Number.prototype.toString`, which `JSON.stringify` uses: the shortest digits that read back
/// as the same f64, written out in full from 1e-6 up to 1e21 and with an exponent outside that.
pub(crate) fn js_number(x: f64) -> String {
    if !x.is_finite() {
        return "null".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("{:e} always writes an exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    // The decimal point sits after the first n digits.
    let n = exp.parse::<i32>().expect("{:e} writes a whole exponent") + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat(-n as usize))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{sign}{}", e.abs())
    };
    if x < 0.0 {
        format!("-{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_the_way_javascript_writes_them() {
        // Arrange
        let cases = [
            (20.0, "20"),
            (12.5, "12.5"),
            (0.5, "0.5"),
            (-3.0, "-3"),
            (-0.0, "0"),
            (21_600_000.0, "21600000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (1.5e300, "1.5e+300"),
            (123.456, "123.456"),
            (0.1 + 0.2, "0.30000000000000004"),
        ];
        for (x, expected) in cases {
            // Act
            let written = js_number(x);
            // Assert
            assert_eq!(written, expected, "{x}");
        }
    }

    #[test]
    fn a_repeated_key_keeps_its_first_place_and_its_last_value() {
        // Act
        let v = Value::parse(r#"{"a":1,"b":2,"a":3}"#).unwrap();

        // Assert
        assert_eq!(v, Value::Object(vec![("a".into(), Value::Number(3.0)), ("b".into(), Value::Number(2.0))]));
    }

    #[test]
    fn pretty_output_matches_json_stringify_with_two_spaces() {
        // Arrange
        let v = Value::parse(r#"{"o":{},"a":[],"n":[1,{"x":null}],"s":"tab\t\u0001 ø \"q\""}"#).unwrap();

        // Act
        let text = v.to_pretty();

        // Assert
        assert_eq!(text, "{\n  \"o\": {},\n  \"a\": [],\n  \"n\": [\n    1,\n    {\n      \"x\": null\n    }\n  ],\n  \"s\": \"tab\\t\\u0001 ø \\\"q\\\"\"\n}");
    }
}
