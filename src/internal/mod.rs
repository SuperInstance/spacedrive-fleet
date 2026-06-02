//! Internal stubs for external dependencies.
//!
//! In-production: links against `cocapn-core` and `sd-core` directly.
//! Standalone: these stubs provide the minimal surface area to compile
//! and test the fleet logic independently.
pub mod cocapn_core;
pub mod sd_core;
