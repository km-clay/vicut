use crate::{call_main, linebuf::LineBuf, modes::{normal::ViNormal, ViMode}};
use log::debug;
use pretty_assertions::assert_eq;

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub const LOREM_IPSUM_MULTILINE: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub mod modes;
pub mod linebuf;
pub mod editor;
pub mod pattern_match;
pub mod wiki_examples;

fn vicut_integration(input: &str, args: &[&str], expected: &str) {
	let output = call_main(args, input).unwrap();
	let output = output.strip_suffix("\n").unwrap_or(&output);
	/*
	println!("got: {output:?}");
	println!("expected: {expected:?}");
	*/
	assert_eq!(output,expected)
}


fn normal_cmd(cmd: &str, buf: &str, cursor: usize) -> (String,usize) {
	let cmd = ViNormal::new()
		.cmds_from_raw(cmd)
		.pop()
		.unwrap();
	let mut buf = LineBuf::new().with_initial(buf.to_string(), cursor);
	buf.exec_cmd(cmd).unwrap();
	(buf.as_str().to_string(),buf.cursor.get()) 
}
