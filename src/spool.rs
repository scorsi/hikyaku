//! The durable spool (ADR 0004, docs/spool.md): append-only NDJSON segments per signal, an
//! atomic `state.json`, segment close into zstd, and the full-spool policy. The spool is the
//! only place that assigns `seq`, so it also runs the stores' validation on every record.

use crate::logschema;
use crate::record::{PendingLog, Scrape, ScrapeResult, wrap};
use crate::scrape_record;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A segment closes at 16 MiB or 1 hour.
pub const SEGMENT_BYTES: u64 = 16 << 20;
pub const SEGMENT_AGE: Duration = Duration::from_secs(3600);
/// Closed segments are compressed whole (ADR 0002: ≈ 20 MB of encoder memory).
const ZSTD_LEVEL: i32 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Logs,
    Metrics,
}

impl Signal {
    pub const ALL: [Signal; 2] = [Signal::Logs, Signal::Metrics];

    pub fn name(self) -> &'static str {
        match self {
            Signal::Logs => "logs",
            Signal::Metrics => "metrics",
        }
    }

    pub fn parse(s: &str) -> Option<Signal> {
        Signal::ALL.into_iter().find(|x| x.name() == s)
    }

    fn i(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamState {
    pub stream: String,
    pub next_seq: u64,
    pub acked_seq: u64,
    pub open_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FilePos {
    pub path: String,
    pub inode: u64,
    pub offset: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub v: u32,
    pub streams: BTreeMap<String, StreamState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_cursor: Option<String>,
    #[serde(default)]
    pub files: BTreeMap<String, FilePos>,
    #[serde(default)]
    pub paused: BTreeMap<String, bool>,
    #[serde(default)]
    pub replacements: BTreeMap<String, BTreeMap<String, String>>,
}

impl State {
    pub fn stream(&self, s: Signal) -> &StreamState {
        &self.streams[s.name()]
    }
    fn stream_mut(&mut self, s: Signal) -> &mut StreamState {
        self.streams.get_mut(s.name()).expect("stream state")
    }
}

/// A record waiting for its `stream` and `seq`.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    Log(PendingLog),
    Scrape(Scrape),
}

impl Pending {
    fn render(&self, node: &str, stream: &str, seq: u64) -> String {
        match self {
            Pending::Log(l) => l.render(node, stream, seq),
            Pending::Scrape(s) => s.render(node, stream, seq),
        }
    }

    fn check(&self, record: &str) -> Result<(), String> {
        // Stricter than the stores: a refused record must fit in its replacement.
        if record.len() > crate::record::MAX_REPLACEABLE_BYTES {
            return Err("record-too-large".into());
        }
        match self {
            Pending::Log(_) => logschema::check_record(record.as_bytes()),
            Pending::Scrape(_) => scrape_record::check(record.as_bytes()),
        }
    }

    /// A record the agent built and its own validation refuses is an agent bug: it is kept,
    /// wrapped as `unparsed` (logs) or as a failed scrape (metrics), never dropped (ADR 0003).
    fn fallback(&self, record: &str, reason: &str) -> Pending {
        match self {
            Pending::Log(l) => Pending::Log(PendingLog {
                line: wrap(
                    &crate::ts::now(),
                    "error",
                    "unparsed",
                    Some(reason),
                    record.as_bytes(),
                ),
                // The service is kept unless it is itself the refused field.
                service: Some(l.service.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "hikyaku".into()),
                source: l.source,
            }),
            Pending::Scrape(s) => Pending::Scrape(Scrape {
                result: ScrapeResult::Down {
                    error: format!("invalid-record:{reason}"),
                },
                ..s.clone()
            }),
        }
    }
}

/// What a commit changed besides the data: pause transitions to announce, agent bugs to log.
#[derive(Debug, Default, PartialEq)]
pub struct Commit {
    /// False when ingestion was refused (spool paused): nothing written, positions unchanged.
    pub admitted: bool,
    pub first_seq: u64,
    pub count: u64,
    /// `(signal, paused)` for every signal whose pause state flipped.
    pub transitions: Vec<(Signal, bool)>,
    /// `(reason, record)` for every record replaced by its fallback.
    pub bugs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub limit: u64,
    pub bytes: [u64; 2],
    pub paused: [bool; 2],
    pub unacked: [u64; 2],
}

impl Snapshot {
    pub fn total(&self) -> u64 {
        self.bytes.iter().sum()
    }
}

pub struct Spool {
    dir: PathBuf,
    node: String,
    pub limit: u64,
    pub segment_bytes: u64,
    pub state: State,
    pub fresh: bool,
    open: [File; 2],
    open_first_seq: [u64; 2],
    open_since: [Instant; 2],
    closed_bytes: [u64; 2],
    /// The spool directory, `flock`ed for the life of the spool: one agent per spool.
    _lock: File,
    /// Where the sender's last read stopped inside a closed segment, so the next batch goes on
    /// decompressing from there instead of from the segment's start.
    drain: [Option<Cursor>; 2],
}

/// A position in a segment: the reader, the seq of its next line, a line read but not taken.
struct Cursor {
    segment: u64,
    seq: u64,
    pending: Option<Vec<u8>>,
    reader: Box<dyn BufRead + Send>,
}

impl Spool {
    /// Opens or creates the spool, recovers from an interrupted commit or close (spool.md),
    /// and re-evaluates the pause with this `limit`.
    pub fn open(dir: &Path, node: &str, limit: u64) -> io::Result<(Spool, Vec<(Signal, bool)>)> {
        for s in Signal::ALL {
            fs::create_dir_all(dir.join(s.name()))?;
        }
        // Before any recovery: a second agent on this spool would truncate the first one's
        // appends that are not committed yet.
        let lock = File::open(dir)?;
        lock.try_lock().map_err(|e| match e {
            fs::TryLockError::WouldBlock => io::Error::new(
                io::ErrorKind::WouldBlock,
                "locked by another hikyaku process",
            ),
            fs::TryLockError::Error(e) => e,
        })?;
        let _ = fs::remove_file(dir.join("state.json.tmp"));
        let (state, fresh) = match fs::read(dir.join("state.json")) {
            Ok(bytes) => (
                serde_json::from_slice(&bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
                false,
            ),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (fresh_state()?, true),
            Err(e) => return Err(e),
        };
        let open_file = |s: Signal| {
            OpenOptions::new()
                .read(true)
                .append(true)
                .create(true)
                .open(dir.join(s.name()).join("open.ndjson"))
        };
        let mut spool = Spool {
            dir: dir.to_path_buf(),
            node: node.to_string(),
            limit,
            segment_bytes: SEGMENT_BYTES,
            state,
            fresh,
            open: [open_file(Signal::Logs)?, open_file(Signal::Metrics)?],
            open_first_seq: [0; 2],
            open_since: [Instant::now(); 2],
            closed_bytes: [0; 2],
            _lock: lock,
            drain: [None, None],
        };
        // The open segments (and their directories) may have just been created: their entries
        // must be durable before a state that counts on them is.
        for s in Signal::ALL {
            File::open(dir.join(s.name()))?.sync_all()?;
        }
        File::open(dir)?.sync_all()?;
        for s in Signal::ALL {
            spool.recover(s)?;
        }
        let transitions = spool.evaluate_pause();
        spool.write_state()?;
        Ok((spool, transitions))
    }

    fn recover(&mut self, s: Signal) -> io::Result<()> {
        // 1. A close that was interrupted before its rename left a .tmp; a crash between the
        //    ack's state write and the deletion left fully acked closed segments.
        let dir = self.dir.join(s.name());
        let st = self.state.stream(s).clone();
        let mut closed_bytes = 0;
        let mut segments = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                fs::remove_file(entry.path())?;
            } else if let Some(range) = segment_range(&name) {
                if range.1 <= st.acked_seq {
                    fs::remove_file(entry.path())?;
                    continue;
                }
                closed_bytes += entry.metadata()?.len();
                segments.push(range);
            }
        }
        self.closed_bytes[s.i()] = closed_bytes;
        // 2. The open segment ends at the committed length; the rest was never acknowledged.
        //    Shorter is only legitimate once its records are in a closed segment (close) or
        //    acked (ADR 0012): anything else lost committed records, which must not be
        //    accepted silently.
        let committed = st.open_bytes;
        let len = self.open[s.i()].metadata()?.len();
        let all_acked = st.acked_seq + 1 >= st.next_seq;
        let closed = segments.iter().any(|&(_, b)| b + 1 == st.next_seq);
        if len < committed && !all_acked && !closed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}/open.ndjson: {len} bytes, {committed} committed: records lost",
                    s.name()
                ),
            ));
        }
        // A fully acked open segment is emptied (ADR 0012), also when the crash came between
        // the ack's state write and the truncation.
        let keep = if all_acked { 0 } else { len.min(committed) };
        if len != keep {
            self.open[s.i()].set_len(keep)?;
            self.open[s.i()].sync_all()?;
        }
        self.state.stream_mut(s).open_bytes = keep;
        // 3. After a close's rename, a closed segment covers the open segment's first seq.
        let first = self.first_open_seq(s)?;
        if segments.iter().any(|&(a, b)| a <= first && first <= b) {
            self.open[s.i()].set_len(0)?;
            self.open[s.i()].sync_all()?;
            self.state.stream_mut(s).open_bytes = 0;
        }
        self.open_first_seq[s.i()] = self.first_open_seq(s)?;
        Ok(())
    }

    /// The `seq` of the open segment's first line, or `next_seq` when it is empty.
    fn first_open_seq(&mut self, s: Signal) -> io::Result<u64> {
        let open_bytes = self.state.stream(s).open_bytes;
        if open_bytes == 0 {
            return Ok(self.state.stream(s).next_seq);
        }
        let mut reader = BufReader::new(File::open(self.dir.join(s.name()).join("open.ndjson"))?);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        Ok(seq_of(&line).unwrap_or(self.state.stream(s).next_seq))
    }

    pub fn is_paused(&self, s: Signal) -> bool {
        self.state.paused.get(s.name()).copied().unwrap_or(false)
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut snap = Snapshot {
            limit: self.limit,
            bytes: [0; 2],
            paused: [false; 2],
            unacked: [0; 2],
        };
        for s in Signal::ALL {
            let st = self.state.stream(s);
            // Replacements live in state.json but count against the limit like spooled bytes.
            let replaced: usize = self
                .state
                .replacements
                .get(s.name())
                .map_or(0, |r| r.values().map(String::len).sum());
            snap.bytes[s.i()] = self.closed_bytes[s.i()] + st.open_bytes + replaced as u64;
            snap.paused[s.i()] = self.is_paused(s);
            snap.unacked[s.i()] = st.next_seq - 1 - st.acked_seq;
        }
        snap
    }

    /// Bytes ingestion may still add before it pauses (99 % of the limit). Sources size their
    /// batches against it, so a batch overshoots by at most one record and the last 1 % stays
    /// for own events and `skipped-spool-full` records (ADR 0004).
    pub fn room(&self) -> u64 {
        (self.limit - self.limit / 100).saturating_sub(self.snapshot().total())
    }

    /// The bytes `p` takes in the spool once rendered (agent fields and newline included).
    pub fn estimate(&self, p: &PendingLog) -> u64 {
        p.render(&self.node, &"0".repeat(32), u64::MAX).len() as u64 + 1
    }

    /// Appends the records (validated, `seq` assigned), fsyncs, then commits the state with
    /// `positions` applied (cursor, file offsets). `reserve` is for the agent's own events: they
    /// may use the last 1 % of the limit, but nothing beyond it.
    pub fn commit(
        &mut self,
        s: Signal,
        items: &[Pending],
        reserve: bool,
        positions: impl FnOnce(&mut State),
    ) -> io::Result<Commit> {
        let mut out = Commit {
            first_seq: self.state.stream(s).next_seq,
            ..Commit::default()
        };
        // Admission is decided here, under the spool's lock: a source that checked the room
        // earlier may find the spool paused by another one since (ADR 0004).
        if !reserve && self.is_paused(s) {
            return Ok(out);
        }
        out.admitted = true;
        let over_limit = self.snapshot().total() >= self.limit;
        let mut buf = Vec::new();
        // Where the last record starts in `buf`.
        let mut last = 0;
        if !(reserve && over_limit) {
            let stream = self.state.stream(s).stream.clone();
            let mut seq = out.first_seq;
            for item in items {
                last = buf.len();
                let mut record = item.render(&self.node, &stream, seq);
                if let Err(reason) = item.check(&record) {
                    let fallback = item
                        .fallback(&record, &reason)
                        .render(&self.node, &stream, seq);
                    out.bugs
                        .push((reason, std::mem::replace(&mut record, fallback)));
                }
                buf.extend_from_slice(record.as_bytes());
                buf.push(b'\n');
                seq += 1;
            }
            out.count = seq - out.first_seq;
        }
        // Against the room as accounted now: a batch sized against an older room (another
        // source committed since) would eat the reserve. It may overshoot by its last record
        // only, as the sources size it; beyond, it pauses ingestion as it would once written,
        // without being written (ADR 0004).
        if !reserve && last as u64 >= self.room() {
            let mut refused = Commit {
                first_seq: out.first_seq,
                ..Commit::default()
            };
            for s in Signal::ALL {
                if !self.is_paused(s) {
                    self.state.paused.insert(s.name().into(), true);
                    refused.transitions.push((s, true));
                }
            }
            if !refused.transitions.is_empty() {
                self.write_state()?;
            }
            return Ok(refused);
        }
        if !buf.is_empty() {
            self.open[s.i()].write_all(&buf)?;
            self.open[s.i()].sync_data()?;
            let st = self.state.stream_mut(s);
            st.next_seq += out.count;
            st.open_bytes += buf.len() as u64;
        }
        positions(&mut self.state);
        self.write_state()?;
        if self.state.stream(s).open_bytes >= self.segment_bytes
            || self.open_since[s.i()].elapsed() >= SEGMENT_AGE
        {
            self.close(s)?;
        }
        out.transitions = self.evaluate_pause();
        if !out.transitions.is_empty() {
            self.write_state()?;
        }
        Ok(out)
    }

    /// Closes every open segment older than an hour (the ticker calls it).
    pub fn close_aged(&mut self) -> io::Result<Vec<(Signal, bool)>> {
        for s in Signal::ALL {
            if self.state.stream(s).open_bytes > 0
                && self.open_since[s.i()].elapsed() >= SEGMENT_AGE
            {
                self.close(s)?;
            }
        }
        let transitions = self.evaluate_pause();
        if !transitions.is_empty() {
            self.write_state()?;
        }
        Ok(transitions)
    }

    /// Compresses the open segment into `<first>-<last>.ndjson.zst`, then empties it.
    pub fn close(&mut self, s: Signal) -> io::Result<()> {
        let st = self.state.stream(s).clone();
        if st.open_bytes == 0 {
            return Ok(());
        }
        let dir = self.dir.join(s.name());
        let name = format!(
            "{:020}-{:020}.ndjson.zst",
            self.open_first_seq[s.i()],
            st.next_seq - 1
        );
        let tmp = dir.join(format!("{name}.tmp"));
        let mut encoder = zstd::Encoder::new(File::create(&tmp)?, ZSTD_LEVEL)?;
        let mut src = File::open(dir.join("open.ndjson"))?;
        src.rewind()?;
        io::copy(&mut src.take(st.open_bytes), &mut encoder)?;
        let file = encoder.finish()?;
        file.sync_all()?;
        fs::rename(&tmp, dir.join(&name))?;
        File::open(&dir)?.sync_all()?;
        self.closed_bytes[s.i()] += fs::metadata(dir.join(&name))?.len();
        self.open[s.i()].set_len(0)?;
        self.open[s.i()].sync_all()?;
        self.state.stream_mut(s).open_bytes = 0;
        self.open_first_seq[s.i()] = st.next_seq;
        self.open_since[s.i()] = Instant::now();
        self.write_state()
    }

    /// Up to `max_records` spooled records from `from` on, in seq order, stopping before
    /// `max_bytes` is crossed (at least one record). The bytes as spooled, without newline;
    /// replacements are applied by the sender.
    pub fn read(
        &mut self,
        s: Signal,
        from: u64,
        max_bytes: usize,
        max_records: usize,
    ) -> io::Result<Vec<Vec<u8>>> {
        let dir = self.dir.join(s.name());
        let mut segments: Vec<(u64, u64)> = fs::read_dir(&dir)?
            .filter_map(|e| segment_range(&e.ok()?.file_name().to_string_lossy()))
            .filter(|&(_, b)| b >= from)
            .collect();
        segments.sort();
        let mut batch = Batch {
            from,
            max_bytes,
            max_records,
            out: Vec::new(),
            bytes: 0,
        };
        for (a, b) in segments {
            let mut cur = match self.drain[s.i()].take() {
                Some(c) if c.segment == a && c.seq <= from => c,
                _ => {
                    let name = format!("{a:020}-{b:020}.ndjson.zst");
                    let reader = zstd::Decoder::new(File::open(dir.join(name))?)?;
                    Cursor {
                        segment: a,
                        seq: a,
                        pending: None,
                        reader: Box::new(BufReader::new(reader)),
                    }
                }
            };
            if !batch.take(&mut cur)? {
                self.drain[s.i()] = Some(cur);
                return Ok(batch.out);
            }
        }
        // The open segment is plain and may be emptied by a close: read afresh each time.
        let open = File::open(dir.join("open.ndjson"))?;
        let mut cur = Cursor {
            segment: u64::MAX,
            seq: self.open_first_seq[s.i()],
            pending: None,
            reader: Box::new(BufReader::new(open.take(self.state.stream(s).open_bytes))),
        };
        batch.take(&mut cur)?;
        Ok(batch.out)
    }

    /// The store committed everything up to `acked`: the state first, then closed segments
    /// fully acked are deleted, a fully acked open segment is emptied (ADR 0012) and
    /// replacements up to `acked` dropped (ADR 0005, spool.md).
    pub fn ack(&mut self, s: Signal, acked: u64) -> io::Result<Vec<(Signal, bool)>> {
        if acked <= self.state.stream(s).acked_seq {
            return Ok(Vec::new());
        }
        self.state.stream_mut(s).acked_seq = acked;
        if let Some(r) = self.state.replacements.get_mut(s.name()) {
            r.retain(|seq, _| seq.parse::<u64>().is_ok_and(|n| n > acked));
        }
        self.write_state()?;
        let dir = self.dir.join(s.name());
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if segment_range(&name).is_some_and(|(_, b)| b <= acked) {
                let len = entry.metadata()?.len();
                fs::remove_file(entry.path())?;
                self.closed_bytes[s.i()] -= len.min(self.closed_bytes[s.i()]);
            }
        }
        // Closing it would give a closed segment deleted at once: empty it in place instead.
        // A crash before the state write is recovered by the truncation to `open_bytes`.
        let st = self.state.stream(s);
        let emptied = st.open_bytes > 0 && acked + 1 >= st.next_seq;
        if emptied {
            let next_seq = st.next_seq;
            self.open[s.i()].set_len(0)?;
            self.open[s.i()].sync_all()?;
            self.state.stream_mut(s).open_bytes = 0;
            self.open_first_seq[s.i()] = next_seq;
            self.open_since[s.i()] = Instant::now();
        }
        let transitions = self.evaluate_pause();
        if emptied || !transitions.is_empty() {
            self.write_state()?;
        }
        Ok(transitions)
    }

    /// The record sent in place of the spooled one at `seq`, committed before the resend.
    pub fn replacement(&self, s: Signal, seq: u64) -> Option<&String> {
        self.state.replacements.get(s.name())?.get(&seq.to_string())
    }

    /// Commits a replacement; its bytes count against the limit, so it may pause ingestion.
    pub fn set_replacement(
        &mut self,
        s: Signal,
        seq: u64,
        record: String,
    ) -> io::Result<Vec<(Signal, bool)>> {
        self.state
            .replacements
            .entry(s.name().into())
            .or_default()
            .insert(seq.to_string(), record);
        let transitions = self.evaluate_pause();
        self.write_state()?;
        Ok(transitions)
    }

    /// Pause at 99 % of the limit (the last 1 % is for the agent's own events), resume below
    /// 90 %. The limit is shared, so both signals flip together.
    fn evaluate_pause(&mut self) -> Vec<(Signal, bool)> {
        let total = self.snapshot().total();
        let mut out = Vec::new();
        for s in Signal::ALL {
            let paused = self.is_paused(s);
            let now = if paused {
                total >= self.limit / 10 * 9
            } else {
                total >= self.limit - self.limit / 100
            };
            if now != paused {
                self.state.paused.insert(s.name().into(), now);
                out.push((s, now));
            }
        }
        out
    }

    /// `state.json.tmp`, fsync, rename, fsync the directory.
    fn write_state(&self) -> io::Result<()> {
        let tmp = self.dir.join("state.json.tmp");
        let mut f = File::create(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&self.state)?)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        fs::rename(&tmp, self.dir.join("state.json"))?;
        File::open(&self.dir)?.sync_all()
    }
}

/// What `Spool::read` collects.
struct Batch {
    from: u64,
    max_bytes: usize,
    max_records: usize,
    out: Vec<Vec<u8>>,
    bytes: usize,
}

impl Batch {
    /// Lines from the cursor on; false once the batch is full, the line that did not fit kept
    /// in the cursor for the next batch.
    fn take(&mut self, cur: &mut Cursor) -> io::Result<bool> {
        loop {
            let line = match cur.pending.take() {
                Some(line) => line,
                None => {
                    let mut line = Vec::new();
                    if cur.reader.read_until(b'\n', &mut line)? == 0 {
                        return Ok(true);
                    }
                    if line.last() == Some(&b'\n') {
                        line.pop();
                    }
                    line
                }
            };
            if cur.seq >= self.from {
                if self.out.len() >= self.max_records
                    || (!self.out.is_empty() && self.bytes + line.len() > self.max_bytes)
                {
                    cur.pending = Some(line);
                    return Ok(false);
                }
                self.bytes += line.len();
                self.out.push(line);
            }
            cur.seq += 1;
        }
    }
}

fn fresh_state() -> io::Result<State> {
    let mut state = State {
        v: 1,
        ..State::default()
    };
    for s in Signal::ALL {
        state.streams.insert(
            s.name().into(),
            StreamState {
                stream: random_stream()?,
                next_seq: 1,
                acked_seq: 0,
                open_bytes: 0,
            },
        );
        state.paused.insert(s.name().into(), false);
    }
    Ok(state)
}

/// 16 random bytes as 32 lowercase hex digits (ADR 0005).
fn random_stream() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `<first>-<last>.ndjson.zst` → `(first, last)`.
fn segment_range(name: &str) -> Option<(u64, u64)> {
    let (a, b) = name.strip_suffix(".ndjson.zst")?.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Through `json::parse`: a kept service line can hold what serde_json refuses (`1e10000`).
fn seq_of(record: &str) -> Option<u64> {
    crate::json::parse(record.as_bytes())
        .ok()?
        .get("seq")?
        .as_u64()
}

/// `hikyaku spool dump`: every spooled record as NDJSON, closed segments then the open one,
/// each followed by its replacement (`"_replaced":true`) when one exists (spool.md).
pub fn dump(
    dir: &Path,
    signal: Option<Signal>,
    from: u64,
    to: u64,
    out: &mut impl Write,
) -> io::Result<()> {
    let state: State = serde_json::from_slice(&fs::read(dir.join("state.json"))?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    for s in Signal::ALL {
        if signal.is_some_and(|x| x != s) {
            continue;
        }
        let sdir = dir.join(s.name());
        let mut segments: Vec<(String, (u64, u64))> = fs::read_dir(&sdir)?
            .filter_map(|e| {
                let name = e.ok()?.file_name().to_string_lossy().into_owned();
                let range = segment_range(&name)?;
                Some((name, range))
            })
            .collect();
        segments.sort();
        let replacements = state.replacements.get(s.name());
        let print = |line: &str, out: &mut dyn Write| -> io::Result<()> {
            let Some(seq) = seq_of(line) else {
                return Ok(());
            };
            if seq < from || seq > to {
                return Ok(());
            }
            writeln!(out, "{line}")?;
            if let Some(r) = replacements.and_then(|m| m.get(&seq.to_string())) {
                let r = r.strip_suffix('}').unwrap_or(r);
                writeln!(out, "{r},\"_replaced\":true}}")?;
            }
            Ok(())
        };
        for (name, (a, b)) in segments {
            if b < from || a > to {
                continue;
            }
            let reader = BufReader::new(zstd::Decoder::new(File::open(sdir.join(name))?)?);
            for line in reader.lines() {
                print(&line?, out)?;
            }
        }
        let open_bytes = state.streams.get(s.name()).map_or(0, |st| st.open_bytes);
        let reader = BufReader::new(File::open(sdir.join("open.ndjson"))?.take(open_bytes));
        for line in reader.lines() {
            print(&line?, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::PendingLog;

    fn event(n: u64) -> Pending {
        Pending::Log(PendingLog::event("info", "tick", &[("n", n.into())]))
    }

    fn dumped(dir: &Path, s: Signal) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        dump(dir, Some(s), 0, u64::MAX, &mut out).unwrap();
        String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn seqs(records: &[serde_json::Value]) -> Vec<u64> {
        records.iter().map(|r| r["seq"].as_u64().unwrap()).collect()
    }

    #[test]
    fn commit_and_recover() {
        let tmp = tempdir("commit");
        let (mut spool, tr) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        assert!(spool.fresh && tr.is_empty());
        let stream = spool.state.stream(Signal::Logs).stream.clone();
        assert!(crate::json::is_stream(&stream));
        let c = spool
            .commit(Signal::Logs, &[event(1), event(2)], false, |st| {
                st.journal_cursor = Some("s=1".into())
            })
            .unwrap();
        assert_eq!((c.first_seq, c.count), (1, 2));
        assert!(c.bugs.is_empty());
        let open_bytes = spool.state.stream(Signal::Logs).open_bytes;
        drop(spool);

        // Crash between the append and the state write: the extra bytes are not committed.
        let open = tmp.join("logs/open.ndjson");
        let mut f = OpenOptions::new().append(true).open(&open).unwrap();
        f.write_all(b"{\"seq\":3,\"garbage\":true}\n").unwrap();
        fs::write(tmp.join("state.json.tmp"), b"half").unwrap();
        let (spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        assert!(!spool.fresh);
        assert_eq!(spool.state.stream(Signal::Logs).stream, stream);
        assert_eq!(spool.state.stream(Signal::Logs).next_seq, 3);
        assert_eq!(spool.state.journal_cursor.as_deref(), Some("s=1"));
        assert_eq!(fs::metadata(&open).unwrap().len(), open_bytes);
        assert!(!tmp.join("state.json.tmp").exists());
        assert_eq!(seqs(&dumped(&tmp, Signal::Logs)), [1, 2]);
    }

    #[test]
    fn close_and_recover_at_every_step() {
        let tmp = tempdir("close");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        spool.segment_bytes = 1; // every commit closes a segment
        spool
            .commit(Signal::Logs, &[event(1), event(2)], false, |_| {})
            .unwrap();
        let seg = tmp.join("logs/00000000000000000001-00000000000000000002.ndjson.zst");
        assert!(seg.exists());
        assert_eq!(spool.state.stream(Signal::Logs).open_bytes, 0);
        spool.close(Signal::Logs).unwrap(); // idempotent on an empty open segment
        spool.segment_bytes = SEGMENT_BYTES;
        spool
            .commit(Signal::Logs, &[event(3)], false, |_| {})
            .unwrap();
        drop(spool);

        // Crash before the rename: the .tmp is removed, the open segment is intact.
        fs::write(
            tmp.join("logs/00000000000000000003-00000000000000000003.ndjson.zst.tmp"),
            b"x",
        )
        .unwrap();
        let (spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        assert_eq!(seqs(&dumped(&tmp, Signal::Logs)), [1, 2, 3]);
        drop(spool);

        // Crash after the rename, before the open segment is emptied: it is emptied at start.
        let open_bytes = fs::metadata(tmp.join("logs/open.ndjson")).unwrap().len();
        let mut enc = zstd::Encoder::new(
            File::create(tmp.join("logs/00000000000000000003-00000000000000000003.ndjson.zst"))
                .unwrap(),
            1,
        )
        .unwrap();
        io::copy(
            &mut File::open(tmp.join("logs/open.ndjson")).unwrap(),
            &mut enc,
        )
        .unwrap();
        enc.finish().unwrap();
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        assert_eq!(spool.state.stream(Signal::Logs).open_bytes, 0);
        assert!(open_bytes > 0 && fs::metadata(tmp.join("logs/open.ndjson")).unwrap().len() == 0);
        assert_eq!(seqs(&dumped(&tmp, Signal::Logs)), [1, 2, 3]);
        spool
            .commit(Signal::Logs, &[event(4)], false, |_| {})
            .unwrap();
        let all = dumped(&tmp, Signal::Logs);
        assert_eq!(seqs(&all), [1, 2, 3, 4]);
        for r in &all {
            assert_eq!(logschema::check_record(r.to_string().as_bytes()), Ok(()));
        }
        let mut out = Vec::new();
        dump(&tmp, Some(Signal::Logs), 2, 3, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 2);
    }

    #[test]
    fn read_across_segments_and_ack() {
        let tmp = tempdir("read");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        spool.segment_bytes = 1;
        spool
            .commit(Signal::Logs, &[event(1), event(2)], false, |_| {})
            .unwrap();
        spool.segment_bytes = SEGMENT_BYTES;
        spool
            .commit(Signal::Logs, &[event(3), event(4)], false, |_| {})
            .unwrap();
        let seq = |r: &Vec<u8>| seq_of(std::str::from_utf8(r).unwrap()).unwrap();
        let all = spool.read(Signal::Logs, 2, 1 << 20, 100).unwrap();
        assert_eq!(all.iter().map(seq).collect::<Vec<_>>(), [2, 3, 4]);
        assert!(all.iter().all(|r| r.last() == Some(&b'}')));
        let two = spool.read(Signal::Logs, 1, 1 << 20, 2).unwrap();
        assert_eq!(two.iter().map(seq).collect::<Vec<_>>(), [1, 2]);
        let one = spool.read(Signal::Logs, 1, 1, 100).unwrap();
        assert_eq!(
            one.len(),
            1,
            "at least one record, never more than max_bytes"
        );
        spool
            .set_replacement(Signal::Logs, 3, "{\"r\":3}".into())
            .unwrap();
        assert!(spool.ack(Signal::Logs, 2).unwrap().is_empty());
        let seg = tmp.join("logs/00000000000000000001-00000000000000000002.ndjson.zst");
        assert!(!seg.exists(), "fully acked closed segment deleted");
        assert_eq!(spool.closed_bytes[0], 0);
        assert!(spool.replacement(Signal::Logs, 3).is_some());
        spool.ack(Signal::Logs, 3).unwrap();
        assert!(spool.replacement(Signal::Logs, 3).is_none());
        assert_eq!(seqs(&dumped(&tmp, Signal::Logs)), [3, 4]);
        drop(spool);
        // A crash between the ack's state write and the deletion: deleted at start.
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        assert_eq!(spool.state.stream(Signal::Logs).acked_seq, 3);
        spool.close(Signal::Logs).unwrap();
        spool.state.stream_mut(Signal::Logs).acked_seq = 4;
        spool.write_state().unwrap();
        drop(spool);
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        assert_eq!(spool.snapshot().bytes[0], 0);
        assert!(
            spool
                .read(Signal::Logs, 5, 1 << 20, 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn full_spool_pauses_and_resumes() {
        let tmp = tempdir("full");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        let mut paused = false;
        for n in 0..1000 {
            let c = spool
                .commit(Signal::Logs, &[event(n)], false, |_| {})
                .unwrap();
            if c.transitions == [(Signal::Logs, true), (Signal::Metrics, true)] {
                paused = true;
                break;
            }
        }
        assert!(paused && spool.is_paused(Signal::Metrics));
        let before = spool.snapshot().total();
        // The reserve takes own events up to the limit, then nothing.
        let c = spool
            .commit(Signal::Logs, &[event(1000)], true, |_| {})
            .unwrap();
        assert_eq!(c.count, 1);
        for _ in 0..50 {
            spool
                .commit(Signal::Logs, &[event(1001)], true, |_| {})
                .unwrap();
        }
        assert!(spool.snapshot().total() > before && spool.snapshot().total() < 100_000 + 300);
        drop(spool);
        // Raising the limit and restarting resumes.
        let (spool, tr) = Spool::open(&tmp, "node-a", 1 << 20).unwrap();
        assert_eq!(tr, [(Signal::Logs, false), (Signal::Metrics, false)]);
        assert!(!spool.is_paused(Signal::Logs));
    }

    #[test]
    fn fully_acked_open_segment_is_emptied_and_resumes() {
        let tmp = tempdir("ack-open");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        let mut n = 0;
        while !spool.is_paused(Signal::Logs) {
            n += 1;
            spool
                .commit(Signal::Logs, &[event(n)], false, |_| {})
                .unwrap();
        }
        let open = tmp.join("logs/open.ndjson");
        // Partly acked: the open segment keeps every byte, still paused.
        assert!(spool.ack(Signal::Logs, n - 1).unwrap().is_empty());
        assert!(spool.state.stream(Signal::Logs).open_bytes > 0);
        // Fully acked: emptied at once, ingestion resumes without waiting for the close.
        assert_eq!(
            spool.ack(Signal::Logs, n).unwrap(),
            [(Signal::Logs, false), (Signal::Metrics, false)]
        );
        assert_eq!(spool.snapshot().total(), 0);
        assert_eq!(fs::metadata(&open).unwrap().len(), 0);
        assert!(
            fs::read_dir(tmp.join("logs")).unwrap().count() == 1,
            "no segment written"
        );
        // The next record goes on in the same stream, readable from the open segment.
        let c = spool
            .commit(Signal::Logs, &[event(n + 1)], false, |_| {})
            .unwrap();
        assert_eq!(c.first_seq, n + 1);
        assert_eq!(seqs(&dumped(&tmp, Signal::Logs)), [n + 1]);
        assert_eq!(
            spool.read(Signal::Logs, n + 1, 1 << 20, 10).unwrap().len(),
            1
        );
        drop(spool);
        let (spool, tr) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        assert!(tr.is_empty() && !spool.is_paused(Signal::Logs));
        assert_eq!(spool.state.stream(Signal::Logs).acked_seq, n);
        assert_eq!(seqs(&dumped(&tmp, Signal::Logs)), [n + 1]);
    }

    fn skipped() -> Pending {
        Pending::Scrape(Scrape {
            ts: crate::ts::now(),
            service: "hikyaku".into(),
            dur_us: 0,
            result: ScrapeResult::Down {
                error: "skipped-spool-full".into(),
            },
        })
    }

    fn line(n: u64) -> PendingLog {
        let raw = format!(
            "{{\"v\":1,\"ts\":\"2026-10-20T08:00:00.000000000Z\",\"level\":\"info\",\"event\":\"tick\",\"n\":{n},\"pad\":\"{}\"}}",
            "x".repeat(100)
        );
        PendingLog::from_source(raw.as_bytes(), "", "info", "gen1", "journald")
    }

    // The spool-full nixosTest: one journald batch crossed the limit, the reserve was gone and
    // the `skipped-spool-full` record (and the `spool-full` event) never reached the spool.
    #[test]
    fn batches_sized_by_room_keep_the_reserve() {
        let tmp = tempdir("room");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 2 << 20).unwrap();
        let unbounded: Vec<Pending> = (0..10_000).map(|n| Pending::Log(line(n))).collect();
        let c = spool
            .commit(Signal::Logs, &unbounded, false, |_| {})
            .unwrap();
        // Refused under the lock: it pauses without being written, the reserve is kept.
        assert!(!c.admitted && spool.is_paused(Signal::Metrics));
        assert_eq!(spool.snapshot().total(), 0);

        // What the sources do: close the batch once its estimate reaches the room.
        let tmp = tempdir("room-bounded");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 2 << 20).unwrap();
        let mut n = 0;
        while !spool.is_paused(Signal::Logs) {
            let room = spool.room();
            let (mut batch, mut bytes) = (Vec::new(), 0);
            while bytes < room && batch.len() < 1000 {
                let p = line(n);
                n += 1;
                let rendered = p.render("node-a", &"0".repeat(32), n).len() as u64 + 1;
                assert!(spool.estimate(&p) >= rendered);
                bytes += spool.estimate(&p);
                batch.push(Pending::Log(p));
            }
            spool.commit(Signal::Logs, &batch, false, |_| {}).unwrap();
        }
        let one = spool.estimate(&line(0));
        assert!(spool.snapshot().total() <= spool.limit - spool.limit / 100 + one);
        assert_eq!(spool.room(), 0);
        let c = spool
            .commit(Signal::Metrics, &[skipped()], true, |_| {})
            .unwrap();
        assert_eq!(c.count, 1);
        let ev = Pending::Log(PendingLog::event("warn", "spool-full", &[]));
        assert_eq!(
            spool
                .commit(Signal::Logs, &[ev], true, |_| {})
                .unwrap()
                .count,
            1
        );
        assert_eq!(
            dumped(&tmp, Signal::Metrics)[0]["error"],
            "skipped-spool-full"
        );
    }

    #[test]
    fn invalid_record_is_kept_as_bug() {
        let tmp = tempdir("bug");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        let bad = Pending::Log(PendingLog {
            line: "{\"v\":1,\"level\":\"info\",\"event\":\"x\"}".into(),
            service: "svc".into(),
            source: None,
        });
        let c = spool.commit(Signal::Logs, &[bad], false, |_| {}).unwrap();
        assert_eq!(c.bugs.len(), 1);
        assert_eq!(c.bugs[0].0, "missing-field:ts");
        let r = &dumped(&tmp, Signal::Logs)[0];
        assert_eq!(r["event"], "unparsed");
        assert_eq!(r["level"], "error");
        assert_eq!(logschema::check_record(r.to_string().as_bytes()), Ok(()));
    }

    #[test]
    fn second_open_is_refused_without_touching_the_spool() {
        let tmp = tempdir("lock");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        spool
            .commit(Signal::Logs, &[event(1)], false, |_| {})
            .unwrap();
        // The first agent appended a record and has not committed its state yet.
        let open = tmp.join("logs/open.ndjson");
        OpenOptions::new()
            .append(true)
            .open(&open)
            .unwrap()
            .write_all(b"{\"seq\":2}\n")
            .unwrap();
        let (len, state) = (
            fs::metadata(&open).unwrap().len(),
            fs::read(tmp.join("state.json")).unwrap(),
        );
        let err = Spool::open(&tmp, "node-a", 1 << 30).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
        assert_eq!(fs::metadata(&open).unwrap().len(), len);
        assert_eq!(fs::read(tmp.join("state.json")).unwrap(), state);
        drop(spool);
        assert!(
            Spool::open(&tmp, "node-a", 1 << 30).is_ok(),
            "released on drop"
        );
    }

    #[test]
    fn a_lost_open_segment_is_an_error() {
        let tmp = tempdir("lost-open");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        spool
            .commit(Signal::Logs, &[event(1), event(2)], false, |_| {})
            .unwrap();
        spool.ack(Signal::Logs, 1).unwrap();
        drop(spool);
        // The state counts on bytes the open segment no longer has, unacked and not closed.
        fs::write(tmp.join("logs/open.ndjson"), b"").unwrap();
        let err = Spool::open(&tmp, "node-a", 1 << 30).err().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
        assert!(err.to_string().contains("records lost"), "{err}");
    }

    #[test]
    fn crash_between_full_ack_and_truncation_empties_at_start() {
        let tmp = tempdir("ack-crash");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        let mut n = 0;
        while !spool.is_paused(Signal::Logs) {
            n += 1;
            spool
                .commit(Signal::Logs, &[event(n)], false, |_| {})
                .unwrap();
        }
        // The ack's state write, then the crash before the truncation.
        spool.state.stream_mut(Signal::Logs).acked_seq = n;
        spool.write_state().unwrap();
        drop(spool);
        let (spool, tr) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        assert_eq!(tr, [(Signal::Logs, false), (Signal::Metrics, false)]);
        assert_eq!(spool.snapshot().total(), 0);
        assert_eq!(fs::metadata(tmp.join("logs/open.ndjson")).unwrap().len(), 0);
        assert_eq!(spool.state.stream(Signal::Logs).next_seq, n + 1);
    }

    #[test]
    fn replacements_count_against_the_limit() {
        let tmp = tempdir("replacements");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        spool
            .commit(Signal::Logs, &[event(1), event(2)], false, |_| {})
            .unwrap();
        let before = spool.snapshot().total();
        assert!(
            spool
                .set_replacement(Signal::Logs, 1, "r".repeat(10_000))
                .unwrap()
                .is_empty()
        );
        assert_eq!(spool.snapshot().total(), before + 10_000);
        // Enough replaced bytes pause ingestion like spooled ones.
        assert_eq!(
            spool
                .set_replacement(Signal::Logs, 2, "r".repeat(90_000))
                .unwrap(),
            [(Signal::Logs, true), (Signal::Metrics, true)]
        );
        // Acked: dropped from the state and from the total, ingestion resumes.
        assert_eq!(
            spool.ack(Signal::Logs, 2).unwrap(),
            [(Signal::Logs, false), (Signal::Metrics, false)]
        );
        assert_eq!(spool.snapshot().total(), 0);
    }

    #[test]
    fn a_paused_spool_refuses_ingestion_under_its_lock() {
        let tmp = tempdir("admission");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        // Two sources sized their batches against the same room; the first one fills it.
        let room = spool.room();
        let mut first = Vec::new();
        let mut bytes = 0;
        while bytes < room {
            let seq = first.len() as u64 + 1;
            let p = line(seq);
            bytes += p.render("node-a", &"0".repeat(32), seq).len() as u64 + 1;
            first.push(Pending::Log(p));
        }
        let second = [Pending::Log(line(0))];
        let c = spool
            .commit(Signal::Logs, &first, false, |st| {
                st.journal_cursor = Some("a".into())
            })
            .unwrap();
        assert!(c.admitted && spool.is_paused(Signal::Logs));
        let total = spool.snapshot().total();
        let c = spool
            .commit(Signal::Logs, &second, false, |st| {
                st.journal_cursor = Some("b".into())
            })
            .unwrap();
        assert!(!c.admitted && c.count == 0);
        assert_eq!(
            spool.state.journal_cursor.as_deref(),
            Some("a"),
            "position kept"
        );
        assert_eq!(spool.snapshot().total(), total);
        // The reserve is untouched: own events still fit.
        assert!(total < spool.limit);
        let c = spool
            .commit(Signal::Logs, &[event(1)], true, |_| {})
            .unwrap();
        assert!(c.admitted && c.count == 1);
    }

    #[test]
    fn batches_that_fit_the_room_alone_but_not_together_keep_the_reserve() {
        let tmp = tempdir("admission-room");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 100_000).unwrap();
        // Two sources read the same room; each batch fits it alone, not both.
        let room = spool.room();
        let batch = |from: u64| {
            let (mut b, mut bytes) = (Vec::new(), 0);
            while bytes < room * 6 / 10 {
                let p = line(from + b.len() as u64);
                bytes += spool.estimate(&p);
                b.push(Pending::Log(p));
            }
            b
        };
        let (first, second) = (batch(0), batch(1000));
        let c = spool.commit(Signal::Logs, &first, false, |_| {}).unwrap();
        assert!(c.admitted && !spool.is_paused(Signal::Logs));
        let total = spool.snapshot().total();
        let c = spool
            .commit(Signal::Logs, &second, false, |st| {
                st.journal_cursor = Some("b".into())
            })
            .unwrap();
        assert!(!c.admitted && c.count == 0);
        assert_eq!(
            c.transitions,
            [(Signal::Logs, true), (Signal::Metrics, true)]
        );
        assert!(spool.is_paused(Signal::Logs) && spool.is_paused(Signal::Metrics));
        assert_eq!(spool.snapshot().total(), total, "nothing written");
        assert_eq!(spool.state.journal_cursor, None, "position kept");
        let c = spool
            .commit(Signal::Metrics, &[skipped()], true, |_| {})
            .unwrap();
        assert!(c.admitted && c.count == 1, "the reserve is untouched");
    }

    #[test]
    fn draining_a_closed_segment_goes_on_where_it_stopped() {
        let tmp = tempdir("drain");
        let (mut spool, _) = Spool::open(&tmp, "node-a", 1 << 30).unwrap();
        let items: Vec<Pending> = (1..=10).map(event).collect();
        spool.commit(Signal::Logs, &items, false, |_| {}).unwrap();
        spool.close(Signal::Logs).unwrap();
        let mut read = |from| -> Vec<u64> {
            spool
                .read(Signal::Logs, from, 1 << 20, 3)
                .unwrap()
                .iter()
                .map(|r| seq_of(std::str::from_utf8(r).unwrap()).unwrap())
                .collect()
        };
        assert_eq!(read(1), [1, 2, 3]);
        assert_eq!(read(1), [1, 2, 3], "a resend reads the segment again");
        assert_eq!(read(4), [4, 5, 6]);
        // The next batch comes from the kept reader, not from the segment's start: garbage
        // renamed over the segment's name is never opened.
        let seg = tmp.join(format!("logs/{:020}-{:020}.ndjson.zst", 1, 10));
        fs::write(tmp.join("garbage"), b"not zstd").unwrap();
        fs::rename(tmp.join("garbage"), seg).unwrap();
        assert_eq!(read(7), [7, 8, 9]);
        assert_eq!(read(10), [10]);
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hikyaku-spool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
