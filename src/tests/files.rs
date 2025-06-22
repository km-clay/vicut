use super::vicut_integration;

const VICUT_MAIN: &str = include_str!("golden_files/vicut_main.rs");
/// `vicut -m ":%s/Opts/Arguments/g"`
const MAIN_ARGV_REPLACED: &str = include_str!("golden_files/vicut_main_argv_replaced.rs");
/// `vicut -m "/impl Opts" -c "V$%"`
const MAIN_EXTRACTED_IMPL: &str = include_str!("golden_files/vicut_main_extracted_impl.rs");
/// `vicut -g "//\s[^/].*" -m "f/" -c "$" -n vicut_main.rs`
const MAIN_ALL_COMMENTS: &str = include_str!("golden_files/vicut_main_all_comments.rs");

#[test]
#[ignore]
fn file_replace_argv() {
	vicut_integration(
		VICUT_MAIN,
		&["-m", ":%s/Opts/Arguments/g"],
		MAIN_ARGV_REPLACED.trim_end()
	);
}

#[test]
#[ignore]
fn file_extract_impl() {
	vicut_integration(
		VICUT_MAIN,
		&["-m", "/impl Opts", "-c", "V$%"],
		MAIN_EXTRACTED_IMPL.trim_end(),
	);
}

#[test]
#[ignore]
fn file_all_comments() {
	vicut_integration(
		VICUT_MAIN,
		&["-g", "//\\s[^/].*",
				"-m", "f/",
				"-c", "$",
				"-n",
			"--end",
		],
		MAIN_ALL_COMMENTS.trim_end(),
	);
}
