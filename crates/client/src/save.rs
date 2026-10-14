//! F5 saves, F9 reloads the last save. The save is also loaded at startup.

use std::path::Path;

use bevy::prelude::*;
use rbvg_voxel::persistence::{self, PersistError};
use rbvg_voxel::{BlockRegistry, TerrainGenerator, VoxelWorld};

use crate::DataPaths;
use crate::player::{EYE_HEIGHT, Player};
use crate::scripting::Scripts;
use crate::voxel::{Blocks, ChunkEntities, GameWorld, Pipeline};

pub struct SavePlugin;

impl Plugin for SavePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, save_or_load);
    }
}

fn new_world(registry: &BlockRegistry, seed: u32) -> Result<VoxelWorld, String> {
    let generator = TerrainGenerator::new(seed, registry).map_err(|e| e.to_string())?;
    Ok(VoxelWorld::new(seed, std::sync::Arc::new(generator)))
}

/// Loads the save in `dir`, or creates a new world with `seed` if there is none.
/// Returns the world and the saved player feet position.
pub fn load_or_create(
    dir: &Path,
    registry: &BlockRegistry,
    seed: u32,
) -> Result<(VoxelWorld, Option<[f32; 3]>), String> {
    match load(dir, registry) {
        Ok(loaded) => {
            info!("loaded world from {}", dir.display());
            Ok(loaded)
        }
        Err(PersistError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            info!(
                "no save at {}, generating world with seed {seed}",
                dir.display()
            );
            Ok((new_world(registry, seed)?, None))
        }
        // Refuse to start rather than risk overwriting a save we could not read.
        Err(e) => Err(format!(
            "cannot load save {}: {e} (move it away to start a new world)",
            dir.display()
        )),
    }
}

fn load(
    dir: &Path,
    registry: &BlockRegistry,
) -> Result<(VoxelWorld, Option<[f32; 3]>), PersistError> {
    let meta = persistence::load_meta(dir)?;
    let chunks = persistence::load_chunks(dir, registry)?;
    let generator = TerrainGenerator::new(meta.seed, registry)
        .map_err(|e| PersistError::UnknownBlock(e.to_string()))?;
    let mut world = VoxelWorld::new(meta.seed, std::sync::Arc::new(generator));
    for (pos, chunk) in chunks {
        world.insert_saved(pos, chunk);
    }
    Ok((world, meta.player_position))
}

fn save_or_load(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    paths: Res<DataPaths>,
    blocks: Res<Blocks>,
    mut world: ResMut<GameWorld>,
    mut entities: ResMut<ChunkEntities>,
    mut pipeline: ResMut<Pipeline>,
    mut scripts: ResMut<Scripts>,
    player: Single<(&mut Transform, &mut Player)>,
) {
    let (mut transform, mut player) = player.into_inner();
    if keys.just_pressed(KeyCode::F5) {
        let feet = Player::feet(transform.translation).to_array();
        match persistence::save_world(&paths.save_dir, &world.0, &blocks.0, Some(feet)) {
            Ok(()) => info!("saved world to {}", paths.save_dir.display()),
            Err(e) => error!("save failed: {e}"),
        }
    } else if keys.just_pressed(KeyCode::F9) {
        match load(&paths.save_dir, &blocks.0) {
            Ok((loaded, feet)) => {
                world.0 = loaded;
                // In-flight chunk work and scheduled script callbacks belong to the old
                // world; late results are rejected by ticket.
                pipeline.0.reset();
                scripts.0.clear_scheduled();
                for (_, e) in entities.0.drain() {
                    commands.entity(e).despawn();
                }
                if let Some(feet) = feet {
                    transform.translation = Vec3::from_array(feet) + Vec3::Y * EYE_HEIGHT;
                    player.velocity = Vec3::ZERO;
                }
                info!("reloaded world from {}", paths.save_dir.display());
            }
            Err(e) => error!("load failed: {e}"),
        }
    }
}
