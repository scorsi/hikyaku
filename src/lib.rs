//! hikyaku, the node agent (ADR 0001): the library holds everything the binary and the tests
//! share; `main.rs` only parses arguments.

pub mod config;
pub mod json;
pub mod logschema;
pub mod pb;
pub mod record;
pub mod scrape_record;
pub mod spool;
pub mod ts;
