use crate::call_main;
use pretty_assertions::assert_eq;

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub const LOREM_IPSUM_MULTILINE: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub mod modes;
pub mod linebuf;
pub mod editor;

// Integration tests:

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

	let input = "The quick brown fox jumps over the lazy dog";
	let args = [
		"-c", r"/fox<CR>",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"The quick brown f");

	let args = [
		"-c", r"/\b.o.<CR>",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"The quick brown f");

	let args = [
		"-c", r"/\b.o.\b<CR>n",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"The quick brown fox jumps over the lazy d");
}

#[test]
fn stuff_from_wiki() {
	// Make sure this test always passes
	let input = "foo bar biz";
	let args = [
		"-c", "e"
	];

	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"foo");

	let input = "foo bar";
	let args = [
		"-m", "w",
		"-c", "e"
	];

	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"bar");

	let input = "foo bar biz\nbiz foo bar\nbar biz foo";
	let args = [
		"--linewise", "--template", "< {{1}} > ( {{2}} ) { {{3}} }",
		"-c", "e",
		"-m", "w",
		"-r", "2", "2",
		"-m", "0j", 
		"-r", "4", "2",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"< foo > ( bar ) { biz }\n< biz > ( foo ) { bar }\n< bar > ( biz ) { foo }");

	let input = "foo bar";
	let args = [
		"--json",
		"-c", "e",
		"-m", "w",
		"-c", "e"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"[\n  {\n    \"1\": \"foo\",\n    \"2\": \"bar\"\n  }\n]");

	let input = "enp8s0           ethernet  connected               Wired connection 1\nlo               loopback  connected (externally)  lo                 \nwlp15s0          wifi      disconnected            --                 \np2p-dev-wlp15s0  wifi-p2p  disconnected            --                 
";
	let args = [
		"--linewise", "--delimiter", " --- ",
		"-c", "E", "-m", "w",
		"-r", "2", "1",
		"-c", "ef)", "-m", "w",
		"-c", "$"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"enp8s0 --- ethernet --- connected --- Wired connection 1\nlo --- loopback --- connected (externally) --- lo                 \nwlp15s0 --- wifi --- disconnected --- --                 \np2p-dev-wlp15s0 --- wifi-p2p --- disconnected --- --");

	let input = "a b c d e f";
	let args = [
		"--delimiter", " | ",
		"-c", "wge",
		"-m", "w",
		"-r", "2", "2",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"a | b | c");

	let input = "foo bar biz\nbiz bar foo\nbar biz foo";
	let args = [
		"--json",
    "-c", "e",
    "-m", "jb",
    "-r", "2", "2",
    "-m", "w2k",
    "-n",        
    "-r", "5", "2",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"[
  {
    \"1\": \"foo\",
    \"2\": \"biz\",
    \"3\": \"bar\"
  },
  {
    \"1\": \"bar\",
    \"2\": \"bar\",
    \"3\": \"biz\"
  },
  {
    \"1\": \"biz\",
    \"2\": \"foo\",
    \"3\": \"foo\"
  }
]");
	let input = "31200) FiberFast Networks (Portland, OR, United States) [321.23 km]";
	let args = [
		"--json",
    "-c", "name=id", "e",
    "-m", "W",
    "-c", "name=provider", "t(h",
    "-c", "name=location", "vi)",
    "-c", "name=distance", "vi]",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"[
  {
    \"distance\": \"321.23 km\",
    \"id\": \"31200\",
    \"location\": \"Portland, OR, United States\",
    \"provider\": \"FiberFast Networks\"
  }
]");
	let args = [
		"--template", "{{id}} - {{provider}} @ {{location}} ({{distance}})",
    "-c", "name=id", "e",
    "-m", "W",
    "-c", "name=provider", "t(h",
    "-c", "name=location", "vi)",
    "-c", "name=distance", "vi]",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"31200 - FiberFast Networks @ Portland, OR, United States (321.23 km)");

	let input = "useful_data1 some_garbage useful_data2";
	let args = [
		"--json",
		"-c", "wdwe"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"[
  {
    \"1\": \"useful_data1 useful_data2\"
  }
]");

	let input = "some_stuff some_stuff some_stuff";
	let args = [
		"--delimiter", " --- ",
		"-c", "iField 1: <esc>we",
		"-m", "w",
		"-c", "iField 2: <esc>we",
		"-m", "w",
		"-c", "iField 3: <esc>we",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"Field 1: some_stuff --- Field 2: some_stuff --- Field 3: some_stuff");

	let input = "This text has (some stuff) inside of parenthesis, and [some other stuff] inside of brackets";
	let args = [
		"--delimiter", " -- ",
		"-c", "vi)",
		"-c", "vi]"
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"some stuff -- some other stuff");

	let input = "31200) FiberFast Networks (Portland, OR, United States) [321.23 km]\n18220) MetroLink Broadband (Austin, TX, United States) [121.47 km]\n29834) Skyline Internet (Denver, CO, United States) [295.88 km]";
	let args = [
		"--linewise", "--delimiter", " --- ",
		"-c", "e",
		"-m", "2w", 
		"-c", "t(h",
		"-c", "vi)",
		"-c", "vi]",
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"31200 --- FiberFast Networks --- Portland, OR, United States --- 321.23 km\n18220 --- MetroLink Broadband --- Austin, TX, United States --- 121.47 km\n29834 --- Skyline Internet --- Denver, CO, United States --- 295.88 km");
/*
	let input = "";
	let args = [
	];
	let output = call_main(&args, input).unwrap();
	assert_eq!(output.trim(),"");
*/
}
