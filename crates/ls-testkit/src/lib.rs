//! The shared LSP test harness (the "ls-testkit").
//!
//! Consolidates the wire-driver code that used to be duplicated across the
//! `ls-server` integration suites (`fake_bsp_e2e`, `server_surface`, the
//! `real_bsp_common` client) into one kit, and adds the two pieces the suites
//! lacked: a scriptable JVM-free [`fake_pc::FakePcService`] that plugs into the
//! REAL serve loop through [`ls_server::IndexBootstrap::with_pc`] (so the
//! PC-backed wire surface — completion, resolve, hover, signature help, the
//! definition family — is testable without booting a JVM), and a
//! [`client::WireClient`] that can also spawn the production binary and drive it
//! over real stdio.
//!
//! Test-only: consumed as a dev-dependency; never part of the shipped server.

pub mod client;
pub mod fake_bsp;
pub mod fake_pc;
pub mod fixtures;
pub mod positions;
pub mod wire;
