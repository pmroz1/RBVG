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
    let registry = load_content(&root)?;
    let host = load_mods(&root.join("mods"))?;
    let paths = DataPaths {
        save_dir: root.join("saves").join("world"),
    };
    let seed = match std::env::var("RBVG_SEED") {
        Ok(s) => s
            .parse()
            .map_err(|_| format!("RBVG_SEED `{s}` is not a u32"))?,
        Err(_) => 1337,
    };
    let (world, player_pos) = save::load_or_create(&paths.save_dir, &registry, seed)?;

    app.insert_resource(paths)
        .insert_resource(voxel::Blocks(Arc::new(registry)))
        .insert_resource(voxel::GameWorld(world))
        .insert_resource(scripting::Scripts(host))
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

fn load_mods(dir: &Path) -> Result<ScriptHost, String> {
    let mut host = ScriptHost::new(ScriptHostConfig::default());
    for manifest in ModManifest::discover(dir).map_err(|e| e.to_string())? {
        // A broken mod is reported but does not prevent the game from starting.
        if let Err(e) = host.load_mod(&manifest) {
            error!("{e}");
        }
    }
    Ok(host)
}
