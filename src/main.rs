//! `hikyaku check <config>`, `hikyaku run <config>`, `hikyaku spool dump [--spool <dir>]
//! [--signal logs|metrics] [--from <seq>] [--to <seq>]`. Hand-written parsing (ADR 0002).

use hikyaku::config::Config;
use hikyaku::spool::{self, Signal};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "usage: hikyaku check <config.toml>
       hikyaku run <config.toml>
       hikyaku spool dump [--spool <dir>] [--signal logs|metrics] [--from <seq>] [--to <seq>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args[..] {
        ["check", path] => Config::load(Path::new(path)).map(|_| ()),
        ["run", path] => Config::load(Path::new(path)).and_then(hikyaku::agent::run),
        ["spool", "dump", ref rest @ ..] => dump(rest),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hikyaku: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dump(args: &[&str]) -> Result<(), String> {
    let mut dir = PathBuf::from("/var/lib/hikyaku");
    let mut signal = None;
    let (mut from, mut to) = (0u64, u64::MAX);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it
            .next()
            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))?;
        match *flag {
            "--spool" => dir = PathBuf::from(value),
            "--signal" => {
                signal =
                    Some(Signal::parse(value).ok_or_else(|| format!("unknown signal {value}"))?)
            }
            "--from" => from = value.parse().map_err(|_| format!("bad --from {value}"))?,
            "--to" => to = value.parse().map_err(|_| format!("bad --to {value}"))?,
            _ => return Err(USAGE.to_string()),
        }
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    spool::dump(&dir, signal, from, to, &mut out)
        .map_err(|e| format!("spool {}: {e}", dir.display()))
}
