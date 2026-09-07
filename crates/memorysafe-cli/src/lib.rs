//! The `msafe` command line, as a library so integration tests can assemble
//! the same pieces the binary does. `src/main.rs` is a thin `main` over this.

pub mod build;
pub mod cmd;
pub mod config;
pub mod render;
