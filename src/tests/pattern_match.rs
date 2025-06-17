use crate::tests::vicut_integration;


#[test]
fn pattern_matching_api_path_regex() {
	vicut_integration(
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]",
		&[ "-c", r"/\/api\/v1\/\w+<CR>" ],
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /"
	);
}

#[test]
fn pattern_matching_status_code() {
	vicut_integration(
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]",
		&[ "-c", r"/\b\d{3}\b<CR>4n", ],
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 2"
	);
}

#[test]
fn pattern_matching_session_id() {
	vicut_integration(
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]",
		&[ "-c", r"/\(\w+-\w+\)<CR>" ],
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID ("
	);
}

#[test]
fn pattern_matching_literal() {
	vicut_integration(
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]",
		&[ "-c", r"/logged<CR>" ],
		"User_453 l"
	);
}

#[test]
fn pattern_matching_forward_and_backward() {
	vicut_integration(
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]",
		&[ "-c", r"/logged<CR>?User<CR>" ],
		"U"
	);
}

#[test]
fn pattern_matching_flag_list() {
	vicut_integration(
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: [authenticated,admin,cachehit]",
		&[ "-c", r"/\[\w+(,\w+)*\]<CR>" ],
		"User_453 logged in from IP 192.168.0.42 at [2025-06-14 04:15:32] with session ID (abc123-XYZ), request path: /api/v1/users?limit=100&offset=200. Status: 200 OK. Response time: 123.45ms. Flags: ["
	);
}

#[test]
fn pattern_matching_literal2() {
	vicut_integration(
		 "The quick brown fox jumps over the lazy dog",
		&[ "-c", r"/fox<CR>", ],
		"The quick brown f"
	);
}

#[test]
fn pattern_matching_regex_test() {
	vicut_integration(
		 "The quick brown fox jumps over the lazy dog",
		&[ "-c", r"/\b.o.<CR>", ],
		"The quick brown f"
	);
}

#[test]
fn pattern_matching_mix_search_and_command() {
	vicut_integration(
		 "The quick brown fox jumps over the lazy dog",
		&[ "-c", r"/\b.o.\b<CR>n", ],
		"The quick brown fox jumps over the lazy d"
	);
}
