use crate::tests::vicut_integration;

#[test]
fn wiki_example_simple() {
	vicut_integration(
		"foo bar biz",
		&[ "-c", "e" ],
		"foo"
	);
}

#[test]
fn wiki_example_simple2() {
	vicut_integration(
		"foo bar",
		&[ "-m", "w", "-c", "e" ],
		"bar"
	);
}

#[test]
fn wiki_example_template_string() {
	vicut_integration(
		"foo bar biz\nbiz foo bar\nbar biz foo",
		&[ 
			"--linewise", "--template", "< {{1}} > ( {{2}} ) { {{3}} }",
			"-c", "e",
			"-m", "w",
			"-r", "2", "2",
			"-m", "0j",
			"-r", "4", "2", 
		],
		"< foo > ( bar ) { biz }\n< biz > ( foo ) { bar }\n< bar > ( biz ) { foo }"
	);
}

#[test]
fn wiki_example_json_output() {
	vicut_integration(
		"foo bar",
		&[ 
			"--json",
			"-c", "e",
			"-m", "w",
			"-c", "e"
		],
		"[\n  {\n    \"1\": \"foo\",\n    \"2\": \"bar\"\n  }\n]"
	);
}

#[test]
fn wiki_example_text_objects() {
	vicut_integration(
		"enp8s0           ethernet  connected               Wired connection 1\nlo               loopback  connected (externally)  lo                 \nwlp15s0          wifi      disconnected            --                 \np2p-dev-wlp15s0  wifi-p2p  disconnected            --                 ",
		&[ 
			"--linewise", "--delimiter", " --- ",
			"-c", "E", "-m", "w",
			"-r", "2", "1",
			"-c", "ef)", "-m", "w",
			"-c", "$"
		],
		"enp8s0 --- ethernet --- connected --- Wired connection 1\nlo --- loopback --- connected (externally) --- lo                 \nwlp15s0 --- wifi --- disconnected --- --                 \np2p-dev-wlp15s0 --- wifi-p2p --- disconnected --- --                 "
	);
}

#[test]
fn wiki_example_repeat_command() {
	vicut_integration(
		 "a b c d e f",
		&[ 
			"--delimiter", " | ",
			"-c", "wge",
			"-m", "w",
			"-r", "2", "2",
		],
		"a | b | c"
	);
}

#[test]
fn wiki_example_nested_repeat() {
	vicut_integration(
		"foo bar biz\nbiz foo bar\nbar biz foo",
		&[ 
			"--json",
			"-c", "e",
			"-m", "jb",
			"-r", "2", "2",
			"-m", "w2k",
			"-n",        
			"-r", "5", "2",
		],
		"[
  {
    \"1\": \"foo\",
    \"2\": \"biz\",
    \"3\": \"bar\"
  },
  {
    \"1\": \"bar\",
    \"2\": \"foo\",
    \"3\": \"biz\"
  },
  {
    \"1\": \"biz\",
    \"2\": \"bar\",
    \"3\": \"foo\"
  }
]"
	);
}

#[test]
fn wiki_example_name_fields_json() {
	vicut_integration(
		"31200) FiberFast Networks (Portland, OR, United States) [321.23 km]",
		&[ 
			"--json",
			"-c", "name=id", "e",
			"-m", "W",
			"-c", "name=provider", "t(h",
			"-c", "name=location", "vi)",
			"-c", "name=distance", "vi]",
		],
		"[
  {
    \"distance\": \"321.23 km\",
    \"id\": \"31200\",
    \"location\": \"Portland, OR, United States\",
    \"provider\": \"FiberFast Networks\"
  }
]"
	);
}

#[test]
fn wiki_example_name_fields_template() {
	vicut_integration(
		"31200) FiberFast Networks (Portland, OR, United States) [321.23 km]",
		&[ 
			"--template", "{{id}} - {{provider}} @ {{location}} ({{distance}})",
			"-c", "name=id", "e",
			"-m", "W",
			"-c", "name=provider", "t(h",
			"-c", "name=location", "vi)",
			"-c", "name=distance", "vi]",
		],
		"31200 - FiberFast Networks @ Portland, OR, United States (321.23 km)"
	);
}

#[test]
fn wiki_example_edit_buffer() {
	vicut_integration(
		"useful_data1 some_garbage useful_data2",
		&[ 
			"--json",
			"-c", "wdwe"
		],
		"[
  {
    \"1\": \"useful_data1 useful_data2\"
  }
]"
	);
}

#[test]
fn wiki_example_insert_mode() {
	vicut_integration(
		"some_stuff some_stuff some_stuff",
		&[ 
			"--delimiter", " --- ",
			"-c", "iField 1: <esc>we",
			"-m", "w",
			"-c", "iField 2: <esc>we",
			"-m", "w",
			"-c", "iField 3: <esc>we",
		],
		"Field 1: some_stuff --- Field 2: some_stuff --- Field 3: some_stuff"
	);
}

#[test]
fn wiki_example_visual_mode() {
	vicut_integration(
		"This text has (some stuff) inside of parenthesis, and [some other stuff] inside of brackets",
		&[ 
			"--delimiter", " -- ",
			"-c", "vi)",
			"-c", "vi]"
		],
		"some stuff -- some other stuff"
	);
}

#[test]
fn wiki_example_visual_mode2() {
	vicut_integration(
		"31200) FiberFast Networks (Portland, OR, United States) [321.23 km]\n18220) MetroLink Broadband (Austin, TX, United States) [121.47 km]\n29834) Skyline Internet (Denver, CO, United States) [295.88 km]",
		&[ 
			"--linewise", "--delimiter", " --- ",
			"-c", "e",
			"-m", "2w", 
			"-c", "t(h",
			"-c", "vi)",
			"-c", "vi]",
		],
		"31200 --- FiberFast Networks --- Portland, OR, United States --- 321.23 km\n18220 --- MetroLink Broadband --- Austin, TX, United States --- 121.47 km\n29834 --- Skyline Internet --- Denver, CO, United States --- 295.88 km"
	);
}

#[test]
fn wiki_example_substitution() {
	vicut_integration(
	 "foo bar foo\nbar foo bar\nfoo bar foo",
		&[ 
			"-m", ":%s/foo/###/g<CR>",
			"-m", ":%s/bar/%%%/g<CR>",
			"-m", ":%s/%%%/foo/g<CR>",
			"-m", ":%s/###/bar/g<CR>"
		],
		"bar foo bar\nfoo bar foo\nbar foo bar"
	);
}
