//! Every line of nisshi's fixtures/logs/ and masu's fixtures/records/ through hikyaku's
//! validators: same verdict, same reason (ADR 0008). The directories come from the flake inputs
//! (`HIKYAKU_NISSHI_FIXTURES`, `HIKYAKU_MASU_FIXTURES`), defaulting to sibling checkouts.

use serde_json::Value;
use std::path::PathBuf;

fn fixtures(var: &str, dflt: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| PathBuf::from(dflt), PathBuf::from)
}

/// The bytes under test: `input`, `input_b64`, or the generated form (`input_repeat` in nisshi,
/// `generate` in masu: prefix + repeat × count + suffix).
fn input(line: &Value) -> Vec<u8> {
    let forms = ["input", "input_b64", "input_repeat", "generate"];
    let present = forms.iter().filter(|f| line.get(**f).is_some()).count();
    assert_eq!(present, 1, "exactly one input form: {line}");
    if let Some(s) = line["input"].as_str() {
        return s.as_bytes().to_vec();
    }
    if let Some(s) = line["input_b64"].as_str() {
        return base64(s);
    }
    let g = line.get("input_repeat").unwrap_or(&line["generate"]);
    let fill = g.get("repeat").or(g.get("fill")).unwrap().as_str().unwrap();
    let count = g["count"].as_u64().unwrap() as usize;
    [
        g["prefix"].as_str().unwrap(),
        &fill.repeat(count),
        g["suffix"].as_str().unwrap(),
    ]
    .concat()
    .into_bytes()
}

/// Standard base64 with padding (RFC 4648 section 4).
fn base64(s: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    assert!(s.len().is_multiple_of(4), "base64 length");
    let mut out = Vec::new();
    for chunk in s.as_bytes().chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = n << 6
                | ALPHABET
                    .iter()
                    .position(|&a| a == c)
                    .expect("base64 alphabet") as u32;
        }
        n <<= 6 * pad;
        out.extend_from_slice(&n.to_be_bytes()[1..4 - pad]);
    }
    out
}

fn run(path: PathBuf, check: fn(&[u8]) -> Result<(), String>) -> usize {
    let file = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut n = 0;
    for (i, text) in file.lines().enumerate() {
        let line: Value = serde_json::from_str(text).unwrap();
        let valid = line["valid"].as_bool().unwrap();
        let reason = line["reason"].as_str().unwrap();
        let want = if valid {
            Ok(())
        } else {
            Err(reason.to_string())
        };
        assert_eq!(check(&input(&line)), want, "{}:{}", path.display(), i + 1);
        n += 1;
    }
    n
}

#[test]
fn nisshi_service_lines() {
    let dir = fixtures("HIKYAKU_NISSHI_FIXTURES", "../nisshi/fixtures");
    assert!(
        run(
            dir.join("logs/service-lines.ndjson"),
            hikyaku::logschema::check_line
        ) > 40
    );
}

#[test]
fn nisshi_records() {
    let dir = fixtures("HIKYAKU_NISSHI_FIXTURES", "../nisshi/fixtures");
    assert!(
        run(
            dir.join("logs/records.ndjson"),
            hikyaku::logschema::check_record
        ) > 40
    );
}

#[test]
fn masu_records() {
    let dir = fixtures("HIKYAKU_MASU_FIXTURES", "../masu/fixtures");
    assert!(
        run(
            dir.join("records/records.ndjson"),
            hikyaku::scrape_record::check
        ) > 70
    );
}

#[test]
fn base64_decodes() {
    assert_eq!(base64("/w=="), [0xff]);
    assert_eq!(base64("eyJ2IjoxfQ=="), br#"{"v":1}"#);
    assert_eq!(base64("//4="), [0xff, 0xfe]);
}
