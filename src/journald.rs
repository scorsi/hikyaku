//! journald through a `journalctl --output=json --follow` child (ADR 0002, 0003): entries are
//! parsed here into the records of ADR 0003; the child and the cursor are driven by `agent`.

use crate::record::{PendingLog, level_of_priority};
use crate::ts;
use serde_json::Value;
use std::io;
use std::process::{Child, Command, Stdio};

/// `SD_MESSAGE_JOURNAL_DROPPED`: journald's "Suppressed N messages from <unit>" under its rate
/// limit. Verified by a nixosTest against the deployed systemd, not trusted from here.
pub const SUPPRESSED_MESSAGE_ID: &str = "a596d6fe7bfa4994828e72309e95d61e";

/// The agent's own unit: its lines are already in the stream as own events.
pub const OWN_UNIT: &str = "hikyaku";

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub cursor: String,
    pub realtime_us: u64,
    /// Unit without `.service`, else `SYSLOG_IDENTIFIER`, else `_COMM`, else `journald`.
    pub unit: String,
    pub priority: String,
    pub message: Vec<u8>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Record(PendingLog),
    Suppressed {
        unit: String,
        count: u64,
    },
    /// A line of the agent's own unit.
    Own,
}

/// `journalctl` from the saved cursor (the entry at the cursor comes first and is skipped by
/// the caller, or reveals a gap), or from the start of the boot for a fresh spool.
pub fn spawn(cursor: Option<&str>) -> io::Result<Child> {
    Command::new("journalctl")
        .args(args(cursor))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
}

fn args(cursor: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "--output=json",
        "--all",
        "--quiet",
        "--follow",
        "--lines=all",
    ]
    .map(String::from)
    .into();
    match cursor {
        // `--follow` alone implies `--boot` (the current boot, journalctl.c in systemd 260):
        // from a cursor of the previous boot it would skip that boot's tail (journal-gap).
        Some(c) => a.extend([format!("--cursor={c}"), "--boot=all".into()]),
        None => a.push("--boot".into()),
    }
    a
}

/// One line of `journalctl -o json`. Fields journald could not print as UTF-8 strings come as
/// arrays of byte values; `--all` keeps the long ones.
pub fn parse_entry(line: &[u8]) -> Option<Entry> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let text = |name: &str| v.get(name).and_then(Value::as_str).map(str::to_string);
    let unit = text("_SYSTEMD_UNIT")
        .map(|u| u.strip_suffix(".service").unwrap_or(&u).to_string())
        .or_else(|| text("SYSLOG_IDENTIFIER"))
        .or_else(|| text("_COMM"))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "journald".into());
    Some(Entry {
        cursor: text("__CURSOR")?,
        realtime_us: text("__REALTIME_TIMESTAMP")
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(now_us),
        unit,
        priority: text("PRIORITY").unwrap_or_default(),
        message: bytes_of(v.get("MESSAGE")),
        message_id: text("MESSAGE_ID"),
    })
}

fn bytes_of(v: Option<&Value>) -> Vec<u8> {
    match v {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(a)) if a.iter().all(|x| x.as_u64().is_some_and(|n| n < 256)) => {
            a.iter().map(|x| x.as_u64().unwrap_or(0) as u8).collect()
        }
        Some(Value::Null) | None => Vec::new(),
        Some(other) => other.to_string().into_bytes(),
    }
}

pub fn classify(e: &Entry) -> Item {
    if e.unit == OWN_UNIT {
        return Item::Own;
    }
    if (e.message_id.as_deref() == Some(SUPPRESSED_MESSAGE_ID) || e.unit == "systemd-journald")
        && let Some((unit, count)) = parse_suppressed(&e.message)
    {
        return Item::Suppressed { unit, count };
    }
    Item::Record(PendingLog::from_source(
        &e.message,
        &ts::from_micros(e.realtime_us),
        level_of_priority(&e.priority),
        &e.unit,
        "journald",
    ))
}

/// "Suppressed 1234 messages from generator.service" → `("generator.service", 1234)`.
fn parse_suppressed(msg: &[u8]) -> Option<(String, u64)> {
    let s = std::str::from_utf8(msg).ok()?;
    let rest = s.strip_prefix("Suppressed ")?;
    let (count, rest) = rest.split_once(" messages from ")?;
    let unit = rest.split_whitespace().next()?;
    Some((unit.to_string(), count.parse().ok()?))
}

/// The realtime microseconds a cursor carries (`t=` field, hex).
pub fn cursor_realtime_us(cursor: &str) -> Option<u64> {
    cursor
        .split(';')
        .find_map(|f| f.strip_prefix("t="))
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
}

fn now_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logschema::check_record;

    const STREAM: &str = "9b1e0c5d7a2f4e8b9c0d1e2f3a4b5c6d";

    // Shapes of `journalctl -o json` lines: a text message, a binary message (array of bytes),
    // a schema line from a house service, and journald's own suppression message.
    const SSHD: &str = r#"{"__CURSOR":"s=6b7a;i=1a2;b=9f;m=2d6e;t=65e5c0b2a8f04;x=1","__REALTIME_TIMESTAMP":"1792397643512004","__MONOTONIC_TIMESTAMP":"186150","_BOOT_ID":"9f","PRIORITY":"6","SYSLOG_FACILITY":"4","SYSLOG_IDENTIFIER":"sshd","_PID":"812","_COMM":"sshd","_SYSTEMD_UNIT":"sshd.service","MESSAGE":"Accepted publickey for root from 198.51.100.7 port 51122 ssh2"}"#;
    const BINARY: &str = r#"{"__CURSOR":"s=6b7a;i=1a3;b=9f;m=2d6f;t=65e5c0b2a8f05;x=2","__REALTIME_TIMESTAMP":"1792397643512005","PRIORITY":"3","_SYSTEMD_UNIT":"forgejo.service","MESSAGE":[255,254,65]}"#;
    const HOUSE: &str = r#"{"__CURSOR":"s=6b7a;i=1a4;b=9f;m=2d70;t=65e5c0b2a8f06;x=3","__REALTIME_TIMESTAMP":"1792397643512006","PRIORITY":"6","_SYSTEMD_UNIT":"annai.service","MESSAGE":"{\"v\":1,\"ts\":\"2026-10-19T08:14:03.512004117Z\",\"level\":\"info\",\"event\":\"query\",\"qname\":\"example.org.\"}"}"#;
    const SUPPRESSED: &str = r#"{"__CURSOR":"s=6b7a;i=1a5;b=9f;m=2d71;t=65e5c0b2a8f07;x=4","__REALTIME_TIMESTAMP":"1792397643512007","PRIORITY":"5","_SYSTEMD_UNIT":"systemd-journald.service","SYSLOG_IDENTIFIER":"systemd-journald","MESSAGE_ID":"a596d6fe7bfa4994828e72309e95d61e","N_DROPPED":"9873","MESSAGE":"Suppressed 9873 messages from flood.service"}"#;
    const KERNEL: &str = r#"{"__CURSOR":"s=6b7a;i=1a6;b=9f;m=2d72;t=65e5c0b2a8f08;x=5","__REALTIME_TIMESTAMP":"1792397643512008","PRIORITY":"4","_TRANSPORT":"kernel","SYSLOG_IDENTIFIER":"kernel","MESSAGE":"usb 1-1: reset"}"#;
    const OWN: &str = r#"{"__CURSOR":"s=6b7a;i=1a7;b=9f;m=2d73;t=65e5c0b2a8f09;x=6","__REALTIME_TIMESTAMP":"1792397643512009","PRIORITY":"6","_SYSTEMD_UNIT":"hikyaku.service","MESSAGE":"{\"v\":1,\"ts\":\"2026-10-19T08:14:03.512004117Z\",\"level\":\"info\",\"event\":\"heartbeat\"}"}"#;

    fn record(item: Item) -> serde_json::Value {
        let Item::Record(p) = item else {
            panic!("{item:?}")
        };
        let r = p.render("node-a", STREAM, 1);
        assert_eq!(check_record(r.as_bytes()), Ok(()), "{r}");
        serde_json::from_str(&r).unwrap()
    }

    #[test]
    fn wraps_journal_entries() {
        let e = parse_entry(SSHD.as_bytes()).unwrap();
        assert_eq!(e.unit, "sshd");
        assert_eq!(e.cursor, "s=6b7a;i=1a2;b=9f;m=2d6e;t=65e5c0b2a8f04;x=1");
        let r = record(classify(&e));
        assert_eq!(r["event"], "line");
        assert_eq!(r["ts"], "2026-10-19T08:14:03.512004000Z");
        assert_eq!(r["level"], "info");
        assert_eq!(r["service"], "sshd");
        assert_eq!(r["source"], "journald");
        assert_eq!(
            r["msg"],
            "Accepted publickey for root from 198.51.100.7 port 51122 ssh2"
        );

        let r = record(classify(&parse_entry(BINARY.as_bytes()).unwrap()));
        assert_eq!(r["msg_b64"], "//5B");
        assert_eq!(r["level"], "error");
        assert_eq!(r["service"], "forgejo");

        let r = record(classify(&parse_entry(HOUSE.as_bytes()).unwrap()));
        assert_eq!(r["event"], "query");
        assert_eq!(r["qname"], "example.org.");
        assert_eq!(r["service"], "annai");

        let r = record(classify(&parse_entry(KERNEL.as_bytes()).unwrap()));
        assert_eq!(r["service"], "kernel");
        assert_eq!(r["level"], "warn");

        assert_eq!(
            classify(&parse_entry(SUPPRESSED.as_bytes()).unwrap()),
            Item::Suppressed {
                unit: "flood.service".into(),
                count: 9873
            }
        );
        assert_eq!(classify(&parse_entry(OWN.as_bytes()).unwrap()), Item::Own);
        assert!(parse_entry(b"not json").is_none());
    }

    #[test]
    fn resumes_across_boots() {
        let a = args(Some("s=6b7a;i=1a2;b=9f;m=2d6e;t=65e5c0b2a8f04;x=1"));
        assert!(a.contains(&"--cursor=s=6b7a;i=1a2;b=9f;m=2d6e;t=65e5c0b2a8f04;x=1".into()));
        assert!(a.contains(&"--boot=all".into()), "{a:?}");
        let a = args(None);
        assert!(a.contains(&"--boot".into()) && !a.iter().any(|x| x.starts_with("--cursor")));
    }

    #[test]
    fn cursor_time() {
        assert_eq!(
            cursor_realtime_us("s=6b7a;i=1a2;b=9f;m=2d6e;t=65e5c0b2a8f04;x=1"),
            Some(0x65e5c0b2a8f04)
        );
        assert_eq!(cursor_realtime_us("s=1"), None);
    }
}
