//! masu's scrape-record rules (storage-format.md, fixtures/records/README.md): what the store
//! checks on each pushed record. The agent runs them on every record it builds (ADR 0003).

use crate::json::{self, field};
use crate::ts;
use serde_json::Value;

/// A pushed record: 4 MiB stored, less the 43-byte `recv_ts` member the store inserts.
pub const MAX_RECORD_BYTES: usize = 4 * 1024 * 1024 - 43;
/// A scrape body is capped so that a refused record still fits once base64-encoded (ADR 0003).
pub const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

pub fn check(b: &[u8]) -> Result<(), String> {
    if b.len() > MAX_RECORD_BYTES {
        return Err("record-too-large".into());
    }
    if std::str::from_utf8(b).is_err() {
        return Err("not-utf8".into());
    }
    if b.first() != Some(&b'{')
        || b.last() != Some(&b'}')
        || b.iter().any(|&c| c == b'\n' || c == b'\r')
    {
        return Err("framing".into());
    }
    let obj = json::parse(b)?;
    // masu parses with serde_json, stricter than the shared scanner the log path keeps (nisshi):
    // a number out of its range or a lone surrogate is `not-json` there.
    if serde_json::from_slice::<Value>(b).is_err() {
        return Err("not-json".into());
    }
    if let Some(name) = obj.first_duplicate() {
        return Err(format!("duplicate-field:{name}"));
    }
    if obj.get("recv_ts").is_some() {
        return Err("reserved-field:recv_ts".into());
    }
    field(&obj, "v", |v| v.as_u64().filter(|&n| n == 1))?;
    // masu: a real date and time that fits signed 64-bit Unix nanoseconds.
    field(&obj, "ts", |v| {
        v.as_str().filter(|s| ts::to_unix_nanos(s).is_some())
    })?;
    field(&obj, "node", |v| v.as_str().filter(|s| json::is_node(s)))?;
    field(&obj, "service", Value::as_str)?;
    field(&obj, "stream", |v| {
        v.as_str().filter(|s| json::is_stream(s))
    })?;
    field(&obj, "seq", Value::as_u64)?;
    let up = field(&obj, "up", Value::as_bool)?;
    field(&obj, "dur_us", Value::as_u64)?;
    let error = if up {
        field(&obj, "content_type", Value::as_str)?;
        field(&obj, "body", Value::as_str)?;
        None
    } else {
        Some(field(&obj, "error", Value::as_str)?)
    };
    if obj.get("raw_b64").is_some() {
        field(&obj, "raw_b64", Value::as_str)?;
        if !error.is_some_and(|e| e.starts_with("rejected:")) {
            return Err("unexpected-field:raw_b64".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_serde_json_refuses_is_not_json() {
        let rec = |extra: &str| {
            format!(
                r#"{{"v":1,"ts":"2026-10-19T08:14:00.000213904Z","node":"a","service":"","stream":"{}","seq":0,"up":false,"dur_us":0,"error":"","extra":{extra}}}"#,
                "0".repeat(32)
            )
        };
        assert_eq!(check(rec("1e10").as_bytes()), Ok(()));
        assert_eq!(check(rec("1e10000").as_bytes()), Err("not-json".into()));
        assert_eq!(check(rec(r#""\ud800""#).as_bytes()), Err("not-json".into()));
        // The log path keeps both forms, as nisshi does.
        assert!(json::parse(rec("1e10000").as_bytes()).is_ok());
        assert!(json::parse(rec(r#""\ud800""#).as_bytes()).is_ok());
    }
}
