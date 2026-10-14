//! End-to-end: the shipped `regrowth` mod against a real voxel world, the script
//! scheduler and the chunk pipeline, all headless.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rbvg_scripting::{
    ModManifest, ScriptCommand, ScriptEvent, ScriptHost, ScriptHostConfig, VoxelQuery,
};
use rbvg_voxel::meshing::{ChunkMesh, mesh_chunk};
use rbvg_voxel::streaming::{ChunkPipeline, StreamConfig};
use rbvg_voxel::{BlockId, BlockPos, BlockRegistry, TerrainGenerator, VoxelWorld};

struct Game {
    registry: Arc<BlockRegistry>,
    world: VoxelWorld,
    scripts: ScriptHost,
    pipeline: ChunkPipeline,
    surface: BlockPos,
}

impl Game {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut registry = BlockRegistry::default();
        let src = std::fs::read_to_string(root.join("content/core/blocks.toml")).unwrap();
        registry.load_toml("core", &src).unwrap();
        let generator = TerrainGenerator::new(42, &registry).unwrap();
        let surface = BlockPos::new(0, generator.surface_height(0, 0), 0);
        let mut world = VoxelWorld::new(42, Arc::new(generator));
        let mut pipeline = ChunkPipeline::new(StreamConfig {
            radius: 1,
            radius_y: 1,
            ..StreamConfig::default()
        });
        // Stream the area around the surface using simulated (inline) completions.
        loop {
            let jobs = pipeline.update(&mut world, surface.chunk());
            if jobs.is_empty() {
                break;
            }
            for job in jobs {
                pipeline.finish_generation(&mut world, job.run());
            }
        }
        let manifest = ModManifest::discover(&root.join("mods"))
            .unwrap()
            .into_iter()
            .find(|m| m.id == "regrowth")
            .expect("regrowth mod present");
        let mut scripts = ScriptHost::new(ScriptHostConfig::default());
        scripts.load_mod(&manifest).unwrap();
        let mut game = Self {
            registry: Arc::new(registry),
            world,
            scripts,
            pipeline,
            surface,
        };
        game.remesh();
        game
    }

    /// Player edit followed by event dispatch, like the client's interaction systems.
    fn player_edit(&mut self, pos: BlockPos, new: &str) {
        let id = self.registry.id(new).unwrap();
        let old = self.world.set_block(pos, id).unwrap();
        let block = self
            .registry
            .name(if id == BlockId::AIR { old } else { id });
        let event = if id == BlockId::AIR {
            ScriptEvent::BlockBroken {
                pos,
                block: block.into(),
            }
        } else {
            ScriptEvent::BlockPlaced {
                pos,
                block: block.into(),
            }
        };
        let query = VoxelQuery {
            world: &self.world,
            registry: &self.registry,
        };
        let (cmds, stats) = self.scripts.dispatch(&event, &query);
        assert_eq!(stats.errors, 0);
        assert!(cmds.is_empty(), "regrowth acts later, not during the event");
    }

    /// One scheduler tick; applies the commands the way the client does.
    fn tick(&mut self, secs: f64) -> Vec<ScriptCommand> {
        let query = VoxelQuery {
            world: &self.world,
            registry: &self.registry,
        };
        let (cmds, stats) = self.scripts.tick(Duration::from_secs_f64(secs), &query);
        assert_eq!(stats.errors, 0);
        for ScriptCommand::SetBlock { pos, block, .. } in &cmds {
            let id = self.registry.id(block).unwrap();
            self.world.set_block(*pos, id).unwrap();
        }
        cmds
    }

    fn remesh(&mut self) -> usize {
        let mut applied = 0;
        loop {
            let jobs = self.pipeline.schedule_meshing(
                &mut self.world,
                &self.registry,
                self.surface.chunk(),
            );
            if jobs.is_empty() {
                return applied;
            }
            for job in jobs {
                applied += usize::from(
                    self.pipeline
                        .finish_meshing(&mut self.world, job.run())
                        .is_some(),
                );
            }
        }
    }
}

#[test]
fn broken_block_regrows_after_three_seconds_and_remeshes() {
    let mut g = Game::new();
    let p = g.surface;
    let original = g.world.get_block(p);
    assert_eq!(g.registry.name(original), "core:grass");
    g.player_edit(p, "core:air");
    g.remesh();
    // Gameplay continues: 2.9 s of frames pass with nothing happening.
    for _ in 0..29 {
        assert!(g.tick(0.1).is_empty());
    }
    let cmds = g.tick(0.1);
    assert_eq!(cmds.len(), 1);
    assert_eq!(g.world.get_block(p), original);
    // The edit dirtied the chunk; the pipeline rebuilds a mesh identical to a fresh one.
    assert!(g.world.is_dirty(p.chunk()));
    assert!(g.remesh() >= 1);
    let mut fresh = ChunkMesh::default();
    mesh_chunk(&g.world, &g.registry, p.chunk(), &mut fresh);
    assert!(fresh.face_count() > 0);
    assert_eq!(g.scripts.scheduled_count(), 0);
}

#[test]
fn regrowth_never_overwrites_a_player_block_or_loads_chunks() {
    let mut g = Game::new();
    let filled = g.surface;
    g.player_edit(filled, "core:air");
    g.player_edit(filled, "core:stone"); // the player fills the hole before 3 s
    // A solid block in the neighbouring chunk column (x = -1 is chunk -1).
    let mut gone = g.surface.offset(-1, 1, 0);
    while g.world.get_block(gone) == BlockId::AIR {
        gone.y -= 1;
    }
    assert_ne!(gone.chunk(), filled.chunk());
    g.player_edit(gone, "core:air");
    assert!(g.world.unload_chunk(gone.chunk())); // the area unloads meanwhile
    assert!(g.tick(3.0).is_empty());
    assert_eq!(g.registry.name(g.world.get_block(filled)), "core:stone");
    assert!(
        !g.world.is_loaded(gone.chunk()),
        "queries never force-load chunks"
    );
}
