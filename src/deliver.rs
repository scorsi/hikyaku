//! Delivery to the stores (ADR 0005): one sender per configured stream, one request in flight,
//! in seq order, from the oldest unacked record; HTTPS with the node's bearer token, an optional
//! pinned CA and `server_name` (ADR 0006). The response table of ADR 0005 is `Sender::step`.

use crate::agent::Agent;
use crate::config::Destination;
use crate::pb::masu::v1 as masu_v1;
use crate::pb::nisshi::v1 as nisshi_v1;
use crate::record::{rejected_log, rejected_scrape};
use crate::spool::{Commit, Signal};
use crate::ts;
use prost::Message;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use serde_json::Value;
use std::io::Read;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TIMEOUT: Duration = Duration::from_secs(30);
/// A stopped stream (request-level 400, 401, 403, refused replacement) retries this often.
pub const STOPPED_RETRY: Duration = Duration::from_secs(300);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// A batch is at most 4 MiB of records before compression (at least one record).
pub const BATCH_BYTES: usize = 4 << 20;
/// With no backlog, records are batched for up to this long.
const LINGER: Duration = Duration::from_secs(1);
const ZSTD_LEVEL: i32 = 3;

/// A store's push endpoint: the HTTPS agent (CA, name, resolver), the URL and the token.
pub struct Client {
    http: ureq::Agent,
    url: String,
    token: String,
    token_file: std::path::PathBuf,
}

/// What the store answered, before interpretation.
#[derive(Debug)]
pub enum Response {
    Status(u16, Vec<u8>),
    /// Connection, TLS or timeout; `tls` when the handshake itself was refused.
    Network {
        err: String,
        tls: bool,
    },
}

impl Client {
    /// The token is read here and again after a 401 (`reload_token`), so a rotated token file
    /// is picked up without a restart; it never appears in an error.
    pub fn new(d: &Destination) -> Result<Client, String> {
        let token = read_token(&d.token_file)?;
        // No redirect: an ack must come from the configured store over TLS, never from
        // wherever a 3xx points (an http:// hop could forge acked_seq).
        let mut builder = ureq::AgentBuilder::new().timeout(TIMEOUT).redirects(0);
        if let Some(ca) = &d.ca_file {
            let roots = CertificateDer::pem_file_iter(ca)
                .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
                .map_err(|e| format!("ca_file {}: {e}", ca.display()))?;
            let mut store = rustls::RootCertStore::empty();
            let (added, _) = store.add_parsable_certificates(roots);
            if added == 0 {
                return Err(format!("ca_file {}: no certificate", ca.display()));
            }
            let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(store)
            .with_no_client_auth();
            builder = builder.tls_config(Arc::new(tls));
        }
        let base = d.url.trim_end_matches('/');
        let url = match &d.server_name {
            None => format!("{base}/v1/push"),
            Some(name) => {
                // The request names `server_name` (SNI, verification, Host); the resolver maps
                // it back to the configured host, so an IP literal needs no DNS (ADR 0005).
                let (scheme, rest) = base.split_once("://").ok_or("url: missing scheme")?;
                let (authority, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
                let port = match authority.rsplit_once(':') {
                    Some((_, p)) if p.parse::<u16>().is_ok() => p.to_string(),
                    _ => "443".to_string(),
                };
                let target = if authority.ends_with(&format!(":{port}")) {
                    authority.to_string()
                } else {
                    format!("{authority}:{port}")
                };
                let netloc = format!("{name}:{port}");
                builder = builder.resolver(move |n: &str| -> std::io::Result<Vec<SocketAddr>> {
                    let n = if n == netloc { target.as_str() } else { n };
                    Ok(n.to_socket_addrs()?.collect())
                });
                format!("{scheme}://{name}:{port}{path}/v1/push")
            }
        };
        Ok(Client {
            http: builder.build(),
            url,
            token,
            token_file: d.token_file.clone(),
        })
    }

    /// Reads the token file again; true when it holds another valid token.
    pub fn reload_token(&mut self) -> Result<bool, String> {
        let token = read_token(&self.token_file)?;
        let changed = token != self.token;
        self.token = token;
        Ok(changed)
    }

    pub fn push(&self, body: &[u8]) -> Response {
        let result = self
            .http
            .post(&self.url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Type", "application/x-protobuf")
            .set("Content-Encoding", "zstd")
            .send_bytes(body);
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => {
                let tls = std::error::Error::source(&t)
                    .and_then(|e| e.downcast_ref::<std::io::Error>())
                    .and_then(|e| e.get_ref())
                    .is_some_and(|e| e.is::<rustls::Error>());
                return Response::Network {
                    err: t.to_string(),
                    tls,
                };
            }
        };
        let status = resp.status();
        let mut body = Vec::new();
        if let Err(e) = resp.into_reader().take(1 << 20).read_to_end(&mut body) {
            return Response::Network {
                err: format!("reading the response: {e}"),
                tls: false,
            };
        }
        Response::Status(status, body)
    }
}

/// One line of visible ASCII (RFC 6750's token characters are a subset): anything else would
/// make an invalid header, and the HTTP library would quote the whole header, token included,
/// in its error. The errors here never carry the file's content.
fn read_token(path: &std::path::Path) -> Result<String, String> {
    let text = std::fs::read(path).map_err(|e| format!("token_file {}: {e}", path.display()))?;
    let token = text.trim_ascii();
    if token.is_empty() || token.len() > 4096 || !token.iter().all(|c| c.is_ascii_graphic()) {
        return Err(format!(
            "token_file {}: not one line of visible ASCII characters (at most 4096)",
            path.display()
        ));
    }
    Ok(String::from_utf8_lossy(token).into_owned())
}

/// The `code` label of `hikyaku_push_requests_total` (masu's metric convention).
pub fn code_label(r: &Response) -> String {
    match r {
        Response::Network { .. } => "network".into(),
        Response::Status(c, _) if (500..600).contains(c) && *c != 507 => "5xx".into(),
        Response::Status(c, _) => c.to_string(),
    }
}

/// After a step: send again at once, or wait.
#[derive(Debug, PartialEq)]
pub enum Next {
    Again,
    Wait(Duration),
}

pub struct Sender<'a> {
    agent: &'a Agent,
    signal: Signal,
    client: Client,
    max_records: usize,
    backoff: Duration,
}

/// One batch as read from the spool, replacements applied.
struct Batch {
    first: u64,
    records: Vec<Vec<u8>>,
    replaced: Vec<bool>,
    more: bool,
    body: Vec<u8>,
}

impl<'a> Sender<'a> {
    pub fn new(agent: &'a Agent, signal: Signal, client: Client) -> Sender<'a> {
        Sender {
            agent,
            signal,
            client,
            max_records: usize::MAX,
            backoff: BACKOFF_MIN,
        }
    }

    fn event(&self, level: &str, event: &str, fields: &[(&str, Value)]) {
        let mut all = vec![("signal", self.signal.name().into())];
        all.extend_from_slice(fields);
        self.agent.event(level, event, &all);
    }

    /// The spool lock is held while reading, never while sending or logging.
    fn batch(&self) -> std::io::Result<Option<Batch>> {
        let spool = self.agent.spool.lock().unwrap();
        let st = spool.state.stream(self.signal);
        let (first, next_seq, stream) = (st.acked_seq + 1, st.next_seq, st.stream.clone());
        if first >= next_seq {
            return Ok(None);
        }
        let mut records = spool.read(self.signal, first, BATCH_BYTES, self.max_records)?;
        if records.is_empty() {
            return Ok(None);
        }
        let oldest = serde_json::from_slice::<Value>(&records[0])
            .ok()
            .and_then(|v| v["ts"].as_str().and_then(ts::to_unix_nanos))
            .unwrap_or(0);
        let mut replaced = vec![false; records.len()];
        for (i, r) in records.iter_mut().enumerate() {
            if let Some(x) = spool.replacement(self.signal, first + i as u64) {
                *r = x.clone().into_bytes();
                replaced[i] = true;
            }
        }
        let snap = spool.snapshot();
        let paused = spool.is_paused(self.signal);
        let node = self.agent.cfg.node.clone();
        drop(spool);
        let stream_id = hex(&stream);
        let version = env!("CARGO_PKG_VERSION").to_string();
        // The request takes the records and gives them back once encoded: a refusal needs
        // the refused bytes, and a 4 MiB batch is not copied.
        macro_rules! request {
            ($m:ident) => {{
                let req = $m::PushRequest {
                    node,
                    stream_id,
                    first_seq: first,
                    records,
                    status: Some($m::AgentStatus {
                        version,
                        spool_bytes: snap.total(),
                        spool_limit_bytes: snap.limit,
                        paused,
                        oldest_unacked_ns: oldest,
                    }),
                };
                (req.encode_to_vec(), req.records)
            }};
        }
        let (pb, records) = match self.signal {
            Signal::Logs => request!(nisshi_v1),
            Signal::Metrics => request!(masu_v1),
        };
        let n = records.len() as u64;
        let body = zstd::bulk::compress(&pb, ZSTD_LEVEL)?;
        drop(pb);
        Ok(Some(Batch {
            first,
            records,
            replaced,
            more: first + n < next_seq,
            body,
        }))
    }

    /// One request and what the response table of ADR 0005 makes of it.
    pub fn step(&mut self) -> Next {
        let batch = match self.batch() {
            Ok(Some(b)) => b,
            Ok(None) => return Next::Wait(LINGER),
            Err(e) => {
                self.event("error", "spool-error", &[("err", e.to_string().into())]);
                return Next::Wait(BACKOFF_MAX);
            }
        };
        let n = batch.records.len();
        let last = batch.first + n as u64 - 1;
        let resp = self.client.push(&batch.body);
        let code = code_label(&resp);
        self.agent.metrics.inc(
            "hikyaku_push_requests_total",
            &[("signal", self.signal.name()), ("code", &code)],
        );
        match resp {
            Response::Status(200, body) => {
                let acked = match self.signal {
                    Signal::Logs => {
                        nisshi_v1::PushResponse::decode(body.as_slice()).map(|r| r.acked_seq)
                    }
                    Signal::Metrics => {
                        masu_v1::PushResponse::decode(body.as_slice()).map(|r| r.acked_seq)
                    }
                };
                let Ok(acked) = acked else {
                    return self.retry("200", "bad-push-response".into(), false);
                };
                let acked = acked.min(last);
                if acked < batch.first {
                    return self.retry(
                        "200",
                        format!("acked_seq {acked} below {}", batch.first),
                        false,
                    );
                }
                self.backoff = BACKOFF_MIN;
                self.max_records = self.max_records.saturating_mul(2);
                let result = self.agent.spool.lock().unwrap().ack(self.signal, acked);
                self.agent.after(result.map(|transitions| Commit {
                    transitions,
                    ..Commit::default()
                }));
                if batch.more || acked < last {
                    Next::Again
                } else {
                    Next::Wait(LINGER)
                }
            }
            Response::Status(413, _) if n > 1 => {
                self.max_records = n / 2;
                Next::Again
            }
            Response::Status(413, _) => {
                // One record over the store's limit: the agent built it, so it is an agent bug.
                self.event(
                    "error",
                    "record-too-large",
                    &[("rejected_seq", batch.first.into()), ("err", "413".into())],
                );
                Next::Wait(STOPPED_RETRY)
            }
            Response::Status(400, body) => {
                let rejected = match self.signal {
                    Signal::Logs => nisshi_v1::PushRejected::decode(body.as_slice())
                        .map(|r| (r.record_index, r.reason)),
                    Signal::Metrics => masu_v1::PushRejected::decode(body.as_slice())
                        .map(|r| (r.record_index, r.reason)),
                };
                let (index, reason) = rejected.unwrap_or((None, "bad-push-rejected".into()));
                let reason = bounded(reason);
                match index.map(|i| i as usize).filter(|&i| i < n) {
                    Some(i) => self.replace(&batch, i, reason),
                    None => self.stop("400", reason),
                }
            }
            Response::Status(c @ 500..=599, _) => self.retry(&code, format!("http-{c}"), false),
            Response::Status(401, _) => match self.client.reload_token() {
                // The token file was rotated since it was read: try the new token at once.
                Ok(true) => Next::Again,
                Ok(false) => self.stop("401", "http-401".into()),
                Err(e) => self.stop("401", e),
            },
            Response::Status(c, body) => {
                // A 403 says why (masu, nisshi: "stream-owner", wrong role) when it has a body.
                let reason = match self.signal {
                    Signal::Logs => {
                        nisshi_v1::PushRejected::decode(body.as_slice()).map(|r| r.reason)
                    }
                    Signal::Metrics => {
                        masu_v1::PushRejected::decode(body.as_slice()).map(|r| r.reason)
                    }
                };
                let err = reason
                    .ok()
                    .filter(|r| c == 403 && !r.is_empty())
                    .map_or_else(|| format!("http-{c}"), bounded);
                self.stop(&c.to_string(), err)
            }
            Response::Network { err, tls } => self.retry("network", err, tls),
        }
    }

    /// A record-level 400: the refused record is replaced under the same `seq`, the
    /// replacement committed before the resend; a refused replacement stops the stream.
    fn replace(&mut self, batch: &Batch, i: usize, reason: String) -> Next {
        let seq = batch.first + i as u64;
        if batch.replaced[i] {
            self.event(
                "error",
                "replacement-rejected",
                &[("rejected_seq", seq.into()), ("err", reason.into())],
            );
            return Next::Wait(STOPPED_RETRY);
        }
        let refused = &batch.records[i];
        let (node, stream) = {
            let spool = self.agent.spool.lock().unwrap();
            (
                self.agent.cfg.node.clone(),
                spool.state.stream(self.signal).stream.clone(),
            )
        };
        let record = match self.signal {
            Signal::Logs => rejected_log(refused, &reason, &node, &stream, seq),
            Signal::Metrics => rejected_scrape(refused, &reason, &node, &stream, seq),
        };
        // Checked before it is persisted: a record spooled before the size budget (ADR 0013)
        // can give a replacement over the stores' limit, which would be refused in its turn.
        let valid = match self.signal {
            Signal::Logs => crate::logschema::check_record(record.as_bytes()),
            Signal::Metrics => crate::scrape_record::check(record.as_bytes()),
        };
        if let Err(err) = valid {
            self.event(
                "error",
                "replacement-rejected",
                &[("rejected_seq", seq.into()), ("err", err.into())],
            );
            return Next::Wait(STOPPED_RETRY);
        }
        let saved = self
            .agent
            .spool
            .lock()
            .unwrap()
            .set_replacement(self.signal, seq, record);
        self.agent.after(saved.map(|transitions| Commit {
            transitions,
            ..Commit::default()
        }));
        // The label is the reason's class ("duplicate-field", not "duplicate-field:<name>"):
        // bounded cardinality; the event keeps the full reason.
        let class = reason.split(':').next().unwrap_or_default();
        self.agent.metrics.inc(
            "hikyaku_rejected_records_total",
            &[("signal", self.signal.name()), ("reason", class)],
        );
        self.event(
            "error",
            "record-rejected",
            &[("rejected_seq", seq.into()), ("reason", reason.into())],
        );
        Next::Again
    }

    /// A request-level 400, 401, 403: a config or agent bug no retry at once can fix.
    fn stop(&mut self, code: &str, err: String) -> Next {
        self.event(
            "error",
            "push-error",
            &[
                ("code", code.into()),
                ("err", err.into()),
                ("retry_in_s", STOPPED_RETRY.as_secs().into()),
            ],
        );
        Next::Wait(STOPPED_RETRY)
    }

    /// 507, 5xx, network, timeout: exponential backoff with jitter. A refused TLS handshake
    /// (pinned CA, server name) is logged at `error`: it will not heal by itself.
    fn retry(&mut self, code: &str, err: String, tls: bool) -> Next {
        let wait = jitter(self.backoff);
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX);
        self.event(
            if tls { "error" } else { "warn" },
            "push-error",
            &[
                ("code", code.into()),
                ("err", err.into()),
                ("retry_in_s", wait.as_secs_f64().into()),
            ],
        );
        Next::Wait(wait)
    }
}

/// A store's reason, cut to 256 bytes: it goes into events, replacements and the state.
fn bounded(mut reason: String) -> String {
    if reason.len() > 256 {
        let mut end = 256;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
    }
    reason
}

/// Between half and all of `d`.
fn jitter(d: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as f64
        / 1e9;
    d.mul_f64(0.5 + nanos / 2.0)
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok())
        .collect()
}

/// The sender thread of one configured stream; a destination it cannot even set up (token,
/// CA file) is retried every 5 minutes.
pub fn run(agent: &Agent, signal: Signal) {
    let Some(d) = agent.cfg.destination(signal) else {
        return;
    };
    let client = loop {
        match Client::new(d) {
            Ok(c) => break c,
            Err(e) => {
                agent.event(
                    "error",
                    "push-error",
                    &[("signal", signal.name().into()), ("err", e.into())],
                );
                thread::sleep(STOPPED_RETRY);
            }
        }
    };
    let mut sender = Sender::new(agent, signal, client);
    loop {
        if let Next::Wait(d) = sender.step() {
            thread::sleep(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Metrics;
    use crate::record::{PendingLog, base64};
    use crate::spool::{Pending, Spool};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::Mutex;
    use std::sync::mpsc::{Receiver, channel};

    type Reply = Box<dyn Fn(&nisshi_v1::PushRequest) -> (u16, Vec<u8>) + Send>;

    /// A plain-HTTP fake store: answers each push with the next reply, hands the decoded
    /// request to the test.
    fn fake_store(replies: Vec<Reply>) -> (String, Receiver<(String, nisshi_v1::PushRequest)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = channel();
        thread::spawn(move || {
            for reply in replies {
                let (conn, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(conn);
                let mut len = 0;
                let mut auth = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let lower = line.to_ascii_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line.trim().to_string();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let pb = zstd::decode_all(body.as_slice()).unwrap();
                let req = nisshi_v1::PushRequest::decode(pb.as_slice()).unwrap();
                let (code, out) = reply(&req);
                let mut conn = reader.into_inner();
                write!(
                    conn,
                    "HTTP/1.1 {code} X\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                )
                .unwrap();
                conn.write_all(&out).unwrap();
                let _ = tx.send((auth, req));
            }
        });
        (url, rx)
    }

    fn ack(req: &nisshi_v1::PushRequest) -> (u16, Vec<u8>) {
        let last = req.first_seq + req.records.len() as u64 - 1;
        (
            200,
            nisshi_v1::PushResponse { acked_seq: last }.encode_to_vec(),
        )
    }

    fn refuse(index: u32) -> Reply {
        Box::new(move |_| {
            (
                400,
                nisshi_v1::PushRejected {
                    record_index: Some(index),
                    reason: "test-refused".into(),
                }
                .encode_to_vec(),
            )
        })
    }

    fn agent(name: &str, url: &str) -> Agent {
        let dir =
            std::env::temp_dir().join(format!("hikyaku-deliver-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("token"), "secret-token\n").unwrap();
        let cfg: crate::config::Config = toml::from_str(&format!(
            "node = \"node-a\"\nspool = {:?}\nspool_limit = 1073741824\njournald = false\n[nisshi]\nurl = \"{url}\"\ntoken_file = {:?}\n",
            dir.join("spool"),
            dir.join("token")
        ))
        .unwrap();
        let (spool, _) = Spool::open(&cfg.spool, "node-a", cfg.spool_limit).unwrap();
        Agent {
            cfg,
            spool: Mutex::new(spool),
            metrics: Metrics::default(),
        }
    }

    fn records(agent: &Agent, n: u64) {
        let items: Vec<Pending> = (1..=n)
            .map(|i| Pending::Log(PendingLog::event("info", "tick", &[("n", i.into())])))
            .collect();
        agent.commit(Signal::Logs, &items, |_| {});
    }

    fn sender(agent: &Agent) -> Sender<'_> {
        let client = Client::new(agent.cfg.nisshi.as_ref().unwrap()).unwrap();
        Sender::new(agent, Signal::Logs, client)
    }

    fn acked(agent: &Agent) -> u64 {
        agent
            .spool
            .lock()
            .unwrap()
            .state
            .stream(Signal::Logs)
            .acked_seq
    }

    #[test]
    fn replacement_413_and_ack() {
        let (url, rx) = fake_store(vec![
            refuse(1),
            Box::new(|_| (413, Vec::new())),
            Box::new(ack),
        ]);
        let agent = agent("replace", &url);
        records(&agent, 3);
        let mut s = sender(&agent);
        assert_eq!(s.step(), Next::Again);
        let (auth, first) = rx.recv().unwrap();
        assert_eq!(auth, "Authorization: Bearer secret-token");
        assert_eq!((first.first_seq, first.records.len()), (1, 3));
        assert_eq!(first.node, "node-a");
        assert_eq!(first.stream_id.len(), 16);
        assert!(!first.status.as_ref().unwrap().paused);
        let refused = first.records[1].clone();
        // Committed before the resend; the record-rejected event joined the stream (seq 4).
        let saved = agent
            .spool
            .lock()
            .unwrap()
            .replacement(Signal::Logs, 2)
            .cloned()
            .unwrap();
        let v: Value = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            (v["event"].as_str(), v["seq"].as_u64()),
            (Some("rejected"), Some(2))
        );
        assert_eq!(v["raw_b64"], base64(&refused));
        assert_eq!(crate::logschema::check_record(saved.as_bytes()), Ok(()));

        // 413: half the records, at once.
        assert_eq!(s.step(), Next::Again);
        let second = rx.recv().unwrap().1;
        assert_eq!(second.records.len(), 4);
        assert_eq!(second.records[1], saved.as_bytes());
        // Half of them acked, the rest still pending: at once.
        assert_eq!(s.step(), Next::Again);
        let third = rx.recv().unwrap().1;
        assert_eq!(third.records.len(), 2);
        assert_eq!(acked(&agent), 2);
        let spool = agent.spool.lock().unwrap();
        assert!(
            spool.replacement(Signal::Logs, 2).is_none(),
            "dropped once acked"
        );
        drop(spool);
        let m = agent
            .metrics
            .render(&agent.spool.lock().unwrap().snapshot());
        assert!(m.contains(
            "hikyaku_rejected_records_total{signal=\"logs\",reason=\"test-refused\"} 1\n"
        ));
        assert!(m.contains("hikyaku_push_requests_total{signal=\"logs\",code=\"400\"} 1\n"));
        assert!(m.contains("hikyaku_push_requests_total{signal=\"logs\",code=\"413\"} 1\n"));
        assert!(m.contains("hikyaku_push_requests_total{signal=\"logs\",code=\"200\"} 1\n"));
    }

    #[test]
    fn refused_replacement_and_request_errors_stop_the_stream() {
        let (url, rx) = fake_store(vec![
            refuse(0),
            refuse(0),
            Box::new(|_| {
                (
                    400,
                    nisshi_v1::PushRejected {
                        record_index: None,
                        reason: "node-mismatch".into(),
                    }
                    .encode_to_vec(),
                )
            }),
            Box::new(|_| (401, Vec::new())),
            Box::new(|_| {
                (
                    403,
                    nisshi_v1::PushRejected {
                        record_index: None,
                        reason: "stream-owner".into(),
                    }
                    .encode_to_vec(),
                )
            }),
            Box::new(|_| (507, Vec::new())),
            Box::new(|_| (503, Vec::new())),
            Box::new(ack),
        ]);
        let agent = agent("stop", &url);
        records(&agent, 1);
        let mut s = sender(&agent);
        assert_eq!(s.step(), Next::Again);
        assert_eq!(
            s.step(),
            Next::Wait(STOPPED_RETRY),
            "the replacement refused too"
        );
        let second = rx.recv().and_then(|_| rx.recv()).unwrap().1;
        assert!(
            std::str::from_utf8(&second.records[0])
                .unwrap()
                .contains("\"event\":\"rejected\"")
        );
        for _ in 0..3 {
            assert_eq!(s.step(), Next::Wait(STOPPED_RETRY));
        }
        assert_eq!(acked(&agent), 0);
        // The 403's reason is the push-error's err.
        let spooled = agent
            .spool
            .lock()
            .unwrap()
            .read(Signal::Logs, 1, usize::MAX, usize::MAX)
            .unwrap();
        assert!(
            spooled.iter().any(|r| {
                let r = String::from_utf8_lossy(r);
                r.contains("\"event\":\"push-error\"")
                    && r.contains("\"err\":\"stream-owner\"")
                    && r.contains("\"code\":\"403\"")
            }),
            "no push-error with the 403 reason"
        );
        let Next::Wait(a) = s.step() else { panic!() };
        let Next::Wait(b) = s.step() else { panic!() };
        assert!(
            a <= BACKOFF_MIN && b <= 2 * BACKOFF_MIN && b >= BACKOFF_MIN / 2,
            "{a:?} {b:?}"
        );
        s.step();
        assert!(acked(&agent) >= 1);
        let m = agent
            .metrics
            .render(&agent.spool.lock().unwrap().snapshot());
        for code in [
            "400\"} 3", "401\"} 1", "403\"} 1", "507\"} 1", "5xx\"} 1", "200\"} 1",
        ] {
            assert!(
                m.contains(&format!(
                    "hikyaku_push_requests_total{{signal=\"logs\",code=\"{code}\n"
                )),
                "{code}\n{m}"
            );
        }
    }

    #[test]
    fn network_error_and_server_name() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let agent = agent("network", &format!("http://127.0.0.1:{port}"));
        records(&agent, 1);
        let mut s = sender(&agent);
        assert!(matches!(s.step(), Next::Wait(_)));
        let m = agent
            .metrics
            .render(&agent.spool.lock().unwrap().snapshot());
        assert!(m.contains("hikyaku_push_requests_total{signal=\"logs\",code=\"network\"} 1\n"));

        // server_name: the URL names it, the resolver maps it back to the IP literal.
        let d = Destination {
            url: "https://192.0.2.10:9473/".into(),
            server_name: Some("nisshi.example.org".into()),
            ca_file: None,
            token_file: agent.cfg.nisshi.as_ref().unwrap().token_file.clone(),
        };
        assert_eq!(
            Client::new(&d).unwrap().url,
            "https://nisshi.example.org:9473/v1/push"
        );
        let d = Destination {
            url: "https://[2001:db8::1]".into(),
            ..d
        };
        assert_eq!(
            Client::new(&d).unwrap().url,
            "https://nisshi.example.org:443/v1/push"
        );
        let d = Destination {
            ca_file: Some("/nonexistent/ca.pem".into()),
            ..d
        };
        let err = Client::new(&d).err().unwrap();
        assert!(err.starts_with("ca_file /nonexistent/ca.pem"), "{err}");
        assert!(!err.contains("secret-token"));
    }

    #[test]
    fn a_redirect_is_not_followed() {
        let elsewhere = TcpListener::bind("127.0.0.1:0").unwrap();
        elsewhere.set_nonblocking(true).unwrap();
        let target = format!("http://{}/v1/push", elsewhere.local_addr().unwrap());
        let store = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", store.local_addr().unwrap());
        thread::spawn(move || {
            let (c, _) = store.accept().unwrap();
            // The whole request is read first: closing on a body still in flight resets it.
            let mut reader = BufReader::new(c);
            let mut len = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            reader.read_exact(&mut vec![0; len]).unwrap();
            let mut c = reader.into_inner();
            write!(
                c,
                "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        let agent = agent("redirect-location", &url);
        records(&agent, 1);
        let mut s = sender(&agent);
        assert_eq!(s.step(), Next::Wait(STOPPED_RETRY));
        assert_eq!(acked(&agent), 0);
        assert!(
            elsewhere.accept().is_err(),
            "the redirect target was contacted"
        );
    }

    #[test]
    fn a_rotated_token_is_used_after_a_401() {
        let (url, rx) = fake_store(vec![Box::new(|_| (401, Vec::new())), Box::new(ack)]);
        let agent = agent("rotate", &url);
        records(&agent, 1);
        let mut s = sender(&agent);
        let token_file = agent.cfg.nisshi.as_ref().unwrap().token_file.clone();
        std::fs::write(&token_file, "rotated-token\n").unwrap();
        assert_eq!(s.step(), Next::Again, "a new token is tried at once");
        assert_eq!(rx.recv().unwrap().0, "Authorization: Bearer secret-token");
        s.step();
        assert_eq!(rx.recv().unwrap().0, "Authorization: Bearer rotated-token");
        assert_eq!(acked(&agent), 1);
    }

    #[test]
    fn a_malformed_token_never_shows_in_errors() {
        let agent = agent("bad-token", "https://nisshi.example.org");
        let d = agent.cfg.nisshi.as_ref().unwrap();
        for bad in [
            &b"secret\nleak"[..],
            b"secret\x01leak",
            "secret\u{e9}leak".as_bytes(),
            b"secret leak",
            b"  \n",
        ] {
            std::fs::write(&d.token_file, bad).unwrap();
            let err = Client::new(d).err().unwrap();
            assert!(!err.contains("secret") && !err.contains("leak"), "{err}");
        }
    }

    #[test]
    fn an_oversized_legacy_record_is_not_replaced() {
        let agent = agent("legacy", "http://127.0.0.1:9");
        records(&agent, 1);
        let mut s = sender(&agent);
        // A record spooled before the size budget: its replacement would exceed 4 MiB.
        let batch = Batch {
            first: 1,
            records: vec![vec![b'a'; 3_500_000]],
            replaced: vec![false],
            more: false,
            body: Vec::new(),
        };
        assert_eq!(s.replace(&batch, 0, "x".into()), Next::Wait(STOPPED_RETRY));
        let spool = agent.spool.lock().unwrap();
        assert!(
            spool.replacement(Signal::Logs, 1).is_none(),
            "not persisted"
        );
        assert!(
            spool
                .read(Signal::Logs, 1, usize::MAX, usize::MAX)
                .unwrap()
                .iter()
                .any(|r| String::from_utf8_lossy(r).contains("\"event\":\"replacement-rejected\""))
        );
    }
}
