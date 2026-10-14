//! Crosshair, hotbar and debug overlay.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use rbvg_voxel::streaming::LatencyStats;

use crate::interaction::{Hotbar, Target};
use crate::player::Player;
use crate::scripting::ScriptStats;
use crate::voxel::{Blocks, ChunkRender, GameWorld, Pipeline};

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

/// `avg/p95/max` in milliseconds.
fn latency(s: &LatencyStats) -> String {
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    format!(
        "{:.2}/{:.2}/{:.2}",
        ms(s.average()),
        ms(s.percentile(0.95)),
        ms(s.max())
    )
}

#[allow(clippy::too_many_arguments)]
fn update_debug(
    diagnostics: Res<DiagnosticsStore>,
    time: Res<Time<Real>>,
    mut frames: Local<LatencyStats>,
    world: Res<GameWorld>,
    chunks: Query<&ViewVisibility, With<ChunkRender>>,
    pipeline: Res<Pipeline>,
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
    frames.record(time.delta());
    let visible = chunks.iter().filter(|v| v.get()).count();
    let s = &pipeline.0.stats;
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
         frame ms avg/p95/max {}\n\
         chunks loaded {} / rendered {} / visible {}\n\
         gen pending {} active {}  mesh pending {} active {}\n\
         stale gen {} mesh {}  cancelled gen {}\n\
         ms avg/p95/max: gen {}  mesh {}  apply {}\n\
         script dispatch {:.3} ms  scheduler {:.3} ms ({} pending)  script errors {}\n\
         target {target}\n\
         [click] capture mouse  [Esc] release  [F5] save  [F9] load",
        pos.x,
        pos.y,
        pos.z,
        if p.on_ground { " (ground)" } else { "" },
        latency(&frames),
        world.0.loaded_count(),
        chunks.iter().len(),
        visible,
        s.pending_generation,
        s.active_generation,
        s.pending_meshing,
        s.active_meshing,
        s.stale_generation,
        s.stale_meshes,
        s.cancelled_generation,
        latency(&s.generation),
        latency(&s.meshing),
        latency(&s.apply),
        scripts.last_dispatch.as_secs_f64() * 1000.0,
        scripts.last_tick.as_secs_f64() * 1000.0,
        scripts.scheduled,
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
