//! The configuration file of ADR 0007: TOML rendered by the modules, every struct with
//! `deny_unknown_fields` so a misspelled key is an error (ADR 0002). `load` adds the checks
//! TOML cannot express (ADR 0005, 0006).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub node: String,
    #[serde(default = "default_spool")]
    pub spool: PathBuf,
    pub spool_limit: u64,
    #[serde(default = "default_listen")]
    pub listen_metrics: SocketAddr,
    #[serde(default = "default_true")]
    pub journald: bool,
    #[serde(default)]
    pub targets: BTreeMap<String, Target>,
    #[serde(default)]
    pub files: BTreeMap<String, FileInput>,
    pub masu: Option<Destination>,
    pub nisshi: Option<Destination>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub url: String,
    #[serde(default = "default_interval", deserialize_with = "duration")]
    pub interval: Duration,
    #[serde(default = "default_max_series")]
    pub max_series: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInput {
    pub path: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    pub url: String,
    pub server_name: Option<String>,
    pub ca_file: Option<PathBuf>,
    pub token_file: PathBuf,
}

fn default_spool() -> PathBuf {
    "/var/lib/hikyaku".into()
}
fn default_listen() -> SocketAddr {
    ([127, 0, 0, 1], 9471).into()
}
fn default_true() -> bool {
    true
}
fn default_interval() -> Duration {
    Duration::from_secs(30)
}
fn default_max_series() -> u64 {
    2000
}

/// `"30s"`, `"5m"`, `"1h"`.
fn duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    let s = String::deserialize(d)?;
    parse_duration(&s).ok_or_else(|| serde::de::Error::custom(format!("bad duration {s:?}")))
}

pub fn parse_duration(s: &str) -> Option<Duration> {
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = num.parse().ok()?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return None,
    };
    (n > 0).then(|| Duration::from_secs(n * mult))
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Config = toml::from_str(&text).map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Where a signal goes (ADR 0005): logs to nisshi, metrics to masu; `None` = spooled only.
    pub fn destination(&self, s: crate::spool::Signal) -> Option<&Destination> {
        match s {
            crate::spool::Signal::Logs => self.nisshi.as_ref(),
            crate::spool::Signal::Metrics => self.masu.as_ref(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if !crate::json::is_node(&self.node) {
            return Err(format!(
                "node {:?}: must match [a-z0-9][a-z0-9-]{{0,62}}",
                self.node
            ));
        }
        if self.spool_limit == 0 {
            return Err("spool_limit must be positive".into());
        }
        if !self.listen_metrics.ip().is_loopback() {
            return Err(format!(
                "listen_metrics {}: loopback only",
                self.listen_metrics
            ));
        }
        for (name, t) in &self.targets {
            let (scheme, _) = split_url(&t.url).map_err(|e| format!("targets.{name}.url: {e}"))?;
            if scheme != "http" && scheme != "https" {
                return Err(format!("targets.{name}.url: scheme must be http or https"));
            }
        }
        for (name, f) in &self.files {
            if !f.path.is_absolute() {
                return Err(format!("files.{name}.path: must be absolute"));
            }
        }
        for (name, d) in [("masu", &self.masu), ("nisshi", &self.nisshi)] {
            let Some(d) = d else { continue };
            let (scheme, host) = split_url(&d.url).map_err(|e| format!("{name}.url: {e}"))?;
            if scheme != "https" {
                return Err(format!(
                    "{name}.url: the stores are reached over https (ADR 0006)"
                ));
            }
            if host.parse::<IpAddr>().is_ok() && d.server_name.is_none() {
                return Err(format!("{name}: an IP literal in url needs server_name"));
            }
        }
        Ok(())
    }
}

/// `(scheme, host)` of `scheme://host[:port][/path]`; the host without brackets.
pub fn split_url(url: &str) -> Result<(&str, &str), String> {
    let (scheme, rest) = url.split_once("://").ok_or("missing scheme")?;
    let authority = rest.split('/').next().unwrap_or("");
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        authority.rsplit_once(':').map_or(authority, |(h, _)| h)
    };
    if host.is_empty() {
        return Err("missing host".into());
    }
    Ok((scheme, host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_durations() {
        assert_eq!(
            split_url("https://192.0.2.10:9472"),
            Ok(("https", "192.0.2.10"))
        );
        assert_eq!(split_url("http://[::1]:9100/metrics"), Ok(("http", "::1")));
        assert_eq!(
            split_url("https://masu.example.org/x"),
            Ok(("https", "masu.example.org"))
        );
        assert!(split_url("masu.example.org").is_err());
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("0s"), None);
        assert_eq!(parse_duration("30"), None);
    }
}
