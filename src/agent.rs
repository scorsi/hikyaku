//! `hikyaku run`: the sources of ADR 0003 and the senders of ADR 0005 on OS threads around one
//! spool (ADR 0002, 0004), the agent's own events (stderr for its journal, the logs stream for
//! the store) and `/metrics`.

use crate::config::{Config, Target};
use crate::files::Tail;
use crate::journald::{self, Item};
use crate::metrics::{self, Metrics};
use crate::record::{PendingLog, Scrape, ScrapeResult};
use crate::spool::{Commit, FilePos, Pending, Signal, Spool, State};
use crate::{deliver, scrape, ts};
use serde_json::Value;
use std::io::{self, BufRead, BufReader};
use std::net::TcpListener;
use std::os::unix::net::UnixDatagram;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const HEARTBEAT: Duration = Duration::from_secs(300);
const TICK: Duration = Duration::from_secs(10);
/// journald and file lines are committed in batches: up to this many, or whatever arrived
/// within the wait.
const BATCH: usize = 1000;
const BATCH_WAIT: Duration = Duration::from_millis(100);
const RETRY: Duration = Duration::from_secs(5);

pub struct Agent {
    pub cfg: Config,
    pub spool: Mutex<Spool>,
    pub metrics: Metrics,
}

impl Agent {
    /// An own event: the agent's journal (stderr) and the logs stream, from the reserve.
    pub fn event(&self, level: &str, event: &str, fields: &[(&str, Value)]) {
        let p = PendingLog::event(level, event, fields);
        eprintln!("{}", p.line);
        let result =
            self.spool
                .lock()
                .unwrap()
                .commit(Signal::Logs, &[Pending::Log(p)], true, |_| {});
        self.after(result);
    }

    /// An ingestion commit; `positions` records the source position the records came from.
    pub fn commit(&self, signal: Signal, items: &[Pending], positions: impl FnOnce(&mut State)) {
        let result = self
            .spool
            .lock()
            .unwrap()
            .commit(signal, items, false, positions);
        self.after(result);
    }

    /// Announces what a commit changed. A spool I/O error is fatal: the state on disk is
    /// consistent, and systemd restarts the agent (ADR 0004: never lose data silently).
    pub fn after(&self, result: io::Result<Commit>) {
        let commit = match result {
            Ok(c) => c,
            Err(e) => {
                let p = PendingLog::event("error", "spool-error", &[("err", e.to_string().into())]);
                eprintln!("{}", p.line);
                std::process::exit(1);
            }
        };
        let total = self.spool.lock().unwrap().snapshot().total();
        for (signal, paused) in commit.transitions {
            let (level, event) = if paused {
                ("warn", "spool-full")
            } else {
                ("info", "spool-resumed")
            };
            self.event(
                level,
                event,
                &[
                    ("signal", signal.name().into()),
                    ("spool_bytes", total.into()),
                ],
            );
        }
        for (reason, _) in commit.bugs {
            self.metrics.inc("hikyaku_record_bugs_total", &[]);
            self.event("error", "record-invalid", &[("err", reason.into())]);
        }
    }
}

pub fn run(cfg: Config) -> Result<(), String> {
    let (spool, transitions) = Spool::open(&cfg.spool, &cfg.node, cfg.spool_limit)
        .map_err(|e| format!("spool {}: {e}", cfg.spool.display()))?;
    let fresh = spool.fresh;
    let listener = TcpListener::bind(cfg.listen_metrics)
        .map_err(|e| format!("listen_metrics {}: {e}", cfg.listen_metrics))?;
    let agent = Arc::new(Agent {
        cfg,
        spool: Mutex::new(spool),
        metrics: Metrics::default(),
    });
    agent.event(
        "info",
        "agent-start",
        &[
            ("version", env!("CARGO_PKG_VERSION").into()),
            ("fresh_spool", fresh.into()),
        ],
    );
    agent.after(Ok(Commit {
        transitions,
        ..Commit::default()
    }));

    let a = agent.clone();
    thread::spawn(move || {
        metrics::serve(listener, || {
            a.metrics.render(&a.spool.lock().unwrap().snapshot())
        })
    });
    if agent.cfg.journald {
        let a = agent.clone();
        thread::spawn(move || journald_loop(&a));
    }
    for (name, f) in &agent.cfg.files {
        let (a, name, path) = (agent.clone(), name.clone(), f.path.clone());
        thread::spawn(move || file_loop(&a, &name, path));
    }
    let mut targets = agent.cfg.targets.clone();
    // The agent scrapes its own /metrics as target `hikyaku` (ADR 0003).
    targets.entry("hikyaku".into()).or_insert_with(|| Target {
        url: format!("http://{}/metrics", agent.cfg.listen_metrics),
        interval: Duration::from_secs(30),
        max_series: 2000,
    });
    for (name, target) in targets {
        let a = agent.clone();
        thread::spawn(move || scrape_loop(&a, &name, &target));
    }
    // One sender per configured destination (ADR 0005): logs to nisshi, metrics to masu.
    for signal in Signal::ALL {
        if agent.cfg.destination(signal).is_some() {
            let a = agent.clone();
            thread::spawn(move || deliver::run(&a, signal));
        }
    }
    notify_ready();

    let mut last_heartbeat = Instant::now();
    loop {
        thread::sleep(TICK);
        let aged = agent.spool.lock().unwrap().close_aged();
        agent.after(aged.map(|transitions| Commit {
            transitions,
            ..Commit::default()
        }));
        if last_heartbeat.elapsed() >= HEARTBEAT {
            last_heartbeat = Instant::now();
            let snap = agent.spool.lock().unwrap().snapshot();
            agent.event(
                "info",
                "heartbeat",
                &[
                    ("spool_bytes", snap.total().into()),
                    ("unacked", snap.unacked.iter().sum::<u64>().into()),
                ],
            );
        }
    }
}

/// `Type=notify`: READY=1 once every source thread is started.
fn notify_ready() {
    if let Some(path) = std::env::var_os("NOTIFY_SOCKET")
        && let Ok(sock) = UnixDatagram::unbound()
    {
        let _ = sock.send_to(b"READY=1", path);
    }
}

/// Keeps a `journalctl --follow` child running from the saved cursor; batches its entries into
/// the spool with the cursor of the last one. While the spool is paused nothing is read: the
/// pipe fills and journalctl blocks, the journal keeps the lines (ADR 0004).
fn journald_loop(agent: &Agent) {
    loop {
        let cursor = agent.spool.lock().unwrap().state.journal_cursor.clone();
        let mut child = match journald::spawn(cursor.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                agent.event("error", "journald-error", &[("err", e.to_string().into())]);
                thread::sleep(RETRY);
                continue;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = sync_channel::<journald::Entry>(4 * BATCH);
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if let Some(e) = journald::parse_entry(&line)
                            && tx.send(e).is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });

        // With a saved cursor, the first entry is the one already spooled: anything else means
        // journald rotated past the cursor while the agent was paused or stopped.
        let mut first = cursor.is_some();
        let mut batch: Vec<Pending> = Vec::new();
        let mut batch_bytes = 0;
        let mut last_cursor: Option<String> = None;
        loop {
            while agent.spool.lock().unwrap().is_paused(Signal::Logs) {
                thread::sleep(Duration::from_secs(1));
            }
            let room = agent.spool.lock().unwrap().room();
            let entry = match rx.recv_timeout(BATCH_WAIT) {
                Ok(e) => Some(e),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let pushed = batch.len();
            if let Some(e) = entry.as_ref() {
                if std::mem::take(&mut first) {
                    if Some(&e.cursor) == cursor.as_ref() {
                        continue;
                    }
                    let from = cursor.as_deref().and_then(journald::cursor_realtime_us);
                    agent.metrics.inc("hikyaku_journal_gaps_total", &[]);
                    batch.push(own(
                        "warn",
                        "journal-gap",
                        &[
                            (
                                "from_ts",
                                from.map_or(Value::Null, |us| ts::from_micros(us).into()),
                            ),
                            ("to_ts", ts::from_micros(e.realtime_us).into()),
                        ],
                    ));
                }
                match journald::classify(e) {
                    Item::Own => {}
                    Item::Suppressed { unit, count } => {
                        agent.metrics.add(
                            "hikyaku_journald_suppressed_lines_total",
                            &[("unit", &unit)],
                            count,
                        );
                        batch.push(own(
                            "warn",
                            "journald-suppressed",
                            &[("unit", unit.into()), ("count", count.into())],
                        ));
                    }
                    Item::Record(p) => batch.push(Pending::Log(p)),
                }
                last_cursor = Some(e.cursor.clone());
            }
            batch_bytes += estimate(agent, &batch[pushed..]);
            if last_cursor.is_some()
                && (entry.is_none() || batch.len() >= BATCH || batch_bytes >= room)
            {
                let cursor = last_cursor.take();
                agent.commit(Signal::Logs, &batch, |st| st.journal_cursor = cursor);
                batch.clear();
                batch_bytes = 0;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = reader.join();
        agent.event("error", "journald-exited", &[]);
        thread::sleep(RETRY);
    }
}

/// The spool bytes of `items` once rendered: sources compare it with `Spool::room`.
fn estimate(agent: &Agent, items: &[Pending]) -> u64 {
    let spool = agent.spool.lock().unwrap();
    items
        .iter()
        .map(|p| match p {
            Pending::Log(l) => spool.estimate(l),
            Pending::Scrape(_) => 0,
        })
        .sum()
}

/// An own event that travels inside a source batch (so it keeps its place in the stream).
fn own(level: &str, event: &str, fields: &[(&str, Value)]) -> Pending {
    let p = PendingLog::event(level, event, fields);
    eprintln!("{}", p.line);
    Pending::Log(p)
}

/// Tails one declared file (ADR 0003, macOS), its inode and offset saved with each commit.
fn file_loop(agent: &Agent, name: &str, path: std::path::PathBuf) {
    let saved = agent.spool.lock().unwrap().state.files.get(name).cloned();
    let saved = saved.filter(|p| p.path == path.to_string_lossy());
    let mut tail = Tail {
        path: path.clone(),
        inode: saved.as_ref().map_or(0, |p| p.inode),
        offset: saved.as_ref().map_or(0, |p| p.offset),
    };
    loop {
        if agent.spool.lock().unwrap().is_paused(Signal::Logs) {
            thread::sleep(Duration::from_secs(1));
            continue;
        }
        let polled = match tail.poll(BATCH) {
            Ok(p) => p,
            Err(e) => {
                agent.event(
                    "error",
                    "file-error",
                    &[
                        ("path", path.to_string_lossy().into()),
                        ("err", e.to_string().into()),
                    ],
                );
                thread::sleep(RETRY);
                continue;
            }
        };
        let mut batch = Vec::new();
        if polled.truncated {
            batch.push(own(
                "warn",
                "file-truncated",
                &[("path", path.to_string_lossy().into())],
            ));
        }
        let now = ts::now();
        let room = agent.spool.lock().unwrap().room();
        let mut bytes = estimate(agent, &batch);
        let mut taken = 0;
        for line in &polled.lines {
            if bytes >= room {
                break;
            }
            let p = PendingLog::from_source(line, &now, "info", name, "file");
            bytes += agent.spool.lock().unwrap().estimate(&p);
            batch.push(Pending::Log(p));
            taken += 1;
        }
        // Lines beyond the room are read again after the pause (their offset is not saved).
        let rest: u64 = polled.lines[taken..]
            .iter()
            .map(|l| l.len() as u64 + 1)
            .sum();
        tail.offset -= rest;
        let full = polled.lines.len() >= BATCH || rest > 0;
        if !batch.is_empty() {
            let pos = FilePos {
                path: path.to_string_lossy().into_owned(),
                inode: tail.inode,
                offset: tail.offset,
            };
            agent.commit(Signal::Logs, &batch, |st| {
                st.files.insert(name.to_string(), pos);
            });
        }
        if !full {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

/// Scrapes one target every interval; while the spool is paused, one `skipped-spool-full`
/// record when the pause starts, then nothing (ADR 0004).
fn scrape_loop(agent: &Agent, name: &str, target: &Target) {
    let http = scrape::agent();
    let mut was_paused = false;
    loop {
        let start = Instant::now();
        let paused = agent.spool.lock().unwrap().is_paused(Signal::Metrics);
        if paused && !was_paused {
            let skipped = Scrape {
                ts: ts::now(),
                service: name.to_string(),
                dur_us: 0,
                result: ScrapeResult::Down {
                    error: "skipped-spool-full".into(),
                },
            };
            agent.metrics.inc(
                "hikyaku_scrapes_total",
                &[("target", name), ("result", "skipped")],
            );
            let result = agent.spool.lock().unwrap().commit(
                Signal::Metrics,
                &[Pending::Scrape(skipped)],
                true,
                |_| {},
            );
            agent.after(result);
        } else if !paused {
            let (record, label) = scrape::scrape(&http, name, &target.url, target.max_series);
            agent.metrics.inc(
                "hikyaku_scrapes_total",
                &[("target", name), ("result", label)],
            );
            agent.commit(Signal::Metrics, &[Pending::Scrape(record)], |_| {});
        }
        was_paused = paused;
        thread::sleep(target.interval.saturating_sub(start.elapsed()));
    }
}
