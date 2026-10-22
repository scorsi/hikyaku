//! Scrapes (ADR 0003): one record per attempt in masu's envelope, the body kept as received,
//! `series-limit` above `max_series`, `body-too-large` above 2 MiB.

use crate::record::{Scrape, ScrapeResult};
use crate::scrape_record::MAX_BODY_BYTES;
use crate::ts;
use std::io::Read;
use std::time::{Duration, Instant};

pub const TIMEOUT: Duration = Duration::from_secs(10);

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(TIMEOUT).build()
}

/// One scrape; the second value is the `result` label of `hikyaku_scrapes_total`.
pub fn scrape(
    agent: &ureq::Agent,
    service: &str,
    url: &str,
    max_series: u64,
) -> (Scrape, &'static str) {
    let ts = ts::now();
    let start = Instant::now();
    let (result, label) = match agent.get(url).call() {
        Ok(resp) => {
            let content_type = resp.header("Content-Type").unwrap_or("").to_string();
            let mut body = Vec::new();
            match resp
                .into_reader()
                .take(MAX_BODY_BYTES as u64 + 1)
                .read_to_end(&mut body)
            {
                Err(e) => (
                    ScrapeResult::Down {
                        error: io_error(&e),
                    },
                    "error",
                ),
                // The cap is on the body as the record holds it, JSON-escaped (ADR 0013): a
                // refused record must still fit its replacement.
                Ok(_) if body.len() > MAX_BODY_BYTES || escaped_len(&body) > MAX_BODY_BYTES => (
                    ScrapeResult::Down {
                        error: "body-too-large".into(),
                    },
                    "error",
                ),
                Ok(_) if series(&body) > max_series => (
                    ScrapeResult::Down {
                        error: "series-limit".into(),
                    },
                    "series-limit",
                ),
                // Kept as received or not at all: no lossy replacement of invalid bytes.
                Ok(_) => match String::from_utf8(body) {
                    Ok(body) => (ScrapeResult::Up { content_type, body }, "ok"),
                    Err(_) => (
                        ScrapeResult::Down {
                            error: "body-not-utf8".into(),
                        },
                        "error",
                    ),
                },
            }
        }
        Err(ureq::Error::Status(code, _)) => (
            ScrapeResult::Down {
                error: format!("http-{code}"),
            },
            "error",
        ),
        Err(ureq::Error::Transport(t)) => {
            let text = t.to_string();
            let error = match t.kind() {
                ureq::ErrorKind::ConnectionFailed => "connection-refused".to_string(),
                _ if text.contains("timed out") || text.contains("timeout") => {
                    "timeout".to_string()
                }
                _ => text,
            };
            (ScrapeResult::Down { error }, "error")
        }
    };
    (
        Scrape {
            ts,
            service: service.to_string(),
            dur_us: start.elapsed().as_micros() as u64,
            result,
        },
        label,
    )
}

/// The bytes `body` takes as a JSON string (quotes excluded); invalid UTF-8 counts as is.
fn escaped_len(body: &[u8]) -> usize {
    body.iter()
        .map(|&c| match c {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 0x08 | 0x0c => 2,
            0..0x20 => 6,
            _ => 1,
        })
        .sum()
}

/// Samples in an exposition: non-empty lines that are not comments.
pub fn series(body: &[u8]) -> u64 {
    body.split(|&b| b == b'\n')
        .filter(|l| !l.is_empty() && l[0] != b'#')
        .count() as u64
}

fn io_error(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "timeout".into(),
        _ => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn serve_once(status: &'static str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/metrics", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = c.read(&mut buf);
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            c.write_all(head.as_bytes()).unwrap();
            c.write_all(&body).unwrap();
        });
        url
    }

    #[test]
    fn results() {
        let agent = agent();
        let (s, label) = scrape(
            &agent,
            "svc",
            &serve_once("200 OK", b"# TYPE a counter\na_total 1\n".to_vec()),
            10,
        );
        assert_eq!(label, "ok");
        assert_eq!(
            s.result,
            ScrapeResult::Up {
                content_type: "text/plain; version=0.0.4".into(),
                body: "# TYPE a counter\na_total 1\n".into()
            }
        );
        let (s, label) = scrape(
            &agent,
            "svc",
            &serve_once("200 OK", b"a 1\nb 2\nc 3\n".to_vec()),
            2,
        );
        assert_eq!(
            (label, s.result),
            (
                "series-limit",
                ScrapeResult::Down {
                    error: "series-limit".into()
                }
            )
        );
        let (s, _) = scrape(
            &agent,
            "svc",
            &serve_once("200 OK", vec![b'x'; MAX_BODY_BYTES + 1]),
            10,
        );
        assert_eq!(
            s.result,
            ScrapeResult::Down {
                error: "body-too-large".into()
            }
        );
        // Under 2 MiB as received, over it once JSON-escaped.
        let (s, _) = scrape(
            &agent,
            "svc",
            &serve_once("200 OK", vec![b'\\'; 1_600_000]),
            10,
        );
        assert_eq!(
            s.result,
            ScrapeResult::Down {
                error: "body-too-large".into()
            }
        );
        let (s, label) = scrape(
            &agent,
            "svc",
            &serve_once("200 OK", b"a 1\xff\n".to_vec()),
            10,
        );
        assert_eq!(
            (label, s.result),
            (
                "error",
                ScrapeResult::Down {
                    error: "body-not-utf8".into()
                }
            )
        );
        let (s, label) = scrape(
            &agent,
            "svc",
            &serve_once("503 Service Unavailable", Vec::new()),
            10,
        );
        assert_eq!(
            (label, s.result),
            (
                "error",
                ScrapeResult::Down {
                    error: "http-503".into()
                }
            )
        );
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/metrics", closed.local_addr().unwrap());
        drop(closed);
        let (s, _) = scrape(&agent, "svc", &url, 10);
        assert_eq!(
            s.result,
            ScrapeResult::Down {
                error: "connection-refused".into()
            }
        );
        let tricky = "a\"b\\c\n\t\u{1}\u{8}\u{c}\u{7f}é";
        assert_eq!(
            escaped_len(tricky.as_bytes()),
            serde_json::Value::from(tricky).to_string().len() - 2
        );
        assert_eq!(
            series(b"# HELP x\n# TYPE x gauge\nx 1\n\ny{a=\"b\"} 2\n"),
            2
        );
    }
}
