//! Top-level members of a JSON object in document order, duplicates kept: serde_json's map
//! silently keeps the last duplicate, and the stores blame the first repeated name.
//!
//! The text is checked by a scanner of its own, not serde_json, for nisshi's parser edges
//! (log-schema.md): nesting capped at 64 levels without recursion, numbers of any magnitude
//! and lone UTF-16 surrogate escapes valid. Member names and string values decode lone
//! surrogates to U+FFFD, as the stores (Go's encoding/json) do; a member value serde_json
//! cannot hold (`1e10000`) is `Null`, so only `v`, `seq` and the like can fail on it.

use serde_json::Value;
use std::collections::HashSet;

/// The deepest nesting accepted: the top-level object is level 1 (nisshi log-schema.md).
pub const MAX_DEPTH: usize = 64;

#[derive(Debug, PartialEq)]
pub struct Members(pub Vec<(String, Value)>);

impl Members {
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// The first member name seen twice (after unescaping), if any.
    pub fn first_duplicate(&self) -> Option<&str> {
        let mut seen = HashSet::new();
        self.0
            .iter()
            .find(|(k, _)| !seen.insert(k.as_str()))
            .map(|(k, _)| k.as_str())
    }
}

/// `not-json` unless the bytes hold exactly one JSON value, `not-object` unless it is an object.
pub fn parse(bytes: &[u8]) -> Result<Members, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not-json")?;
    let members = scan(text.as_bytes()).ok_or("not-json")?;
    let members = members.ok_or("not-object")?;
    Ok(Members(
        members
            .into_iter()
            .map(|(k, v)| (unescape(&text[k]), decode(&text[v])))
            .collect(),
    ))
}

type Span = std::ops::Range<usize>;

/// One JSON value (RFC 8259) with surrounding whitespace, iteratively: `None` when invalid or
/// deeper than `MAX_DEPTH`, else the top-level members (key and value spans) of an object.
#[allow(clippy::type_complexity)]
fn scan(b: &[u8]) -> Option<Option<Vec<(Span, Span)>>> {
    let mut i = 0;
    let ws = |i: &mut usize| {
        while matches!(b.get(*i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            *i += 1;
        }
    };
    // Expected closers of the open containers.
    let mut stack: Vec<u8> = Vec::new();
    let mut members = Vec::new();
    let mut key: Span = 0..0;
    let mut start = 0;
    ws(&mut i);
    let object = b.get(i) == Some(&b'{');
    // `i` is at the start of a value.
    loop {
        ws(&mut i);
        if stack.len() == 1 {
            start = i;
        }
        let mut done = true;
        match *b.get(i)? {
            open @ (b'{' | b'[') => {
                stack.push(if open == b'{' { b'}' } else { b']' });
                if stack.len() > MAX_DEPTH {
                    return None;
                }
                i += 1;
                ws(&mut i);
                if b.get(i) == Some(stack.last()?) {
                    i += 1;
                    stack.pop();
                } else {
                    done = false;
                    if open == b'{' {
                        let k = string(b, &mut i)?;
                        if stack.len() == 1 {
                            key = k;
                        }
                        ws(&mut i);
                        (b.get(i) == Some(&b':')).then_some(())?;
                        i += 1;
                    }
                }
            }
            b'"' => {
                string(b, &mut i)?;
            }
            b'-' | b'0'..=b'9' => number(b, &mut i)?,
            _ => {
                let lit = [&b"true"[..], b"false", b"null"]
                    .into_iter()
                    .find(|l| b[i..].starts_with(l))?;
                i += lit.len();
            }
        }
        if !done {
            continue;
        }
        // A value ended: close containers until the next member or element.
        loop {
            if stack.len() == 1 && object {
                members.push((key.clone(), start..i));
            }
            ws(&mut i);
            let Some(&close) = stack.last() else {
                return (i == b.len()).then(|| object.then_some(members));
            };
            match *b.get(i)? {
                b',' => {
                    i += 1;
                    if close == b'}' {
                        ws(&mut i);
                        let k = string(b, &mut i)?;
                        if stack.len() == 1 {
                            key = k;
                        }
                        ws(&mut i);
                        (b.get(i) == Some(&b':')).then_some(())?;
                        i += 1;
                    }
                    break;
                }
                c if c == close => {
                    i += 1;
                    stack.pop();
                }
                _ => return None,
            }
        }
    }
}

/// A string at `b[*i]`: its span, quotes included; `None` unless valid.
fn string(b: &[u8], i: &mut usize) -> Option<Span> {
    let start = *i;
    (b.get(*i) == Some(&b'"')).then_some(())?;
    *i += 1;
    loop {
        match *b.get(*i)? {
            b'"' => {
                *i += 1;
                return Some(start..*i);
            }
            b'\\' => {
                *i += 1;
                match *b.get(*i)? {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => *i += 1,
                    b'u' => {
                        let hex = b.get(*i + 1..*i + 5)?;
                        hex.iter().all(u8::is_ascii_hexdigit).then_some(())?;
                        *i += 5;
                    }
                    _ => return None,
                }
            }
            0..=0x1f => return None,
            _ => *i += 1,
        }
    }
}

/// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`, syntax only.
fn number(b: &[u8], i: &mut usize) -> Option<()> {
    let digits = |i: &mut usize| {
        let from = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        (*i > from).then_some(())
    };
    if b.get(*i) == Some(&b'-') {
        *i += 1;
    }
    if b.get(*i) == Some(&b'0') {
        *i += 1;
    } else {
        digits(i)?;
    }
    if b.get(*i) == Some(&b'.') {
        *i += 1;
        digits(i)?;
    }
    if matches!(b.get(*i), Some(b'e' | b'E')) {
        *i += 1;
        if matches!(b.get(*i), Some(b'+' | b'-')) {
            *i += 1;
        }
        digits(i)?;
    }
    Some(())
}

/// A member value as serde_json holds it; a string with a lone surrogate decoded as the stores
/// do, a number out of serde_json's range as `Null`.
fn decode(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| {
        if raw.starts_with('"') {
            Value::String(unescape(raw))
        } else {
            Value::Null
        }
    })
}

/// A scanned string (quotes included) unescaped; a lone surrogate becomes U+FFFD.
fn unescape(raw: &str) -> String {
    let s = &raw[1..raw.len() - 1];
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices();
    let hex = |at: usize| u32::from_str_radix(&s[at..at + 4], 16).unwrap_or(0);
    while let Some((at, c)) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some((_, e)) = chars.next() else { break };
        out.push(match e {
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'u' => {
                let hi = hex(at + 2);
                chars.nth(3);
                let pair = (0xd800..0xdc00).contains(&hi) && s[at + 6..].starts_with("\\u");
                let lo = if pair { hex(at + 8) } else { 0 };
                if pair && (0xdc00..0xe000).contains(&lo) {
                    chars.nth(5);
                    char::from_u32(0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00))
                } else {
                    char::from_u32(hi)
                }
                .unwrap_or('\u{fffd}')
            }
            other => other,
        });
    }
    out
}

/// `[a-z0-9][a-z0-9-]{0,62}`.
pub fn is_node(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && !s.starts_with('-')
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// 32 lowercase hex digits: the 16-byte `stream_id`.
pub fn is_stream(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `missing-field:<name>` when absent, `<name>-format` when `get` refuses the value.
pub fn field<'a, T>(
    obj: &'a Members,
    name: &str,
    get: impl Fn(&'a Value) -> Option<T>,
) -> Result<T, String> {
    let v = obj
        .get(name)
        .ok_or_else(|| format!("missing-field:{name}"))?;
    get(v).ok_or_else(|| format!("{name}-format"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_duplicates() {
        let m = parse(br#"{"b":1,"a":2,"a":3,"b":4}"#).unwrap();
        assert_eq!(m.0.len(), 4);
        assert_eq!(m.first_duplicate(), Some("a"));
        assert_eq!(parse(b"[1]").unwrap_err(), "not-object");
        assert_eq!(parse(b"\"s\"").unwrap_err(), "not-object");
        assert_eq!(parse(b"null").unwrap_err(), "not-object");
        assert_eq!(parse(b"").unwrap_err(), "not-json");
        assert_eq!(parse(b"{} {}").unwrap_err(), "not-json");
        assert_eq!(parse(b"{\"a\":1,}"), Err("not-json"));
        assert!(parse(b"  {\"a\":1}  ").is_ok());
    }

    #[test]
    fn parser_edges() {
        // nisshi log-schema.md, "Parser edges".
        let deep = |n: usize| format!("{{\"d\":{}{}}}", "[".repeat(n - 1), "]".repeat(n - 1));
        assert!(parse(deep(64).as_bytes()).is_ok());
        assert_eq!(parse(deep(65).as_bytes()), Err("not-json"));
        // Far past any stack: refused, not a crash.
        assert_eq!(parse(deep(1_000_000).as_bytes()), Err("not-json"));
        let m = parse(br#"{"big":1e10000,"n":-0.5E+3,"z":0,"d":123456789012345678901234567890}"#)
            .unwrap();
        assert_eq!(m.get("big"), Some(&Value::Null));
        assert_eq!(m.get("z"), Some(&Value::from(0)));
        for bad in [
            &b"{\"a\":01}"[..],
            b"{\"a\":1.}",
            b"{\"a\":.1}",
            b"{\"a\":1e}",
            b"{\"a\":-}",
            b"{\"a\":+1}",
            b"{\"a\":\"\\x\"}",
            b"{\"a\":\"\\u12\"}",
            b"{\"a\":\"\t\"}",
            b"{\"a\":tru}",
            b"{\"a\" 1}",
            b"{1:1}",
            b"{\"a\":[1,]}",
            b"{\"a\":[1 2]}",
            b"{\"a\":{\"b\":1]}",
            b"{\"a\":1",
        ] {
            assert_eq!(
                parse(bad),
                Err("not-json"),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let m = parse(
            br#"{"s":"a\ud800b","p":"\ud83d\ude00","h":"\ud800\u0041","e":"\"\\\/\b\f\n\r\t","\u0076":[],"x":{"y":[{}]}}"#,
        )
        .unwrap();
        assert_eq!(m.get("s"), Some(&Value::from("a\u{fffd}b")));
        assert_eq!(m.get("p"), Some(&Value::from("\u{1f600}")));
        assert_eq!(m.get("h"), Some(&Value::from("\u{fffd}A")));
        assert_eq!(m.get("e"), Some(&Value::from("\"\\/\u{8}\u{c}\n\r\t")));
        assert_eq!(m.get("v"), Some(&Value::Array(vec![])));
        assert_eq!(m.0.len(), 6);
        // Names are compared exactly, after unescaping.
        let m = parse(br#"{"ts":1,"TS":2,"\u0074s":3}"#).unwrap();
        assert_eq!(m.first_duplicate(), Some("ts"));
        assert_eq!(m.get("TS"), Some(&Value::from(2)));
    }
}
