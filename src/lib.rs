//! Argus - The All-Seeing File Search Tool
//!
//! Library crate exposing the search engine, extractors, indexing, and UI
//! primitives that power the `argus` binary. Exposing these as a library
//! allows integration tests and external tools to reuse the search core.

pub mod extractors;
pub mod index;
pub mod search;
pub mod types;
pub mod ui;
