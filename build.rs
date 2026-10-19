//! Compiles the stores' ingestion protos (ADR 0001): their directories come from the flake
//! inputs through `MASU_PROTO_DIR` and `NISSHI_PROTO_DIR`, defaulting to sibling checkouts.
fn main() -> std::io::Result<()> {
    let dir = |var: &str, dflt: &str| std::env::var(var).unwrap_or_else(|_| dflt.to_string());
    println!("cargo:rerun-if-env-changed=MASU_PROTO_DIR");
    println!("cargo:rerun-if-env-changed=NISSHI_PROTO_DIR");
    let masu = dir("MASU_PROTO_DIR", "../masu/proto");
    let nisshi = dir("NISSHI_PROTO_DIR", "../nisshi/proto");
    prost_build::compile_protos(
        &[
            format!("{masu}/masu/v1/ingest.proto"),
            format!("{nisshi}/nisshi/v1/ingest.proto"),
        ],
        &[masu, nisshi],
    )
}
