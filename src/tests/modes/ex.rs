use crate::tests::vicut_integration;


#[test]
fn ex_delete() {
	vicut_integration(
		"Foo\nBar\nBiz",
		&[
			"-m", ":d",
		],
		"Bar\nBiz"
	);
}

#[test]
fn ex_yank() {
	vicut_integration(
		"\tFoo\nBar\nBiz",
		&[
			"-m", ":y<CR>jp",
		],
		"\tFoo\nBar\n\tFoo\nBiz"
	);
}

#[test]
fn ex_put() {
	vicut_integration(
		"Foo\nBar\nBiz",
		&[
			"-m", ":1y<CR>:2p",
		],
		"Foo\nBar\nFoo\nBiz"
	);
	vicut_integration(
		"Foo\nBar\nBiz",
		&[
			"-m", ":d<CR>:1,2p<CR>",
		],
		"Bar\nFoo\nBiz\nFoo"
	);
}

#[test]
fn ex_substitution() {
	vicut_integration(
		"Foo\nBar\nBiz\nFoo\nBuzz\nFoo\nBaz",
		&[
			"-m", ":%s/Foo/Replaced/g",
		],
		"Replaced\nBar\nBiz\nReplaced\nBuzz\nReplaced\nBaz",
	);
}

#[test]
fn ex_normal() {
	vicut_integration(
		"Foo\nBar\nBiz\nFoo\nBuzz\nFoo\nBaz",
		&[
			"-m", ":/Biz/normal! iNew Text",
		],
		"Foo\nBar\nNew TextBiz\nFoo\nBuzz\nFoo\nBaz",
	);
}

#[test]
fn ex_global_delete() {
	vicut_integration(
		"Foo\nBar\nBiz\nFoo\nBuzz\nFoo\nBaz",
		&[
			"-m", ":g/Foo/d",
		],
		"Bar\nBiz\nBuzz\nBaz",
	);
}

#[test]
fn ex_global_normal() {
	vicut_integration(
		"Foo\nBar\nBiz\nFoo\nBuzz\nFoo\nBaz",
		&[
			"-m", ":g/Foo/normal! iNew Text",
		],
		"New TextFoo\nBar\nBiz\nNew TextFoo\nBuzz\nNew TextFoo\nBaz",
	);
}

#[test]
fn ex_global_normal_nested() {
	vicut_integration(
		"Foo\nBar\nBiz\nFoo\nBuzz\nFoo\nBaz",
		&[
			"-m", ":g/Baz/normal! :g/Bar/normal! :g/Biz/normal! :g/Buzz/normal! :g/Foo/normal! cwWow!",
		],
		"Wow!\nBar\nBiz\nWow!\nBuzz\nWow!\nBaz",
	);
}
