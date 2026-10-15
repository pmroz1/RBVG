//! First-person player controller: mouse look, walking, sprinting, jumping, gravity and
//! voxel collision.

use std::f32::consts::FRAC_PI_2;

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use rbvg_voxel::physics::{Aabb, move_aabb};
use rbvg_voxel::{BlockPos, TerrainGenerator};

use crate::voxel::{Blocks, GameWorld, chunk_at};

pub const EYE_HEIGHT: f32 = 1.62;
pub const HALF_WIDTH: f32 = 0.3;
pub const HEIGHT: f32 = 1.8;
const WALK_SPEED: f32 = 4.3;
const SPRINT_SPEED: f32 = 6.5;
const JUMP_SPEED: f32 = 8.4;
const GRAVITY: f32 = 28.0;
const MAX_FALL_SPEED: f32 = 60.0;
/// Longest simulated frame; hitches beyond this slow the player instead of teleporting.
const MAX_DT: f32 = 0.05;
const MOUSE_SENSITIVITY: f32 = 0.002;

#[derive(Component, Default)]
pub struct Player {
    pub velocity: Vec3,
    pub on_ground: bool,
}

impl Player {
    /// Feet position from the camera (eye) position.
    pub fn feet(eye: Vec3) -> Vec3 {
        eye - Vec3::Y * EYE_HEIGHT
    }

    pub fn aabb(eye: Vec3) -> Aabb {
        Aabb::from_feet(Self::feet(eye).to_array(), HALF_WIDTH, HEIGHT)
    }
}

/// Whether the mouse is captured for gameplay (look + block interaction).
pub fn cursor_captured(cursor: &CursorOptions) -> bool {
    cursor.grab_mode != CursorGrabMode::None
}

pub struct PlayerPlugin {
    /// Saved feet position; `None` spawns above the terrain at the origin.
    pub spawn: Option<Vec3>,
}

#[derive(Resource)]
struct SpawnPoint(Option<Vec3>);

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlayerSystems;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SpawnPoint(self.spawn))
            .add_systems(Startup, spawn_player)
            .add_systems(
                Update,
                (grab_cursor, look, move_player)
                    .chain()
                    .in_set(PlayerSystems)
                    .before(crate::interaction::InteractionSystems),
            );
    }
}

/// Feet position standing on the generated terrain at `(x, z)`, above any trunk.
pub fn surface_spawn(world: &GameWorld, blocks: &Blocks, x: i32, z: i32) -> Vec3 {
    let y = TerrainGenerator::new(world.0.seed(), &blocks.0).map_or(64, |g| {
        let solid = |y| blocks.0.is_solid(g.block_at(BlockPos::new(x, y, z)));
        let mut y = g.surface_height(x, z) + 1;
        while solid(y) || solid(y + 1) {
            y += 1;
        }
        y
    });
    Vec3::new(x as f32 + 0.5, y as f32, z as f32 + 0.5)
}

fn spawn_player(
    mut commands: Commands,
    spawn: Res<SpawnPoint>,
    world: Res<GameWorld>,
    blocks: Res<Blocks>,
) {
    let feet = spawn
        .0
        .unwrap_or_else(|| surface_spawn(&world, &blocks, 0, 0));
    commands.spawn((
        Player::default(),
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: 75f32.to_radians(),
            ..default()
        }),
        Transform::from_translation(feet + Vec3::Y * EYE_HEIGHT),
    ));
}

fn grab_cursor(
    mut cursor: Single<&mut CursorOptions>,
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    menu: Res<crate::interaction::CreativeMenu>,
) {
    if !menu.open && mouse.just_pressed(MouseButton::Left) && !cursor_captured(&cursor) {
        cursor.visible = false;
        cursor.grab_mode = CursorGrabMode::Locked;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.visible = true;
        cursor.grab_mode = CursorGrabMode::None;
    }
}

fn look(
    motion: Res<AccumulatedMouseMotion>,
    cursor: Single<&CursorOptions>,
    mut player: Single<&mut Transform, With<Player>>,
) {
    if motion.delta == Vec2::ZERO || !cursor_captured(&cursor) {
        return;
    }
    // Mouse deltas are already per-frame totals, so no delta-time scaling.
    let (yaw, pitch, _) = player.rotation.to_euler(EulerRot::YXZ);
    let yaw = yaw - motion.delta.x * MOUSE_SENSITIVITY;
    let pitch =
        (pitch - motion.delta.y * MOUSE_SENSITIVITY).clamp(-FRAC_PI_2 + 0.01, FRAC_PI_2 - 0.01);
    player.rotation = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0);
}

fn move_player(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    cursor: Single<&CursorOptions>,
    world: Res<GameWorld>,
    blocks: Res<Blocks>,
    player: Single<(&mut Transform, &mut Player)>,
) {
    let (mut transform, mut player) = player.into_inner();
    let dt = time.delta_secs().min(MAX_DT);
    let eye = transform.translation;

    // Do not simulate until the terrain around the player exists, or we would fall through.
    let here = chunk_at(eye);
    if !world.0.is_loaded(here) || !world.0.is_loaded(here.offset(0, -1, 0)) {
        return;
    }

    let mut wish = Vec3::ZERO;
    if cursor_captured(&cursor) {
        let forward = transform.forward().with_y(0.0).normalize_or_zero();
        let right = transform.right().with_y(0.0).normalize_or_zero();
        for (key, dir) in [
            (KeyCode::KeyW, forward),
            (KeyCode::KeyS, -forward),
            (KeyCode::KeyD, right),
            (KeyCode::KeyA, -right),
        ] {
            if keys.pressed(key) {
                wish += dir;
            }
        }
        if keys.just_pressed(KeyCode::Space) && player.on_ground {
            player.velocity.y = JUMP_SPEED;
        }
    }
    let speed = if keys.pressed(KeyCode::ShiftLeft) {
        SPRINT_SPEED
    } else {
        WALK_SPEED
    };
    let horizontal = wish.normalize_or_zero() * speed;
    player.velocity.x = horizontal.x;
    player.velocity.z = horizontal.z;
    player.velocity.y = (player.velocity.y - GRAVITY * dt).max(-MAX_FALL_SPEED);

    let solid = |p: BlockPos| blocks.0.is_solid(world.0.get_block(p));
    let result = move_aabb(Player::aabb(eye), (player.velocity * dt).to_array(), solid);
    player.on_ground = result.blocked[1] && player.velocity.y <= 0.0;
    if result.blocked[1] {
        player.velocity.y = 0.0;
    }
    let min = Vec3::from_array(result.aabb.min);
    transform.translation = Vec3::new(min.x + HALF_WIDTH, min.y + EYE_HEIGHT, min.z + HALF_WIDTH);

    // Fell out of the world (e.g. dug through the bottom of loaded terrain): respawn.
    if transform.translation.y < -64.0 {
        let feet = surface_spawn(&world, &blocks, 0, 0);
        transform.translation = feet + Vec3::Y * EYE_HEIGHT;
        player.velocity = Vec3::ZERO;
    }
}
