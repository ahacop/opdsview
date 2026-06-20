//! opdsview — a terminal UI for browsing OPDS catalogs.
//!
//! The modules are exposed as a library so integration tests and examples can
//! exercise the parsing, caching, and networking logic without a terminal.

pub mod app;
pub mod cache;
pub mod opds;
pub mod storage;
pub mod ui;
pub mod worker;
