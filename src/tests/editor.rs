use crate::{linebuf::LineBuf, modes::{normal::ViNormal, ViMode}, tests::{LOREM_IPSUM, LOREM_IPSUM_MULTILINE}};
use pretty_assertions::assert_eq;


fn normal_cmd(cmd: &str, buf: &str, cursor: usize) -> (String,usize) {
	let cmd = ViNormal::new()
		.cmds_from_raw(cmd)
		.pop()
		.unwrap();
	let mut buf = LineBuf::new().with_initial(buf, cursor);
	buf.exec_cmd(cmd).unwrap();
	(buf.as_str().to_string(),buf.cursor.get()) 
}

#[test]
fn editor_delete_word() {
	assert_eq!(normal_cmd(
		"dw",
		"The quick brown fox jumps over the lazy dog",
		16),
		("The quick brown jumps over the lazy dog".into(), 16)
	);
}

#[test]
fn editor_delete_backwards() {
	assert_eq!(normal_cmd(
		"2db",
		"The quick brown fox jumps over the lazy dog",
		16),
		("The fox jumps over the lazy dog".into(), 4)
	);
}

#[test]
fn editor_rot13_five_words_backwards() {
	assert_eq!(normal_cmd(
		"g?5b",
		"The quick brown fox jumps over the lazy dog",
		31),
		("The dhvpx oebja sbk whzcf bire the lazy dog".into(), 4)
	);
}

#[test]
fn editor_delete_word_on_whitespace() {
	assert_eq!(normal_cmd(
		"dw",
		"The quick  brown fox",
		10), //on the whitespace between "quick" and "brown"
		("The quick brown fox".into(), 10)
	);
}

#[test]
fn editor_delete_5_words() {
	assert_eq!(normal_cmd(
		"5dw",
		"The quick brown fox jumps over the lazy dog",
		16,),
		("The quick brown dog".into(), 16)
	);
}

#[test]
fn editor_delete_end_includes_last() {
	assert_eq!(normal_cmd(
		"de",
		"The quick brown fox::::jumps over the lazy dog",
		16),
		("The quick brown ::::jumps over the lazy dog".into(), 16)
	);
}

#[test]
fn editor_delete_end_unicode_word() {
	assert_eq!(normal_cmd(
		"de",
		"naïve café world",
		0),
		(" café world".into(), 0)
	);
}

#[test]
fn editor_inplace_edit_cursor_position() {
	assert_eq!(normal_cmd(
		"5~",
		"foobar",
		0),
		("FOOBAr".into(), 4)
	);
	assert_eq!(normal_cmd(
		"5rg",
		"foobar",
		0),
		("gggggr".into(), 4)
	);
}

#[test]
fn editor_insert_mode_not_clamped() {
	assert_eq!(normal_cmd(
		"a",
		"foobar",
		5),
		("foobar".into(), 6)
	)
}

#[test]
fn editor_overshooting_motions() {
	assert_eq!(normal_cmd(
		"5dw",
		"foo bar",
		0),
		("".into(), 0)
	);
	assert_eq!(normal_cmd(
		"3db",
		"foo bar",
		0),
		("foo bar".into(), 0)
	);
	assert_eq!(normal_cmd(
		"3dj",
		"foo bar",
		0),
		("foo bar".into(), 0)
	);
	assert_eq!(normal_cmd(
		"3dk",
		"foo bar",
		0),
		("foo bar".into(), 0)
	);
}

#[test]
fn editor_textobj_quoted() {
	assert_eq!(normal_cmd(
		"di\"",
		"this buffer has \"some \\\"quoted\" text",
		0),
		("this buffer has \"\" text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da\"",
		"this buffer has \"some \\\"quoted\" text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di'",
		"this buffer has 'some \\'quoted' text",
		0),
		("this buffer has '' text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da'",
		"this buffer has 'some \\'quoted' text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di`",
		"this buffer has `some \\`quoted` text",
		0),
		("this buffer has `` text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da`",
		"this buffer has `some \\`quoted` text",
		0),
		("this buffer has text".into(), 16)
	);
}

#[test]
fn editor_textobj_delimited() {
	assert_eq!(normal_cmd(
		"di)",
		"this buffer has (some \\(\\)(inner) \\(\\)delimited) text",
		0),
		("this buffer has () text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da)",
		"this buffer has (some \\(\\)(inner) \\(\\)delimited) text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di]",
		"this buffer has [some \\[\\][inner] \\[\\]delimited] text",
		0),
		("this buffer has [] text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da]",
		"this buffer has [some \\[\\][inner] \\[\\]delimited] text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di}",
		"this buffer has {some \\{\\}{inner} \\{\\}delimited} text",
		0),
		("this buffer has {} text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da}",
		"this buffer has {some \\{\\}{inner} \\{\\}delimited} text",
		0),
		("this buffer has text".into(), 16)
	);
	assert_eq!(normal_cmd(
		"di>",
		"this buffer has <some \\<\\><inner> \\<\\>delimited> text",
		0),
		("this buffer has <> text".into(), 17)
	);
	assert_eq!(normal_cmd(
		"da>",
		"this buffer has <some \\<\\><inner> \\<\\>delimited> text",
		0),
		("this buffer has text".into(), 16)
	);
}

#[test]
fn editor_delete_line_up() {
	assert_eq!(normal_cmd(
		"dk",
		"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.",
		239),
		("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 242,)
	)
} 

#[test]
fn editor_sentence_operations() {
	assert_eq!(normal_cmd(
		"d)",
		LOREM_IPSUM,
		0),
		("Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 0)
	);
	assert_eq!(normal_cmd(
		"5d)",
		LOREM_IPSUM,
		0),
		("Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 0)
	);
	assert_eq!(normal_cmd(
		"d5)",
		LOREM_IPSUM,
		0),
		("Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 0)
	);
	assert_eq!(normal_cmd(
		"d)",
		"This sentence has some closing delimiters after it.)]'\" And this is another sentence.",
		0),
		("And this is another sentence.".into(), 0)
	);
	assert_eq!(normal_cmd(
		"d3)",
		LOREM_IPSUM,
		232),
		("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 232)
	);
	assert_eq!(normal_cmd(
		"d2(",
		LOREM_IPSUM,
		335),
		("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 124)
	);
	assert_eq!(normal_cmd(
		"dis",
		LOREM_IPSUM,
		257),
		("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.  Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 232)
	);
	assert_eq!(normal_cmd(
		"das",
		LOREM_IPSUM,
		257),
		("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.".into(), 232)
	);
}

//"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra."
