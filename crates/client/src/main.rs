//! RBVG desktop client.
//!
//! Loads content and mods, then runs the Bevy app. Gameplay state lives in the
//! renderer-independent `rbvg_voxel` / `rbvg_scripting` crates; this crate wires them to
//! input, rendering and UI.

mod hud;
mod interaction;
mod player;
mod save;
mod scripting;
mod voxel;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use bevy::diagnostic::FrameTimeDiagnosticsPlugin;
use bevy::prelude::*;
use rbvg_scripting::{ModManifest, ScriptHost, ScriptHostConfig};
use rbvg_voxel::BlockRegistry;

/// Filesystem locations used at runtime.
#[derive(Resource, Clone)]
pub struct DataPaths {
    pub save_dir: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut app = App::new();
    // Default plugins first so logging is initialised before content and mods load.
    app.add_plugins((
        DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "RBVG".into(),
                ..default()
            }),
            ..default()
        }),
        FrameTimeDiagnosticsPlugin::default(),
    ));

    let root = std::env::var_os("RBVG_DATA_DIR").map_or_else(|| PathBuf::from("."), PathBuf::from);
    let mut registry = load_content(&root)?;
    let mods = load_mod_content(&root.join("mods"), &mut registry)?;
    let paths = DataPaths {
        save_dir: root.join("saves").join("world"),
    };
    let seed = match std::env::var("RBVG_SEED") {
        Ok(s) => s
            .parse()
            .map_err(|_| format!("RBVG_SEED `{s}` is not a u32"))?,
        Err(_) => 1337,
    };
    let (world, player_pos) = save::load_or_create(&paths.save_dir, &mut registry, seed)?;

    app.insert_resource(paths)
        .insert_resource(voxel::Blocks(Arc::new(registry)))
        .insert_resource(voxel::GameWorld(world))
        .insert_resource(scripting::Scripts(load_scripts(&mods)))
        .add_plugins((
            voxel::VoxelPlugin,
            player::PlayerPlugin {
                spawn: player_pos.map(Vec3::from_array),
            },
            interaction::InteractionPlugin,
            scripting::ScriptingPlugin,
            save::SavePlugin,
            hud::HudPlugin,
        ))
        .run();
    Ok(())
}

fn load_content(root: &Path) -> Result<BlockRegistry, String> {
    let path = root.join("content/core/blocks.toml");
    let src = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {}: {e} (run from the repository root or set RBVG_DATA_DIR)",
            path.display()
        )
    })?;
    let mut registry = BlockRegistry::default();
    registry
        .load_toml("core", &src)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(registry)
}

/// Discovers mods in dependency order and registers their blocks. A mod whose content is
/// invalid is reported and skipped (its scripts are not loaded either).
fn load_mod_content(dir: &Path, registry: &mut BlockRegistry) -> Result<Vec<ModManifest>, String> {
    let mut ok = Vec::new();
    for manifest in ModManifest::discover(dir).map_err(|e| e.to_string())? {
        match manifest.register_blocks(registry) {
            Ok(()) => ok.push(manifest),
            Err(e) => error!("{e}"),
        }
    }
    Ok(ok)
}

fn load_scripts(mods: &[ModManifest]) -> ScriptHost {
    let mut host = ScriptHost::new(ScriptHostConfig::default());
    for manifest in mods {
        // A broken mod is reported but does not prevent the game from starting.
        if let Err(e) = host.load_mod(manifest) {
            error!("{e}");
        }
    }
    host
}
