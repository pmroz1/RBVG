//! Crosshair, hotbar and debug overlay.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;

use crate::interaction::{Hotbar, Target};
use crate::player::Player;
use crate::scripting::ScriptStats;
use crate::voxel::{Blocks, ChunkEntities, GameWorld, VoxelStats};

#[derive(Component)]
struct DebugText;

#[derive(Component)]
struct HotbarText;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hud)
            .add_systems(Update, (update_debug, update_hotbar));
    }
}

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        Text::new("+"),
        TextFont::from_font_size(24.0),
        Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            top: percent(50),
            margin: UiRect::px(-6.0, 0.0, -14.0, 0.0),
            ..default()
        },
    ));
    commands.spawn((
        DebugText,
        Text::default(),
        TextFont::from_font_size(14.0),
        Node {
            position_type: PositionType::Absolute,
            left: px(8),
            top: px(8),
            ..default()
        },
    ));
    commands.spawn((
        HotbarText,
        Text::default(),
        TextFont::from_font_size(18.0),
        Node {
            position_type: PositionType::Absolute,
            left: px(8),
            bottom: px(8),
            ..default()
        },
    ));
}

fn update_debug(
    diagnostics: Res<DiagnosticsStore>,
    world: Res<GameWorld>,
    entities: Res<ChunkEntities>,
    stats: Res<VoxelStats>,
    scripts: Res<ScriptStats>,
    target: Res<Target>,
    blocks: Res<Blocks>,
    player: Single<(&Transform, &Player)>,
    mut text: Single<&mut Text, With<DebugText>>,
) {
    let fps = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let frame_ms = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FRAME_TIME)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    let (transform, p) = player.into_inner();
    let pos = Player::feet(transform.translation);
    let target = target.0.map_or_else(
        || "-".to_owned(),
        |h| {
            format!(
                "{} at ({}, {}, {})",
                blocks.0.name(world.0.get_block(h.block)),
                h.block.x,
                h.block.y,
                h.block.z
            )
        },
    );
    text.0 = format!(
        "FPS {fps:.0} ({frame_ms:.1} ms)\n\
         pos {:.1} {:.1} {:.1}{}\n\
         chunks loaded {} / rendered {} / pending load {} / pending mesh {}\n\
         gen {:.2} ms  mesh {:.2} ms ({} chunks)\n\
         script dispatch {:.3} ms  script errors {}\n\
         target {target}\n\
         [click] capture mouse  [Esc] release  [F5] save  [F9] load",
        pos.x,
        pos.y,
        pos.z,
        if p.on_ground { " (ground)" } else { "" },
        world.0.loaded_count(),
        entities.0.len(),
        stats.pending_loads,
        world.0.dirty_count(),
        stats.last_generation.as_secs_f64() * 1000.0,
        stats.last_meshing.as_secs_f64() * 1000.0,
        stats.meshed_last_frame,
        scripts.last_dispatch.as_secs_f64() * 1000.0,
        scripts.errors,
    );
}

fn update_hotbar(
    hotbar: Res<Hotbar>,
    blocks: Res<Blocks>,
    mut text: Single<&mut Text, With<HotbarText>>,
) {
    if !hotbar.is_changed() {
        return;
    }
    text.0 = hotbar
        .slots
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let name = blocks.0.get(*id).map_or("?", |d| d.display_name.as_str());
            if i == hotbar.selected {
                format!("[{} {name}]", i + 1)
            } else {
                format!(" {} {name} ", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
}
