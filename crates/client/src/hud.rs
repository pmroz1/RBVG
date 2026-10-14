//! Crosshair, hotbar and debug overlay.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use rbvg_voxel::streaming::LatencyStats;

use crate::interaction::{CreativeMenu, Hotbar, Target};
use crate::player::Player;
use crate::scripting::{Notification, ScriptStats};
use crate::voxel::{Blocks, ChunkRender, GameWorld, Pipeline};

#[derive(Component)]
struct DebugText;

#[derive(Component)]
struct HotbarText;

#[derive(Component)]
struct CreativeText;

/// Script notification line; hidden when the timer runs out.
#[derive(Component)]
struct NoticeText(Timer);

const NOTICE_SECONDS: f32 = 4.0;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hud).add_systems(
            Update,
            (update_debug, update_hotbar, update_notice, update_creative)
                .after(crate::scripting::ScriptingSystems),
        );
    }
}

fn spawn_hud(mut commands: Commands) {
    commands.spawn((
        CreativeText,
        Text::default(),
        TextFont::from_font_size(18.0),
        BackgroundColor(Color::srgba(0.02, 0.02, 0.04, 0.95)),
        Node {
            position_type: PositionType::Absolute,
            left: percent(20),
            top: percent(25),
            padding: UiRect::all(px(16)),
            ..default()
        },
        Visibility::Hidden,
        ZIndex(10),
    ));
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
        NoticeText(Timer::from_seconds(0.0, TimerMode::Once)),
        Text::default(),
        TextFont::from_font_size(22.0),
        Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            top: px(60),
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
         [click] capture mouse  [Esc] release  [B] creative  [E] interact  [F5] save  [F9] load",
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
    if !hotbar.is_changed() && !blocks.is_changed() {
        return;
    }
    text.0 = hotbar
        .slots
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let name = if *id == rbvg_voxel::BlockId::AIR {
                "Empty"
            } else {
                blocks.0.get(*id).map_or("?", |d| d.display_name.as_str())
            };
            if i == hotbar.selected {
                format!("[{} {name}]", i + 1)
            } else {
                format!(" {} {name} ", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
}

fn update_creative(
    menu: Res<CreativeMenu>,
    hotbar: Res<Hotbar>,
    blocks: Res<Blocks>,
    mut panel: Single<(&mut Text, &mut Visibility), With<CreativeText>>,
) {
    if !menu.is_changed() && !hotbar.is_changed() && !blocks.is_changed() {
        return;
    }
    let (text, visibility) = &mut *panel;
    **visibility = if menu.open {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if !menu.open {
        return;
    }
    let entries = rbvg_voxel::selection::catalog(&blocks.0);
    text.0 = format!(
        "Creative blocks — slot {}\n\
             ↑/↓ scroll · PgUp/PgDn page · 1–9 slot · Enter assign · B/Esc close\n",
        hotbar.selected + 1,
    );
    let start = menu.index / 12 * 12;
    for (i, (_, def)) in entries.iter().enumerate().skip(start).take(12) {
        text.0.push_str(&format!(
            "\n{} {} ({}){}",
            if i == menu.index { ">" } else { " " },
            def.display_name,
            def.id,
            if def.missing {
                " — MISSING, unavailable"
            } else {
                ""
            },
        ));
    }
    text.0
        .push_str(&format!("\n\n{} registered blocks", entries.len()));
}

fn update_notice(
    time: Res<Time>,
    mut notes: MessageReader<Notification>,
    mut notice: Single<(&mut NoticeText, &mut Text)>,
) {
    let (timer, text) = &mut *notice;
    if let Some(n) = notes.read().last() {
        text.0 = n.0.clone();
        timer.0 = Timer::from_seconds(NOTICE_SECONDS, TimerMode::Once);
    } else if !timer.0.is_finished() && timer.0.tick(time.delta()).just_finished() {
        text.0.clear();
    }
}
