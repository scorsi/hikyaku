//! The agent's own `/metrics` on loopback (masu's metric convention): counters kept here,
//! gauges read from the spool at render time, served by a few lines over `TcpListener`.

use crate::spool::{Signal, Snapshot};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;
use std::time::Duration;

/// `(name, type, help)` of every family, in exposition order.
const FAMILIES: &[(&str, &str, &str)] = &[
    ("hikyaku_build_info", "gauge", "Version of the agent."),
    (
        "hikyaku_spool_bytes",
        "gauge",
        "Bytes spooled per signal, closed segments included.",
    ),
    (
        "hikyaku_spool_limit_bytes",
        "gauge",
        "Configured spool limit, both signals together.",
    ),
    (
        "hikyaku_spool_paused",
        "gauge",
        "1 while ingestion of the signal is paused (spool full).",
    ),
    (
        "hikyaku_unacked_records",
        "gauge",
        "Records spooled and not yet acked by the store.",
    ),
    (
        "hikyaku_scrapes_total",
        "counter",
        "Scrape attempts per target and result.",
    ),
    (
        "hikyaku_journald_suppressed_lines_total",
        "counter",
        "Lines journald dropped under its rate limit, per unit.",
    ),
    (
        "hikyaku_journal_gaps_total",
        "counter",
        "Saved journal cursors that journald no longer had.",
    ),
    (
        "hikyaku_record_bugs_total",
        "counter",
        "Records the agent built that its own validation refused.",
    ),
];

#[derive(Default)]
pub struct Metrics {
    counters: Mutex<BTreeMap<String, u64>>,
}

impl Metrics {
    pub fn add(&self, name: &str, labels: &[(&str, &str)], n: u64) {
        let key = sample(name, labels);
        *self.counters.lock().unwrap().entry(key).or_default() += n;
    }

    pub fn inc(&self, name: &str, labels: &[(&str, &str)]) {
        self.add(name, labels, 1);
    }

    pub fn render(&self, snap: &Snapshot) -> String {
        let mut gauges: BTreeMap<String, u64> = BTreeMap::new();
        gauges.insert(
            sample(
                "hikyaku_build_info",
                &[("version", env!("CARGO_PKG_VERSION"))],
            ),
            1,
        );
        gauges.insert(sample("hikyaku_spool_limit_bytes", &[]), snap.limit);
        for s in Signal::ALL {
            let l = [("signal", s.name())];
            gauges.insert(sample("hikyaku_spool_bytes", &l), snap.bytes[s as usize]);
            gauges.insert(
                sample("hikyaku_spool_paused", &l),
                snap.paused[s as usize] as u64,
            );
            gauges.insert(
                sample("hikyaku_unacked_records", &l),
                snap.unacked[s as usize],
            );
        }
        let counters = self.counters.lock().unwrap();
        let mut out = String::new();
        for (name, kind, help) in FAMILIES {
            out.push_str(&format!("# HELP {name} {help}\n# TYPE {name} {kind}\n"));
            let table = if *kind == "gauge" { &gauges } else { &counters };
            for (key, v) in table.range(name.to_string()..) {
                if key != name && !key.starts_with(&format!("{name}{{")) {
                    break;
                }
                out.push_str(&format!("{key} {v}\n"));
            }
            if *kind == "counter"
                && !counters
                    .keys()
                    .any(|k| k == name || k.starts_with(&format!("{name}{{")))
            {
                // A counter with labels has no sample until its first increment; one without
                // labels is always exposed.
                if ![
                    "hikyaku_scrapes_total",
                    "hikyaku_journald_suppressed_lines_total",
                ]
                .contains(name)
                {
                    out.push_str(&format!("{name} 0\n"));
                }
            }
        }
        out
    }
}

/// `name{a="x",b="y"}` with the convention's escaping of label values.
fn sample(name: &str, labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return name.to_string();
    }
    let labels: Vec<String> = labels
        .iter()
        .map(|(k, v)| {
            let v = v
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n");
            format!("{k}=\"{v}\"")
        })
        .collect();
    format!("{name}{{{}}}", labels.join(","))
}

/// Answers every request on `listener` with the exposition `render` returns.
pub fn serve(listener: TcpListener, render: impl Fn() -> String) {
    for conn in listener.incoming() {
        let Ok(mut conn) = conn else { continue };
        let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = conn.set_write_timeout(Some(Duration::from_secs(5)));
        // Read the request head; whatever it asks for, the answer is the exposition.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while head.len() < 8192 && !head.ends_with(b"\r\n\r\n") {
            match conn.read(&mut byte) {
                Ok(1) => head.push(byte[0]),
                _ => break,
            }
        }
        let body = render();
        let _ = conn.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_families_in_order() {
        let m = Metrics::default();
        m.inc(
            "hikyaku_scrapes_total",
            &[("target", "annai"), ("result", "ok")],
        );
        m.add(
            "hikyaku_journald_suppressed_lines_total",
            &[("unit", "a\"b")],
            3,
        );
        let snap = Snapshot {
            limit: 100,
            bytes: [10, 20],
            paused: [true, false],
            unacked: [1, 2],
        };
        let out = m.render(&snap);
        assert!(out.contains("# TYPE hikyaku_scrapes_total counter\nhikyaku_scrapes_total{target=\"annai\",result=\"ok\"} 1\n"));
        assert!(out.contains("hikyaku_journald_suppressed_lines_total{unit=\"a\\\"b\"} 3\n"));
        assert!(out.contains("hikyaku_spool_paused{signal=\"logs\"} 1\n"));
        assert!(out.contains("hikyaku_spool_paused{signal=\"metrics\"} 0\n"));
        assert!(out.contains("hikyaku_spool_limit_bytes 100\n"));
        assert!(out.contains("hikyaku_journal_gaps_total 0\n"));
        assert!(out.ends_with('\n'));
        let types = out.matches("# TYPE").count();
        assert_eq!(types, FAMILIES.len());
    }
}
