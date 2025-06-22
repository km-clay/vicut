use crate::tests::vicut_integration;

#[test]
fn select_in_parens() {
	vicut_integration(
		"This text is (selected) and this is not.",
		&[
		  "-c", "vi)",
		],
		"selected",
	);
}

#[test]
fn select_in_brackets() {
	vicut_integration(
		"This text is [selected] and this is not.",
		&[
		  "-c", "vi]",
		],
		"selected",
	);
}

#[test]
fn select_in_braces() {
	vicut_integration(
		"This text is {selected} and this is not.",
		&[
		  "-c", "vi{",
		],
		"selected",
	);
}

#[test]
fn select_in_quotes() {
	vicut_integration(
		"This text is \"selected\" and this is not.",
		&[
		  "-c", "vi\"",
		],
		"selected",
	);
}

#[test]
fn select_in_single_quotes() {
	vicut_integration(
		"This text is 'selected' and this is not.",
		&[
		  "-c", "vi'",
		],
		"selected",
	);
}

#[test]
fn select_around_parens() {
	vicut_integration(
		"This text is (selected) and this is not.",
		&[
		  "-c", "va)",
		],
		"(selected) ",
	);
}

#[test]
fn select_around_brackets() {
	vicut_integration(
		"This text is [selected] and this is not.",
		&[
		  "-c", "va]",
		],
		"[selected] ",
	);
}

#[test]
fn select_around_braces() {
	vicut_integration(
		"This text is {selected} and this is not.",
		&[
		  "-c", "va{",
		],
		"{selected} ",
	);
}

#[test]
fn select_around_quotes() {
	vicut_integration(
		"This text is \"selected\" and this is not.",
		&[
		  "-c", "va\"",
		],
		"\"selected\" ",
	);
}

#[test]
fn select_around_single_quotes() {
	vicut_integration(
		"This text is 'selected' and this is not.",
		&[
		  "-c", "va'",
		],
		"'selected' ",
	);
}

#[test]
fn select_lines() {
	vicut_integration(
		"Line 1\nLine 2\nLine 3",
		&[
		  "-m", "Vjd",
		],
		"Line 3",
	);
}

#[test]
fn select_lines_with_count() {
	vicut_integration(
		"Line 1\nLine 2\nLine 3",
		&[
		  "-c", "2Vj",
		],
		"Line 1\nLine 2",
	);
}

#[test]
fn del_inner_line() {
	vicut_integration(
		"Line 1\nLine 2\nLine 3",
		&[
		  "-m", "$v0d",
		],
		"\nLine 2\nLine 3",
	);
}

#[test]
fn select_block() {
	vicut_integration(
		"Line 1\nLine 2\nLine 3",
		&[
		  "-c", "<c-v>jl",
		],
		"Li\nLi",
	);
}

#[test]
fn delete_block() {
	vicut_integration(
		"Line 1\nLine 2\nLine 3",
		&[
		  "-m", "<c-v>jld",
		],
		"ne 1\nne 2\nLine 3",
	);
}

#[test]
fn change_block() {
	vicut_integration(
		"Line 1\nLine 2\nLine 3",
		&[
		  "-m", "<c-v>jlocNew Text",
		],
		"New Textne 1\nNew Textne 2\nLine 3",
	);
}

#[test]
fn change_block_weird() {
	vicut_integration(
		"abcdefg\nabcd\nabcdegh\nabcde\nabcdefg",
		&[
			"-m", "G0$h<c-v>hhhhkkkkocfoo<esc>",
		],
		"afoog\nafoo\nafooh\nafoo\nafoog"
	);
}

#[test]
fn delete_put_block() {
	vicut_integration(
		"abcdefgh\nabcd\nabcdefghi\nabcde\nabcdefg",
		&[
			"-m", "l<c-v>4jldp",
		],
		"adbcefgh\nadbc\nadbcefghi\nadbce\nadbcefg",
	);
}

#[test]
fn put_block_pad_short_lines() {
	vicut_integration(
		"abcdefgh\nabcd\nabcdefghi\nabcde\nabcdefg",
		&[
			"-m", "G0l<c-v>l4kd4lp",
		],
		"adefghbc\nad    bc\nadefghbci\nade   bc\nadefg bc",
	);
}

#[test]
fn put_block_preserve_formatting() {
	vicut_integration(
		"abcdefgh\nabcd\nabcdefghi\nabcde\nabcdefg",
		&[
			"-m", "l<c-v>$Gdp",
		],
		"abcdefgh\nabcd\nabcdefghi\nabcde\nabcdefg",
	);
}
