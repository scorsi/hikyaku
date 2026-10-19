//! `hikyaku check <config>`: the only command of M0. Hand-written argument parsing (ADR 0002).

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage: hikyaku check <config.toml>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["check", path] => hikyaku::config::Config::load(Path::new(path)).map(|_| ()),
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
