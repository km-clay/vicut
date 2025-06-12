use crate::{linebuf::LineBuf, modes::{insert::ViInsert, normal::ViNormal, ViMode}};

use super::super::*;

#[test]
fn vimode_insert_cmds() {
	let raw = "abcdefghijklmnopqrstuvwxyz1234567890-=[];'<>/\\x1b";
	let mut mode = ViInsert::new();
	let cmds = mode.cmds_from_raw(raw);
	insta::assert_debug_snapshot!(cmds)
}

#[test]
fn vimode_normal_cmds() {
	let raw = "d2wg?5b2P5x";
	let mut mode = ViNormal::new();
	let cmds = mode.cmds_from_raw(raw);
	insta::assert_debug_snapshot!(cmds)
}
