//! BSP client foundation: `.bsp/*.json` connection-file discovery, the Scala 3
//! build-target project model with exact dependency-graph queries, and
//! SemanticDB-flag extraction. A behavior-preserving port of the discovery /
//! project-model / flags layer of the Scala `ls.bsp` module.

pub mod client;
pub mod discovery;
pub mod errors;
mod jsonrpc;
pub mod loader;
pub mod model;
pub mod protocol;
pub mod semanticdb;
pub mod session;
pub mod uri;
pub mod wire;

pub use client::BspClientHandlers;
pub use discovery::{BspConnectionDetails, BspConnectionFile, BspDiscovery, BspDiscoveryResult};
pub use errors::BspError;
pub use loader::ProjectModelLoader;
pub use model::{BspProjectModel, BspTarget};
pub use semanticdb::{SemanticdbConfig, SemanticdbFlags};
pub use session::{BspCompileOutcome, BspSession, BspSessionConfig};
