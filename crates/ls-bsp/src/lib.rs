//! BSP client foundation: `.bsp/*.json` connection-file discovery, the Scala 3
//! build-target project model with exact dependency-graph queries, and
//! SemanticDB-flag extraction. A behavior-preserving port of the discovery /
//! project-model / flags layer of the Scala `ls.bsp` module.

pub mod discovery;
pub mod errors;
pub mod model;
pub mod semanticdb;

pub use discovery::{BspConnectionDetails, BspConnectionFile, BspDiscovery, BspDiscoveryResult};
pub use errors::BspError;
pub use model::{BspProjectModel, BspTarget};
pub use semanticdb::{SemanticdbConfig, SemanticdbFlags};
