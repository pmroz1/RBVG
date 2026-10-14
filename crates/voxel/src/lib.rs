//! Renderer-independent voxel world.
//!
//! Nothing in this crate depends on Bevy, so the simulation can run headless
//! (tests, a future dedicated server) and the client only consumes its output.

pub mod chunk;
pub mod coords;
pub mod generation;
pub mod meshing;
pub mod persistence;
pub mod physics;
pub mod raycast;
pub mod registry;
pub mod selection;
pub mod streaming;
pub mod world;

pub use chunk::Chunk;
pub use coords::{BlockPos, CHUNK_SIZE, ChunkPos, LocalPos};
pub use generation::{TerrainGenerator, WorldGenerator};
pub use registry::{BlockDef, BlockId, BlockRegistry, RegistryError};
pub use world::VoxelWorld;
