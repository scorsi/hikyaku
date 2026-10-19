//! Top-level members of a JSON object in document order, duplicates kept: serde_json's map
//! silently keeps the last duplicate, and the stores blame the first repeated name.

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

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
    serde_json::from_slice::<Members>(bytes).map_err(|e| match e.classify() {
        serde_json::error::Category::Data => "not-object",
        _ => "not-json",
    })
}

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Members;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members, A::Error> {
                let mut out = Vec::new();
                while let Some(k) = map.next_key::<String>()? {
                    out.push((k, map.next_value()?));
                }
                Ok(Members(out))
            }
        }
        d.deserialize_map(V)
    }
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
}
