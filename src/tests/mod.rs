use crate::{call_main, Cmd};
use pretty_assertions::assert_eq;

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub const LOREM_IPSUM_MULTILINE: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub mod modes;
pub mod linebuf;
pub mod editor;

#[test]
fn pattern_matching() {
	let input = "User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]";

	// Searching for api path
	let args = [
		"-c", r"/\/api\/v1\/\w+<CR>",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /");
	
	// Searching for status code
	let args = [
		"-c", r"/\b\d{3}\b<CR>4n",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 2");

	// Searching for session id
	let args = [
		"-c", r"/\(\w+-\w+\)<CR>"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (");

	// Searching for the word "logged"
	let args = [
		"-c", r"/logged<CR>"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"User_453 l");

	// Searching for the word "logged" then searching backward for the word "User"
	let args = [
		"-c", r"/logged<CR>?User<CR>"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"U");

	// Searching for the flag list
	let args = [
		"-c", r"/\[\w+(,\w+)*\]<CR>"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [");
}
