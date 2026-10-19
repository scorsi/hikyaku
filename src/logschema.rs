//! nisshi's log schema (log-schema.md, "Validation and reasons"): the agent validates service
//! lines with exactly the store's rules, before a `seq` is assigned (ADR 0003), and every
//! record it builds with the record rules. The `Err` strings are the fixtures' reasons.

use crate::json::{self, Members, field};
use crate::ts;
use serde_json::Value;

/// A service line is at most 1 MiB.
pub const MAX_LINE_BYTES: usize = 1 << 20;
/// A pushed record: 4 MiB stored, less the 43-byte `,"recv_ts":"…"` member the store inserts.
pub const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024 - 43;

const LEVELS: [&str; 4] = ["debug", "info", "warn", "error"];
/// The agent's fields: a service line carrying one is invalid.
const AGENT_FIELDS: [&str; 5] = ["node", "service", "stream", "seq", "source"];

/// One line as a service writes it, without its newline.
pub fn check_line(b: &[u8]) -> Result<(), String> {
    check(b, false)
}

/// One record as the agent pushes it.
pub fn check_record(b: &[u8]) -> Result<(), String> {
    check(b, true)
}

fn check(b: &[u8], record: bool) -> Result<(), String> {
    if record && b.len() > MAX_RECORD_BYTES {
        return Err("record-too-large".into());
    }
    if !record && b.len() > MAX_LINE_BYTES {
        return Err("line-too-large".into());
    }
    if std::str::from_utf8(b).is_err() {
        return Err("not-utf8".into());
    }
    // The store inserts recv_ts before the last byte and keeps one record per line.
    if record
        && (b.first() != Some(&b'{')
            || b.last() != Some(&b'}')
            || b.iter().any(|&c| c == b'\n' || c == b'\r'))
    {
        return Err("framing".into());
    }
    let obj = json::parse(b)?;
    if let Some(name) = obj.first_duplicate() {
        return Err(format!("duplicate-field:{name}"));
    }
    if let Some((name, _)) = obj.0.iter().find(|(k, _)| {
        k == "recv_ts" || k.starts_with('_') || (!record && AGENT_FIELDS.contains(&k.as_str()))
    }) {
        return Err(format!("reserved-field:{name}"));
    }
    field(&obj, "v", |v| v.as_u64().filter(|&n| n == 1))?;
    field(&obj, "ts", |v| v.as_str().filter(|s| ts::is_timestamp(s)))?;
    let level = field(&obj, "level", |v| v.as_str().filter(|s| LEVELS.contains(s)))?;
    field(&obj, "event", |v| v.as_str().filter(|s| is_event(s)))?;
    optional(&obj, "msg", Value::as_str)?;
    if obj.get("err").is_some() {
        field(&obj, "err", |v| {
            v.as_str().filter(|_| level == "warn" || level == "error")
        })?;
    }
    if record {
        field(&obj, "node", |v| v.as_str().filter(|s| json::is_node(s)))?;
        field(&obj, "service", |v| v.as_str().filter(|s| !s.is_empty()))?;
        field(&obj, "stream", |v| {
            v.as_str().filter(|s| json::is_stream(s))
        })?;
        field(&obj, "seq", |v| v.as_u64().filter(|&n| n >= 1))?;
        optional(&obj, "source", |v| {
            v.as_str().filter(|s| *s == "journald" || *s == "file")
        })?;
    }
    Ok(())
}

fn optional<'a, T>(
    obj: &'a Members,
    name: &str,
    get: impl Fn(&'a Value) -> Option<T>,
) -> Result<(), String> {
    match obj.get(name) {
        Some(_) => field(obj, name, get).map(|_| ()),
        None => Ok(()),
    }
}

/// `^[a-z][a-z0-9]*(-[a-z0-9]+)*$`
pub fn is_event(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'-'))
        && !b.ends_with(b"-")
        && !s.contains("--")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events() {
        for ok in ["query", "upstream-timeout", "a1-b2", "x"] {
            assert!(is_event(ok), "{ok}");
        }
        for bad in ["", "Upstream_Timeout", "-a", "a-", "a--b", "1a", "a b"] {
            assert!(!is_event(bad), "{bad}");
        }
    }

    #[test]
    fn line_and_record_differ() {
        let line =
            br#"{"v":1,"ts":"2026-10-19T08:14:03.512004117Z","level":"info","event":"query"}"#;
        assert_eq!(check_line(line), Ok(()));
        assert_eq!(check_record(line), Err("missing-field:node".into()));
        assert_eq!(check_record(b" {}"), Err("framing".into()));
        assert_eq!(check_line(b" {}  "), Err("missing-field:v".into()));
    }
}
