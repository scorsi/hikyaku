//! Records the agent builds (ADR 0003): a service line kept as is, a wrapped line (`line`,
//! `unparsed`), an own event, a scrape record. The `stream` and `seq` are added by the spool at
//! commit time, so a record waits here as a `PendingLog` or a `Scrape` until then.

use crate::logschema::{self, MAX_LINE_BYTES, MAX_RECORD_BYTES};
use serde_json::Value;

/// `PRIORITY` → level (log-schema.md, "Wrapped lines").
pub fn level_of_priority(p: &str) -> &'static str {
    match p {
        "0" | "1" | "2" | "3" => "error",
        "4" => "warn",
        "7" => "debug",
        _ => "info",
    }
}

/// A log record waiting for its `stream` and `seq`: `line` is the service-line form (no agent
/// field), already validated or wrapped.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingLog {
    pub line: String,
    pub service: String,
    pub source: Option<&'static str>,
}

impl PendingLog {
    /// A line read from journald or a file: kept as is when it is a valid schema line, else
    /// wrapped as `unparsed` (looked like JSON) or `line`.
    pub fn from_source(
        raw: &[u8],
        ts: &str,
        level: &str,
        service: &str,
        source: &'static str,
    ) -> PendingLog {
        let trimmed = trim_ascii(raw);
        let line = if trimmed.first() == Some(&b'{') {
            match logschema::check_line(trimmed) {
                // check_line guarantees UTF-8.
                Ok(()) => String::from_utf8(trimmed.to_vec()).unwrap_or_default(),
                Err(reason) => wrap(ts, "warn", "unparsed", Some(&reason), raw),
            }
        } else {
            wrap(ts, level, "line", None, raw)
        };
        PendingLog {
            line,
            service: service.to_string(),
            source: Some(source),
        }
    }

    /// One of the agent's own events (service `hikyaku`, no `source`).
    pub fn event(level: &str, event: &str, fields: &[(&str, Value)]) -> PendingLog {
        PendingLog {
            line: service_line(&crate::ts::now(), level, event, fields),
            service: "hikyaku".into(),
            source: None,
        }
    }

    /// The pushed record: the agent's fields inserted before the final `}` of the line, so the
    /// service's bytes are never re-serialized.
    pub fn render(&self, node: &str, stream: &str, seq: u64) -> String {
        let mut out = self
            .line
            .strip_suffix('}')
            .unwrap_or(&self.line)
            .to_string();
        out.push_str(&format!(
            ",\"node\":{},\"service\":{},\"stream\":\"{stream}\",\"seq\":{seq}",
            Value::from(node),
            Value::from(self.service.as_str())
        ));
        if let Some(source) = self.source {
            out.push_str(&format!(",\"source\":\"{source}\""));
        }
        out.push('}');
        out
    }
}

/// `{"v":1,"ts":…,"level":…,"event":…,<fields>}`; `msg` and `err` keep the schema's place.
pub fn service_line(ts: &str, level: &str, event: &str, fields: &[(&str, Value)]) -> String {
    let mut out = format!("{{\"v\":1,\"ts\":\"{ts}\",\"level\":\"{level}\",\"event\":\"{event}\"");
    for name in ["msg", "err"] {
        if let Some((_, v)) = fields.iter().find(|(k, _)| *k == name) {
            out.push_str(&format!(",\"{name}\":{v}"));
        }
    }
    for (k, v) in fields {
        if *k != "msg" && *k != "err" {
            out.push_str(&format!(",{}:{v}", Value::from(*k)));
        }
    }
    out.push('}');
    out
}

/// A wrapped line: `msg` (UTF-8) or `msg_b64`, cut to 1 MiB with `truncated:true`, and cut
/// further if JSON escaping still makes the record too large for the store.
pub fn wrap(ts: &str, level: &str, event: &str, reason: Option<&str>, msg: &[u8]) -> String {
    let mut limit = MAX_LINE_BYTES;
    loop {
        let (cut, truncated) = if msg.len() > limit {
            (&msg[..limit], true)
        } else {
            (msg, false)
        };
        let mut fields: Vec<(&str, Value)> = Vec::new();
        match std::str::from_utf8(cut) {
            Ok(s) => fields.push(("msg", Value::from(s))),
            Err(e) if truncated && e.error_len().is_none() => {
                // Cut inside a character: keep the whole characters.
                let s = std::str::from_utf8(&cut[..e.valid_up_to()]).unwrap_or_default();
                fields.push(("msg", Value::from(s)))
            }
            Err(_) => fields.push(("msg_b64", Value::from(base64(cut)))),
        }
        if let Some(r) = reason {
            fields.push(("reason", Value::from(r)));
        }
        if truncated {
            fields.push(("truncated", Value::Bool(true)));
        }
        let line = service_line(ts, level, event, &fields);
        // Room for the agent's fields (node, service, stream, seq, source).
        if line.len() + 256 <= MAX_RECORD_BYTES || limit < 1024 {
            return line;
        }
        limit /= 2;
    }
}

/// A scrape record waiting for its `stream` and `seq` (masu storage-format.md).
#[derive(Debug, Clone, PartialEq)]
pub struct Scrape {
    pub ts: String,
    pub service: String,
    pub dur_us: u64,
    pub result: ScrapeResult,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScrapeResult {
    Up { content_type: String, body: String },
    Down { error: String },
}

impl Scrape {
    pub fn render(&self, node: &str, stream: &str, seq: u64) -> String {
        let head = format!(
            "{{\"v\":1,\"ts\":\"{}\",\"node\":{},\"service\":{},\"stream\":\"{stream}\",\"seq\":{seq}",
            self.ts,
            Value::from(node),
            Value::from(self.service.as_str())
        );
        match &self.result {
            ScrapeResult::Up { content_type, body } => format!(
                "{head},\"up\":true,\"dur_us\":{},\"content_type\":{},\"body\":{}}}",
                self.dur_us,
                Value::from(content_type.as_str()),
                Value::from(body.as_str())
            ),
            ScrapeResult::Down { error } => format!(
                "{head},\"up\":false,\"dur_us\":{},\"error\":{}}}",
                self.dur_us,
                Value::from(error.as_str())
            ),
        }
    }
}

fn trim_ascii(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    &b[start..end]
}

/// Standard base64 with padding (RFC 4648 section 4).
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let mut n = 0u32;
        for i in 0..3 {
            n = n << 8 | *chunk.get(i).unwrap_or(&0) as u32;
        }
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logschema::check_record;
    use crate::scrape_record;

    const TS: &str = "2026-10-19T08:14:03.512004000Z";
    const STREAM: &str = "9b1e0c5d7a2f4e8b9c0d1e2f3a4b5c6d";

    fn valid(p: &PendingLog) -> Value {
        let r = p.render("node-a", STREAM, 7);
        assert_eq!(check_record(r.as_bytes()), Ok(()), "{r}");
        serde_json::from_str(&r).unwrap()
    }

    #[test]
    fn kept_wrapped_and_unparsed() {
        let kept = PendingLog::from_source(
            br#"  {"v":1,"ts":"2026-10-19T08:14:03.512004117Z","level":"info","event":"query","n":1}  "#,
            TS,
            "info",
            "annai",
            "journald",
        );
        let v = valid(&kept);
        assert_eq!(v["n"], 1);
        assert_eq!(v["source"], "journald");
        assert_eq!(v["seq"], 7);

        let third = PendingLog::from_source(b"Accepted publickey", TS, "warn", "sshd", "journald");
        let v = valid(&third);
        assert_eq!(v["event"], "line");
        assert_eq!(v["level"], "warn");
        assert_eq!(v["msg"], "Accepted publickey");

        let bad = PendingLog::from_source(
            br#"{"v":1,"level":"info","event":"x"}"#,
            TS,
            "info",
            "genkan",
            "file",
        );
        let v = valid(&bad);
        assert_eq!(v["event"], "unparsed");
        assert_eq!(v["reason"], "missing-field:ts");
        assert_eq!(v["msg"], r#"{"v":1,"level":"info","event":"x"}"#);

        let reserved = PendingLog::from_source(
            br#"{"v":1,"ts":"2026-10-19T08:14:03.512004117Z","level":"info","event":"x","node":"b"}"#,
            TS,
            "info",
            "genkan",
            "journald",
        );
        assert_eq!(valid(&reserved)["reason"], "reserved-field:node");

        let binary = PendingLog::from_source(&[0xff, 0xfe], TS, "info", "sshd", "journald");
        assert_eq!(valid(&binary)["msg_b64"], "//4=");
    }

    #[test]
    fn long_lines_are_cut() {
        let big = vec![b'x'; MAX_LINE_BYTES + 10];
        let p = PendingLog::from_source(&big, TS, "info", "svc", "file");
        let v = valid(&p);
        assert_eq!(v["truncated"], true);
        assert_eq!(v["msg"].as_str().unwrap().len(), MAX_LINE_BYTES);
        // Every byte escapes to 6: the record would exceed 4 MiB without the second cut.
        let ctl = vec![0x01u8; MAX_LINE_BYTES];
        let p = PendingLog::from_source(&ctl, TS, "info", "svc", "file");
        assert!(valid(&p)["truncated"].as_bool().unwrap());
        // A JSON-looking line over 1 MiB is unparsed, not kept.
        let mut json =
            br#"{"v":1,"ts":"2026-10-19T08:14:03.512004117Z","level":"info","event":"big","pad":""#
                .to_vec();
        json.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES));
        json.extend(b"\"}");
        let p = PendingLog::from_source(&json, TS, "info", "svc", "file");
        assert_eq!(valid(&p)["reason"], "line-too-large");
    }

    #[test]
    fn own_events() {
        let e = PendingLog::event(
            "warn",
            "spool-full",
            &[("signal", "logs".into()), ("spool_bytes", 10.into())],
        );
        let v = valid(&e);
        assert_eq!(v["service"], "hikyaku");
        assert!(v.get("source").is_none());
        let line = service_line(
            TS,
            "error",
            "x",
            &[("a", 1.into()), ("err", "boom".into()), ("msg", "m".into())],
        );
        assert_eq!(
            line,
            format!(
                r#"{{"v":1,"ts":"{TS}","level":"error","event":"x","msg":"m","err":"boom","a":1}}"#
            )
        );
    }

    #[test]
    fn scrape_records() {
        let up = Scrape {
            ts: TS.into(),
            service: "annai".into(),
            dur_us: 812,
            result: ScrapeResult::Up {
                content_type: "text/plain; version=0.0.4".into(),
                body: "a_total 1\n".into(),
            },
        };
        let r = up.render("node-a", STREAM, 1);
        assert_eq!(scrape_record::check(r.as_bytes()), Ok(()), "{r}");
        let down = Scrape {
            result: ScrapeResult::Down {
                error: "series-limit".into(),
            },
            ..up
        };
        let r = down.render("node-a", STREAM, 2);
        assert_eq!(scrape_record::check(r.as_bytes()), Ok(()), "{r}");
        assert!(r.contains(r#""up":false,"dur_us":812,"error":"series-limit"}"#));
    }

    #[test]
    fn base64_encodes() {
        assert_eq!(base64(&[0xff]), "/w==");
        assert_eq!(base64(b"{\"v\":1}"), "eyJ2IjoxfQ==");
        assert_eq!(base64(b"abc"), "YWJj");
        assert_eq!(base64(b""), "");
    }
}
