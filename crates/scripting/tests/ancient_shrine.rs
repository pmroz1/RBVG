//! End-to-end: the `ancient_shrine` mod registers a block, handles an interaction,
//! notifies, schedules a ritual, mutates the world, and the resulting events reach other
//! mods within bounded propagation. Headless.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rbvg_scripting::{
    CommandApplier, Envelope, ModManifest, PumpLimits, ScriptCommand, ScriptEvent, ScriptHost,
    ScriptHostConfig, WorldQuery, pump, resolve_order,
};
use rbvg_voxel::streaming::{ChunkPipeline, StreamConfig};
use rbvg_voxel::{BlockPos, BlockRegistry, TerrainGenerator, VoxelWorld};

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Game {
    registry: BlockRegistry,
    world: VoxelWorld,
    notes: Vec<String>,
    surface: BlockPos,
}

impl WorldQuery for Game {
    fn block(&self, pos: BlockPos) -> Option<&str> {
        self.world
            .block_if_loaded(pos)
            .map(|id| self.registry.name(id))
    }
}

impl CommandApplier for Game {
    fn query(&self) -> &dyn WorldQuery {
        self
    }
    fn apply(&mut self, cmd: ScriptCommand) -> Option<ScriptEvent> {
        match cmd {
            ScriptCommand::Notify { message, .. } => {
                self.notes.push(message);
                None
            }
            ScriptCommand::SetBlock { pos, block, .. } => {
                let id = self.registry.id(&block)?;
                self.world.block_if_loaded(pos)?; // never force-load
                let old = self.world.set_block(pos, id).ok()?;
                (old != id).then(|| {
                    if block == "core:air" {
                        ScriptEvent::BlockBroken {
                            pos,
                            block: self.registry.name(old).into(),
                        }
                    } else {
                        ScriptEvent::BlockPlaced { pos, block }
                    }
                })
            }
        }
    }
}

fn setup(mod_ids: &[&str], extra: &[(&str, &str)]) -> (Game, ScriptHost) {
    let mut registry = BlockRegistry::default();
    let src = std::fs::read_to_string(root().join("content/core/blocks.toml")).unwrap();
    registry.load_toml("core", &src).unwrap();
    let generator = TerrainGenerator::new(7, &registry).unwrap();
    let surface = BlockPos::new(0, generator.surface_height(0, 0), 0);

    let mut manifests = ModManifest::discover(&root().join("mods")).unwrap();
    manifests.retain(|m| mod_ids.contains(&m.id.as_str()));
    for m in &manifests {
        m.register_blocks(&mut registry).unwrap();
    }
    let mut world = VoxelWorld::new(7, Arc::new(generator));
    let mut pipeline = ChunkPipeline::new(StreamConfig {
        radius: 1,
        radius_y: 1,
        ..StreamConfig::default()
    });
    loop {
        let jobs = pipeline.update(&mut world, surface.chunk());
        if jobs.is_empty() {
            break;
        }
        for job in jobs {
            pipeline.finish_generation(&mut world, job.run());
        }
    }
    let mut host = ScriptHost::new(ScriptHostConfig::default());
    for m in &manifests {
        host.load_mod(m).unwrap();
    }
    for (id, src) in extra {
        let mut m = manifests[0].clone();
        m.id = (*id).into();
        host.load_mod_source(&m, src).unwrap();
    }
    let game = Game {
        registry,
        world,
        notes: Vec::new(),
        surface,
    };
    (game, host)
}

fn interact(g: &mut Game, host: &ScriptHost, pos: BlockPos) -> rbvg_scripting::PumpStats {
    let event = ScriptEvent::Interact {
        pos,
        block: g.block(pos).unwrap().into(),
    };
    pump(
        host,
        g,
        vec![Envelope::player(event)],
        vec![],
        PumpLimits::default(),
    )
}

fn tick(g: &mut Game, host: &mut ScriptHost, secs: f64) -> rbvg_scripting::PumpStats {
    let (cmds, s) = host.tick(Duration::from_secs_f64(secs), &*g);
    assert_eq!(s.errors, 0);
    pump(host, g, vec![], cmds, PumpLimits::default())
}

#[test]
fn shrine_ritual_end_to_end() {
    // A second mod observes the shrine's changes through the event system.
    let observer = r#"
        events.subscribe("block_placed", function(e)
            if e.origin == "mod" and e.origin_mod == "ancient_shrine" then
                ui.notify("saw " .. e.block)
            end
        end)
    "#;
    let (mut g, mut host) = setup(&["ancient_shrine"], &[("observer", observer)]);
    let altar = g
        .registry
        .id("ancient_shrine:altar")
        .expect("block registered");
    let altar_pos = g.surface.offset(0, 1, 0);
    g.world.set_block(altar_pos, altar).unwrap();

    let stats = interact(&mut g, &host, altar_pos);
    assert_eq!(stats.errors, 0);
    assert_eq!(g.notes, ["The ancient shrine awakens..."]);
    assert_eq!(host.scheduled_count(), 1);

    // Interacting something else does nothing.
    let ground = g.surface;
    let before = g.notes.len();
    interact(&mut g, &host, ground);
    assert_eq!(g.notes.len(), before);

    let ring = g.surface.offset(1, 0, 0);
    let mesh_rev = g.world.revision(ring.chunk());
    assert_eq!(tick(&mut g, &mut host, 2.9).commands, 0);
    let stats = tick(&mut g, &mut host, 0.2);
    assert_eq!(g.block(ring), Some("ancient_shrine:glowstone"));
    assert!(stats.commands >= 2 && stats.events >= 1, "{stats:?}");
    assert_eq!(stats.dropped_events, 0);
    assert!(g.notes.iter().any(|n| n == "saw ancient_shrine:glowstone"));
    assert_ne!(g.world.revision(ring.chunk()), mesh_rev, "mesh invalidated");
}

#[test]
fn engine_works_without_the_mod() {
    let (mut g, host) = setup(&[], &[]);
    assert!(g.registry.id("ancient_shrine:altar").is_none());
    let p = g.surface;
    let stats = interact(&mut g, &host, p);
    assert_eq!(stats.events, 1);
    assert!(g.notes.is_empty());
}

#[test]
fn cross_mod_feedback_loop_is_bounded() {
    let a = r#"
        events.subscribe("block_broken", function(e) world.set_block(e.position, "core:stone") end)
    "#;
    let b = r#"
        events.subscribe("block_placed", function(e) world.set_block(e.position, "core:air") end)
    "#;
    let (mut g, _) = setup(&[], &[]);
    let mut host = ScriptHost::new(ScriptHostConfig::default());
    for (id, src) in [("a", a), ("b", b)] {
        let m = ModManifest {
            id: id.into(),
            name: id.into(),
            version: "0.1.0".into(),
            api_version: rbvg_scripting::API_VERSION,
            entry: "main.luau".into(),
            capabilities: vec![
                rbvg_scripting::Capability::WorldWrite,
                rbvg_scripting::Capability::WorldRead,
            ],
            dependencies: vec![],
            optional_dependencies: vec![],
            blocks: None,
            dir: Default::default(),
        };
        host.load_mod_source(&m, src).unwrap();
    }
    let p = g.surface;
    let stone = g.registry.id("core:stone").unwrap();
    g.world.set_block(p, stone).unwrap();
    // Seed the loop with a player break.
    g.world.set_block(p, rbvg_voxel::BlockId::AIR).unwrap();
    let ev = ScriptEvent::BlockBroken {
        pos: p,
        block: "core:stone".into(),
    };
    let limits = PumpLimits::default();
    let stats = pump(&host, &mut g, vec![Envelope::player(ev)], vec![], limits);
    assert!(stats.dropped_events > 0, "cascade must be cut: {stats:?}");
    assert!(stats.events <= limits.max_events);
    assert!(stats.waves <= limits.max_depth + 2);
}

#[test]
fn before_block_break_can_be_cancelled_deterministically() {
    let (g, _) = setup(&[], &[]);
    let mut host = ScriptHost::new(ScriptHostConfig::default());
    let m = |id: &str| ModManifest {
        id: id.into(),
        name: id.into(),
        version: "0.1.0".into(),
        api_version: rbvg_scripting::API_VERSION,
        entry: "main.luau".into(),
        capabilities: vec![],
        dependencies: vec![],
        optional_dependencies: vec![],
        blocks: None,
        dir: Default::default(),
    };
    host.load_mod_source(
        &m("guard"),
        r#"events.subscribe("before_block_break", function(e)
            if e.block == "core:stone" then e.cancel() end end)"#,
    )
    .unwrap();
    let ev = |block: &str| ScriptEvent::BeforeBlockBreak {
        pos: g.surface,
        block: block.into(),
    };
    assert!(host.dispatch(&ev("core:stone"), &g).1.cancelled);
    assert!(!host.dispatch(&ev("core:dirt"), &g).1.cancelled);
    // Notifications cannot be cancelled.
    host.load_mod_source(
        &m("bad"),
        r#"events.subscribe("block_broken", function(e) e.cancel() end)"#,
    )
    .unwrap();
    let broken = ScriptEvent::BlockBroken {
        pos: g.surface,
        block: "core:stone".into(),
    };
    let (_, s) = host.dispatch(&broken, &g);
    assert!(!s.cancelled && s.errors == 1);
}

#[test]
fn dependency_order_is_deterministic() {
    let mods = ModManifest::discover(&root().join("mods")).unwrap();
    let again = resolve_order(mods.clone()).unwrap();
    assert_eq!(
        mods.iter().map(|m| &m.id).collect::<Vec<_>>(),
        again.iter().map(|m| &m.id).collect::<Vec<_>>()
    );
}
