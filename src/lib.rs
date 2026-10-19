//! hikyaku, the node agent (ADR 0001): the library holds everything the binary and the tests
//! share; `main.rs` only parses arguments.

pub mod agent;
pub mod config;
pub mod files;
pub mod journald;
pub mod json;
pub mod logschema;
pub mod metrics;
pub mod pb;
pub mod record;
pub mod scrape;
pub mod scrape_record;
pub mod spool;
pub mod ts;
