//! Loads the shipped example mod from `mods/` and drives it with real events.

use std::path::Path;

use rbvg_scripting::{ModManifest, ScriptCommand, ScriptEvent, ScriptHost, ScriptHostConfig};
use rbvg_voxel::BlockPos;

#[test]
fn example_logger_restores_every_tenth_block() {
    let mods = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mods");
    let manifests = ModManifest::discover(&mods).unwrap();
    let manifest = manifests
        .iter()
        .find(|m| m.id == "example_logger")
        .expect("example mod present");
    let mut host = ScriptHost::new(ScriptHostConfig::default());
    host.load_mod(manifest).unwrap();

    let mut commands = Vec::new();
    for i in 0..10 {
        let event = ScriptEvent::BlockBroken {
            pos: BlockPos::new(i, -4, 2),
            block: "core:dirt".into(),
        };
        let (mut cmds, stats) = host.dispatch(&event);
        assert_eq!(stats.errors, 0);
        commands.append(&mut cmds);
    }
    assert_eq!(
        commands,
        vec![ScriptCommand::SetBlock {
            mod_id: "example_logger".into(),
            pos: BlockPos::new(9, -4, 2),
            block: "core:stone".into(),
        }]
    );
}
