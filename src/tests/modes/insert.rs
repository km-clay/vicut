use crate::tests::{normal_cmd, vicut_integration};

use super::*;

#[test]
fn vimode_insert_structures() {
	let raw = "abcdefghijklmnopqrstuvwxyz1234567890-=[];'<>/\\x1b";
	let mut mode = ViInsert::new();
	let cmds = mode.cmds_from_raw(raw);
	insta::assert_debug_snapshot!(cmds)
}

#[test]
fn two_inserts() {
	vicut_integration(
		"foo bar biz",
		&[ "-m", "iInserting some text<esc>2wiAnd some more here too<esc>" ],

		"Inserting some textfoo bar And some more here toobiz",
	);
}

#[test]
fn ctrl_w() {
	vicut_integration(
		"foo bar biz",
		&[ "-m", "eiInserting_some_text<c-w>" ],

		"foo bar biz",
	);
}

#[test]
fn linebreaks() {
	// Also tests 'a' at the end of the buffer
	vicut_integration(
		"foo bar biz",
		&[ "-m", "$a<enter>bar foo biz" ],
		"foo bar biz\nbar foo biz",
	);
	vicut_integration(
		"foo bar biz",
		&[ "-m", "$a<CR>bar foo biz" ],
		"foo bar biz\nbar foo biz",
	)
}

#[test]
fn navigation() {
	vicut_integration(
		"foo bar biz\nbar foo biz",
		&[
			"-m", "j",
			"-m", "<right><right><right><right><up>",
			"-c", "e",
		],
		"bar",
	)
}

#[test]
fn backspace_and_delete() {
	vicut_integration(
		"foo bar biz\nbar foo biz",
		&[
			"-m", "jw",
"-m", "i<BS><BS><BS><BS><del><del><del><del>",
			"-c", "e"
		],
		"biz",
	)
}

#[test]
fn end_of_line_motion_boundary() {
	vicut_integration(
		"foo bar", 
		&[
			"-m", "$",
			"-m", "i<right>",
			"-c", "b"
		], 
		"bar"
	);
}

#[test]
fn prefix_insert() {
	vicut_integration(
		"    foo bar", 
		&[
			"-m", "$",
			"-m", "Iinserting some text at the start"
		], 
		"    inserting some text at the startfoo bar"
	);
}

#[test]
fn insert_unicode() {
	vicut_integration(
		"foo", 
		&[
			"-m", "ea→bar",
		], 
		"foo→bar"
	);
}

#[test]
fn insert_in_empty_line() {
	vicut_integration(
		"foo\n\nbiz", 
		&[
			"-m", "jibar",
		], 
		"foo\nbar\nbiz"
	);
}

#[test]
fn insert_from_visual_mode() {
	vicut_integration(
		"foo biz bar", 
		&[
			"-m", "wveIinserting some text",
		], 
		"inserting some textfoo biz bar"
	);
	vicut_integration(
		"foo biz bar", 
		&[
			"-m", "wveAinserting some text",
		], 
		"foo bizinserting some text bar"
	);
}

#[test]
fn insert_empty_buffer() {
	vicut_integration(
		"",
		&[
			"-m", "ihello world"
		],
		"hello world"
	);
}
