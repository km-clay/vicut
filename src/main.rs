#![allow(clippy::unnecessary_to_owned,clippy::while_let_on_iterator)]
//! `vicut` is a command-line tool that brings Vim-style motions and commands
//! to non-interactive text processing.
//!
//! It allows Vim users to apply familiar editing operations to standard input, files,
//! or streams, enabling powerful scripted transformations outside the interactive editor.
//!
//! ### High-level structure:
//! 1. Arguments are parsed into a sequence of commands
//! 2. A `ViCut` instance is created to manage editor state and buffer contents
//! 3. The commands are applied to the input in sequence, modifying and/or extracting text
use std::{collections::BTreeMap, fmt::{Display, Write}, fs, io::{self, BufRead, Write as IoWrite}, path::{Path, PathBuf}};

extern crate tikv_jemallocator;

#[cfg(target_os = "linux")]
#[global_allocator]
/// For linux we use Jemalloc. It is ***significantly*** faster than the default allocator in this case, for some reason.
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use vic::parse::Val;
use exec::ViCut;
use serde_json::{Map, Value};
use rayon::prelude::*;

use crate::{linebuf::LineBuf, vic::error::VicErr};

pub mod vicmd;
pub mod modes;
pub mod exec;
pub mod linebuf;
pub mod keys;
pub mod register;
pub mod reader;
pub mod vic;
#[cfg(test)]
pub mod tests;

/// The field name used in `Cmd::NamedField`
pub type Name = String;

/// Print the given error message and exit the program.
/// Since we're a command-line tool, exiting on errors is the expected behavior, which makes things easy.
///
/// Despite the header, this function does not return anything. It always calls `std::process::exit(1)`.
/// This is done so that the function can be easily used as an argument to methods such as `unwrap_or_else`.
///
/// The error message will be prefixed with `vicut:` if it is not already.
pub fn complain_and_exit<T>(err: impl Display) -> T {
	let err = err.to_string();
	eprintln!("{err}");
	std::process::exit(1)
}

#[derive(Clone, Debug, Default)]
pub struct ExecCtx {
	field_num: usize,
	fields: Vec<(String,String)>, // (name, value)
	fmt_lines: Vec<Vec<(String,String)>>, // Lines to format output from
}

impl ExecCtx {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn push_fields(&mut self) {
		let fields = std::mem::take(&mut self.fields);
		self.fmt_lines.push(fields);
		self.field_num = 0;
	}
	/// Trim the fields 🧑‍🌾
	fn trim_fields(&mut self) {
		let lines = &mut self.fmt_lines;
		for line in lines {
			for (_, field) in line {
				*field = field.trim().to_string()
			}
		}
	}
}

#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub delimiter: Option<String>,
    pub template: Option<String>,
    pub max_jobs: Option<u32>,
    pub backup_extension: Option<String>,
    pub edit_inplace: Option<bool>,
    pub json: Option<bool>,
    pub trace: Option<bool>,
    pub linewise: Option<bool>,
    pub trim_fields: Option<bool>,
    pub keep_mode: Option<bool>,
    pub backup_files: Option<bool>,
    pub single_thread: Option<bool>,
    pub global_uses_line_numbers: Option<bool>,
    pub no_input: Option<bool>,
    pub silent: Option<bool>,
    pub pipe_in: Option<String>,
    pub pipe_out: Option<String>,
    pub out_file: Option<PathBuf>,
    pub vic_file: Option<PathBuf>,
		pub vic_raw: Option<String>,
    pub files: Option<Vec<PathBuf>>,
}

impl Opts {
	pub fn from_cmd_line_args() -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = std::env::args().skip(1).peekable();
		while let Some(arg) = args.next() {
			match arg.as_str() {
				"--" => break,
				"--json" | "-j" => {
					new.json = Some(true);
				}
				"--trace" => {
					new.trace = Some(true);
				}
				"--linewise" => {
					new.linewise = Some(true);
				}
				"--serial" => {
					new.single_thread = Some(true);
				}
				"--trim-fields" => {
					new.trim_fields = Some(true);
				}
				"--keep-mode" => {
					new.keep_mode = Some(true);
				}
				"--backup" => {
					new.backup_files = Some(true);
				}
				"--global-uses-line-numbers" => {
					new.global_uses_line_numbers = Some(true);
				}
				"--silent" => {
					new.silent = Some(true);
				}
				"-i" => {
					new.edit_inplace = Some(true);
				}
				"--no-input" | "-n" => {
					new.no_input = Some(true);
				}
				"--template" | "-t" => {
					let Some(next_arg) = args.next() else {
						return Err(format!("Expected a format string after '{arg}'"))
					};
					if next_arg.starts_with('-') {
						return Err(format!("Expected a format string after '{arg}', found {next_arg}"))
					}
					new.template = Some(next_arg)
				}
				"--delimiter" | "-d" => {
					let Some(next_arg) = args.next() else { continue };
					if next_arg.starts_with('-') {
						return Err(format!("Expected a delimiter after '{arg}', found {next_arg}"))
					}
					new.delimiter = Some(next_arg)
				}
				_ => new.handle_string(arg)
			}
		}
		Ok(new)
	}
	fn validate_filename(filename: &str) -> Result<(),String> {
		let path = PathBuf::from(filename.trim().to_string());
		if !path.exists() {
			return Err(format!("vicut: file not found '{}'",path.display()));
		}
		if !path.is_file() {
			return Err(format!("vicut: '{}' is not a file",path.display()));
		}
		if fs::File::open(&path).is_err() {
			return Err(format!("vicut: failed to read file '{}'",path.display()));
		}
		Ok(())
	}
	/// Handle a filename passed as an argument.
	///
	/// Checks to make sure the following invariants are met:
	/// 1. The path given exists.
	/// 2. The path given refers to a file.
	/// 3. The path given refers to a file that we are allowed to read.
	///
	/// We check all three separately instead of just the last one, so that we can give better error messages
	fn handle_string(&mut self, arg: String) {
		if let Err(e) = Self::validate_filename(&arg) {
			if self.vic_raw.is_some() || self.vic_file.is_some() {
				eprintln!("{e}");
				std::process::exit(1);
			} else {
				self.vic_raw = Some(arg);
				return
			}
		}
		let path = PathBuf::from(arg.trim().to_string());
		if self.files.is_none() {
			if self.vic_file.is_none() {
				self.vic_file = Some(path);
				return
			} else {
				self.files = Some(vec![])
			}
		}
		if !self.files.as_ref().unwrap().contains(&path) {
			self.files.as_mut().unwrap().push(path)
		}
	}
}

/// "Get some help" - Michael Jordan
/// Prints out the help info for `vicut`
fn get_help() -> String {
	let mut help = String::new();
	writeln!(help).ok();
	writeln!(help, "\x1b[1mvicut\x1b[0m").ok();
	writeln!(help, "A text processor that uses Vim motions to slice and extract structured data from stdin.").ok();
	writeln!(help).ok();
	writeln!(help).ok();
	writeln!(help, "\x1b[1;4mUSAGE:\x1b[0m").ok();
	writeln!(help, "\tvicut [OPTIONS] [COMMANDS] [FILES]").ok();
	writeln!(help).ok();
	writeln!(help).ok();
	writeln!(help, "\x1b[1;4mOPTIONS:\x1b[0m").ok();
	writeln!(help, "\t-t, --template <STR>").ok();
	writeln!(help, "\t\tProvide a format template to use for custom output formats. Example:").ok();
	writeln!(help, "\t\t--template \"< {{{{1}}}} > ( {{{{2}}}} ) {{ {{{{3}}}} }}\"").ok();
	writeln!(help, "\t\tNames given to fields explicitly using '-c name=<name>' should be used instead of field numbers.").ok();
	writeln!(help).ok();
	writeln!(help, "\t-d, --delimiter <STR>").ok();
	writeln!(help, "\t\tProvide a delimiter to place between fields in the output. No effect when used with --json.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--keep-mode").ok();
	writeln!(help, "\t\tThe internal editor will not return to normal mode after each command.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--json").ok();
	writeln!(help, "\t\tOutput the result as structured JSON.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--linewise").ok();
	writeln!(help, "\t\tApply given commands to each line in the given input.").ok();
	writeln!(help, "\t\tEach line in the input is treated as it's own separate buffer.").ok();
	writeln!(help, "\t\tThis operation is multi-threaded.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--serial").ok();
	writeln!(help, "\t\tWhen used with --linewise, operates on each line sequentially instead of using multi-threading.").ok();
	writeln!(help, "\t\tNote that the order of lines is maintained regardless of whether or not multi-threading is used.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--jobs").ok();
	writeln!(help, "\t\tWhen used with --linewise, limits the number of threads that the program can use.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--trim-fields").ok();
	writeln!(help, "\t\tTrim leading and trailing whitespace from captured fields.").ok();
	writeln!(help).ok();
	writeln!(help, "\t-i").ok();
	writeln!(help, "\t\tEdit given files in-place.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--backup").ok();
	writeln!(help, "\t\tIf editing files in-place, create a backup first.").ok();
	writeln!(help).ok();
	writeln!(help, "\t--backup-extension").ok();
	writeln!(help, "\t\tIf --backup is set, use the given file extension. Default is '.bak'").ok();
	writeln!(help).ok();
	writeln!(help, "\t--trace").ok();
	writeln!(help, "\t\tPrint debug trace of command execution").ok();
	writeln!(help).ok();
	writeln!(help).ok();
	writeln!(help, "\x1b[1;4mCOMMANDS:\x1b[0m").ok();
	writeln!(help, "\t-c, --cut [name=<NAME>] <VIM_COMMAND>").ok();
	writeln!(help, "\t\tExecute a Vim command on the buffer, and capture the text between the cursor's start and end positions as a field.").ok();
	writeln!(help, "\t\tFields can be optionally given a name, which will be used as the key for that field in formatted JSON output.").ok();
	writeln!(help).ok();
	writeln!(help, "\t-g, --global").ok();
	writeln!(help, "\t-v, --not-global").ok();
	writeln!(help, "\t\tCreates a subscope of command flags that only execute on lines that match a pattern passed to the '-g' flag").ok();
	writeln!(help, "\t\t'-v' variants only execute on lines that don't match the given pattern").ok();
	writeln!(help, "\t\t'-g' <PATTERN> and any commands in it's scope count as a single command for the purpose of repeating with '-r'").ok();
	writeln!(help).ok();
	writeln!(help, "\t--end").ok();
	writeln!(help, "\t\tEnds a '-g'/'-v' subscope, allowing you to continue writing commands in the non-conditional outer scope").ok();
	writeln!(help).ok();
	writeln!(help, "\t-m, --move <VIM_COMMAND>").ok();
	writeln!(help, "\t\tLogically identical to -c/--cut, except it does not capture a field.").ok();
	writeln!(help).ok();
	writeln!(help, "\t-r, --repeat <N> <R>").ok();
	writeln!(help, "\t\tRepeat the last N commands R times. Repeats can be nested.").ok();
	writeln!(help).ok();
	writeln!(help, "\t-n, --next").ok();
	writeln!(help, "\t\tStart a new field group. Each field group becomes one output record.").ok();
	writeln!(help).ok();
	writeln!(help).ok();
	writeln!(help, "\x1b[1;4mNOTES:\x1b[0m").ok();
	writeln!(help, "\t* Commands are executed left to right.").ok();
	writeln!(help, "\t* Cursor state is maintained between commands, but the editor returns to normal mode between each command.").ok();
	writeln!(help, "\t* Commands are not limited to only motions. Commands which edit the buffer can be executed as well.").ok();
	writeln!(help).ok();
	writeln!(help).ok();
	writeln!(help, "\x1b[1;4mEXAMPLE:\x1b[0m").ok();
	writeln!(help, "\t$ echo 'foo bar (boo far) [bar foo]' | vicut --delimiter ' -- ' \\
\t-c 'e' -m 'w' -r 2 1 -c 'va)' -c 'va]'").ok();
	writeln!(help, "\toutputs:").ok();
	writeln!(help, "\tfoo -- bar -- (boo far) -- [bar foo]").ok();
	writeln!(help).ok();
	writeln!(help, "For more info, see: https://github.com/km-clay/vicut").ok();
	help
}

/// Initialize the logger
///
/// This interacts with the `--trace` flag that can be passed in the arguments.
/// If `trace` is true, then trace!() calls always activate, with our custom formatting.
fn init_logger(trace: bool) {
	let mut builder = env_logger::builder();
	if trace {
		builder.filter(None, log::LevelFilter::Trace);
	}

	builder.format(move |buf, record| {
		let color = match record.level() {
			log::Level::Error => "\x1b[1;31m",
			log::Level::Warn => "\x1b[33m",
			log::Level::Info => "\x1b[32m",
			log::Level::Debug => "\x1b[34m",
			log::Level::Trace => "\x1b[36m"
		};
		if trace {
			if record.level() == log::Level::Trace {
				writeln!(buf, "[{color}{}\x1b[0m] {}", record.level(), record.args())
			} else {
				Ok(())
			}
		} else {
			writeln!(buf, "[{color}{}\x1b[0m] {}", record.level(), record.args())
		}
	});

	builder.init();
}

/// Format the stuff we extracted according to user specification
///
/// `lines` is a two-dimensional vector of tuples, each representing a key/value pair for extract fields.
fn format_output(args: &Opts, lines: Vec<Vec<(String,String)>>) -> String {
	if args.json.unwrap_or(false) {
		Ok(format_output_json(lines))
	} else if let Some(template) = args.template.as_deref() {
		format_output_template(template, lines)
	} else {
		let delimiter = args.delimiter.as_deref().unwrap_or(" ");
		Ok(format_output_standard(delimiter, lines))
	}.unwrap_or_else(complain_and_exit)
}

/// Format the output as JSON
fn format_output_json(lines: Vec<Vec<(String,String)>>) -> String {
	if lines.is_empty() || lines.iter().all(|line| line.is_empty()) {
		return String::new();
	}
	let array: Vec<Value> = lines
		.into_iter()
		.map(|fields| {
			let mut obj = Map::new();
			for (name,field) in fields {
				obj.insert(name, Value::String(field));
			}
			Value::Object(obj)
		}).collect();

	let json = Value::Array(array);
	serde_json::to_string_pretty(&json).unwrap()
}

type Files = Vec<(PathBuf, Vec<Vec<(String,String)>>)>; // YEESH
fn format_output_json_files(files: Files) -> String {
	let mut array = vec![];
	for (path, content) in files {
		let mut obj = Map::new();
		let path = path.to_string_lossy().to_string();

		obj.insert("__filename__".into(), Value::String(path));
		let array_content: Vec<Value> = content
			.into_iter()
			.map(|fields| {
				let mut obj = Map::new();
				for (name,field) in fields {
					obj.insert(name, Value::String(field));
				}
				Value::Object(obj)
			}).collect();
		obj.insert("__content__".into(), Value::Array(array_content));
		array.push(Value::Object(obj));
	}
	let json = Value::Array(array);
	serde_json::to_string_pretty(&json).unwrap()
}

/// Check to see if we didn't explicitly extract any fields
///
/// Checks for the `"0"` field name, which is a sentinel value that says "We didn't get any `-c` commands"
/// This can be depended on, since `"0"` is a reserved field name that cannot be set by user input.
fn no_fields_extracted(lines: &[Vec<(String,String)>]) -> bool {
	lines.len() == 1 && lines.first().is_some_and(|record| record.len() == 1 && record.first().is_some_and(|field| field.0 == "0"))
}

/// Perform standard output formatting.
///
/// If we didn't extract any fields, we do our best to preserve the formatting of the original input
/// If we did extract some fields, we print each record one at a time, and each field will be separated by `delimiter`
fn format_output_standard(delimiter: &str, mut lines: Vec<Vec<(String,String)>>) -> String {
	// Let's check to see if we are outputting the whole buffer
	if no_fields_extracted(&lines)  {
		// We performed len checks in no_fields_extracted(), so unwrap is safe
		// So let's double pop the 2d vector and grab the value of our only field
		lines.pop()
			.unwrap()
			.pop()
			.unwrap()
			.1
	} else {
		let mut fields = vec![];
		let mut records = vec![];
		let mut output = String::new();
		for line in lines {
			for field in line {
				fields.push(field.1);
			}
			// Join the fields by the delimiter
			// Also clear fields for the next line
			let record = std::mem::take(&mut fields).join(delimiter);
			// Push the new string
			records.push(record);
		}
		for record in records {
			if record.ends_with('\n') {
				write!(output, "{record}").ok();
			} else {
				writeln!(output,"{record}").ok();
			}
		}
		output
	}
}

/// Format the output according to the given format string
///
/// We use a state machine here to interpolate the fields
/// The loop looks for patterns like {{1}} or {{foo}} to interpolate on
fn format_output_template(template: &str, lines: Vec<Vec<(String,String)>>) -> Result<String,String> {
	let mut field_name = String::new();
	let mut output = String::new();
	let mut cur_line = String::new();
	for line in lines {
		let mut chars = template.chars().peekable();
		while let Some(ch) = chars.next() {
			match ch {
				'\\' => {
					if let Some(esc_ch) = chars.next() {
						cur_line.push(esc_ch)
					}
				}
				'{' if chars.peek() == Some(&'{') => {
					chars.next();
					let mut closed = false;
					while let Some(ch) = chars.next() {
						match ch {
							'\\' => {
								if let Some(esc_ch) = chars.next() {
									field_name.push(esc_ch)
								}
							}
							'}' if chars.peek() == Some(&'}') => {
								chars.next();
								closed = true;
								break
							}
							_ => field_name.push(ch)
						}
					}
					if closed {
						let result = line
							.iter()
							.find(|(name,_)| name == &field_name)
							.map(|(_,field)| field);

						if let Some(field) = result {
							cur_line.push_str(field);
						} else {
							let mut e = String::new();
							writeln!(e,"Did not find a field called '{field_name}' for output template").ok();
							writeln!(e,"Captured field names were:").ok();
							for (name,_) in line {
								writeln!(e,"\t{name}").ok();
							}
							return Err(e)
						}
					} else {
						cur_line.extend(field_name.drain(..));
					}
					field_name.clear();
				}
				_ => cur_line.push(ch)
			}
		}
		if !cur_line.is_empty() {
			writeln!(output,"{}",std::mem::take(&mut cur_line)).ok();
		}
	}
	Ok(output)
}

/// Execute the user's commands.
///
/// Here we are going to initialize a new instance of `ViCut` to manage state for editing this input
/// Next we loop over `args.cmds` and execute each one in sequence.
fn execute(mut vicut: ViCut, args: &Opts, filename: Option<PathBuf>) -> Result<Vec<Vec<(String,String)>>,VicErr> {
	let basename = filename.clone()
		.map(|s| s.file_name().unwrap_or_default().to_string_lossy().to_string())
		.unwrap_or_else(|| String::from("stdin"));
	let filepath = filename.map(|s| s.to_string_lossy().to_string()).unwrap_or(String::from("stdin"));
	vicut.set_var("filename".into(), Val::Str(basename))?;
	vicut.set_var("filepath".into(), Val::Str(filepath))?;


	let cmds = vicut.cmds.clone();
	for cmd in cmds {
		if let Err(e) = vicut.eval_expr(/*is_top_level:*/true,&cmd) {
			match e {
				VicErr::Exit(code) => {
					std::process::exit(code);
				}
				_ => complain_and_exit(e)
			}
		}
		if !vicut.find_opt(|o| o.keep_mode).unwrap_or(false) {
			vicut.set_normal_mode();
		}
	}

	if !vicut.exec_ctx.fields.is_empty() {
		vicut.exec_ctx.push_fields();
	}

	if vicut.exec_ctx.fmt_lines.is_empty() && vicut.find_opt(|o| o.silent).unwrap_or(false) {
		return Ok(vec![]);
	}

	// Let's figure out if we want to print the whole buffer
	let no_fields = vicut.exec_ctx.fmt_lines.is_empty(); // No fields were extracted
	let has_files = vicut.find_opt(|o| o.files.clone()).is_some_and(|f| !f.is_empty());
	let editing_inplace = args.edit_inplace.unwrap_or(false); // We are not editing in place

	// If we have not extracted any fields, and the following conditions are true:
	// * We have files without editing in place, or
	// * We don't have any files, order
	// * We have a pattern search with at least one field extraction
	//
	// then we print the entire buffer
	let should_print_entire_buffer = (!editing_inplace || !has_files) && no_fields;

	if should_print_entire_buffer {
		let buf = vicut.current_buffer();
		let big_line = buf.buffer.clone();
		vicut.exec_ctx.fmt_lines.push(vec![("0".into(),big_line)]);
	}

	if vicut.find_opt(|o| o.trim_fields).unwrap_or(false) {
		vicut.exec_ctx.trim_fields();
	}

	Ok(vicut.exec_ctx.fmt_lines.clone())
}


/// Split a string slice into it's lines.
///
/// We use this instead of `String::lines()` because that method does not include the newline itself
/// in each line. The newline characters are vital to `LineBuf`'s navigation logic.
fn get_lines(value: &str) -> Vec<String> {
	let mut cur_line = String::new();
	let mut lines = vec![];
	let mut chars = value.chars();

	while let Some(ch) = chars.next() {
		match ch {
			'\n' => {
				cur_line.push(ch);
				lines.push(std::mem::take(&mut cur_line))
			}
			_ => cur_line.push(ch)
		}
	}

	if !cur_line.is_empty() {
		lines.push(std::mem::take(&mut cur_line))
	}

	lines
}

/// Multi-thread the execution of file input.
///
/// The steps this function walks through are as follows:
/// 1. Create a `work` vector containing a tuple of the file's path, and it's contents.
/// 2. Call `execute()` on each file's contents
/// 3. Decide how to handle output depending on whether args.edit_inplace is set.
fn execute_multi_thread_files(mut stdout: io::StdoutLock, args: &Opts) {
	let files = args.files.clone().unwrap();
	let work: Vec<(PathBuf, String)> = files.par_iter()
		.fold(Vec::new, |mut acc,file| {
			let contents = fs::read_to_string(file).unwrap_or_else(complain_and_exit);
			acc.push((file.clone(), contents.to_string()));
			acc
		}).reduce(Vec::new, |mut a, mut b| {
			a.append(&mut b);
			a
		});

	// Process each file's content
	let results = work.into_par_iter()
		.map(|(path, content)| {
			let vicut = ViCut::new(args.clone(), content, 0).unwrap_or_else(complain_and_exit);
			let processed = match execute(vicut, args, Some(path.clone())) {
				Ok(content) => content,
				Err(e) => {
					eprintln!("vicut: error in file '{}': {e}",path.display());
					std::process::exit(1)
				}
			};
			(path, processed)
		}).collect::<Vec<_>>();

	// Write back to file
	if args.json.unwrap_or(false)  && files.len() > 1 {
		let json = format_output_json_files(results);
		write!(stdout, "{json}").ok();
		return
	}
	for (path, contents) in results {
		let output = format_output(args, contents);

		if args.edit_inplace.unwrap_or(false) {
			if args.backup_files.unwrap_or(false) {
				let extension = args.backup_extension.as_deref().unwrap_or("bak");
				let backup_path = path.with_extension(format!(
						"{}.{extension}",
						path.extension()
						.and_then(|ext| ext.to_str())
						.unwrap_or("")
				));

				fs::copy(&path, &backup_path).unwrap_or_else(complain_and_exit);
			}
			fs::write(&path, output).unwrap_or_else(complain_and_exit);
		} else if files.len() > 1 {
			if !output.is_empty() {
				writeln!(stdout, "--- {}\n{}",path.display(), output).ok();
			}
		} else {
			write!(stdout, "{output}").ok();
		}
	}
}

/// Executes all input files line-by-line using multi-threaded processing.
///
/// This function is used for `--linewise` execution. It processes all lines in parallel,
/// transforming each line independently using the `execute()` function and then reconstructing
/// the full outputs in order.
///
/// Steps:
/// 1. Split each file into its lines.
/// 2. Combine all lines from all files into a single work pool.
/// 3. Tag each line with its originating filename and line number.
/// 4. Use a parallel iterator to transform each line using `execute()`.
/// 5. Group the transformed lines by filename in a `BTreeMap`.
/// 6. Sort each file’s lines by line number to restore the original order.
/// 7. Reconstruct each file's contents and either:
///     - Write the result back to the original file (`-i is set`)
///     - Print to `stdout`, optionally prefixed by filename (`if multiple input files`)
///
/// Errors during reading, transformation, or writing will abort the program with a diagnostic.
/// Backup files are created if `--backup-files` is enabled.
fn execute_multi_thread_files_linewise(mut stdout: io::StdoutLock, args: &Opts) {
	let files = args.files.clone().unwrap();

	let work: Vec<(PathBuf, usize, String)> = files.par_iter()
		.fold(Vec::new, |mut acc,file| {
			let contents = fs::read_to_string(file).unwrap_or_else(complain_and_exit);
			for (line_no,line) in get_lines(&contents).into_iter().enumerate() {
				acc.push((file.clone(), line_no, line.to_string()));
			}
			acc
		}).reduce(Vec::new, |mut a, mut b| {
			a.append(&mut b);
			a
		});

	// Process each line's content
	let results = work.into_par_iter()
		.map(|(path, line_no, line)| {
			let vicut = ViCut::new(args.clone(), line, 0).unwrap_or_else(complain_and_exit);
			let processed = match execute(vicut, args, Some(path.clone())) {
				Ok(line) => line,
				Err(e) => {
					eprintln!("vicut: error in file '{}', line {}: {e}",path.display(),line_no);
					std::process::exit(1)
				}
			};
			(path, line_no, processed)
		}).collect::<Vec<_>>();

	// Separate content by file
	let mut per_file: BTreeMap<PathBuf, Vec<(usize,String)>> = BTreeMap::new();
	for (path, line_no, processed) in results {
		let output = format_output(args, processed);

		per_file.entry(path)
			.or_default()
			.push((line_no,output));
	}
	if args.json.unwrap_or(false)  && files.len() > 1 {
		let results = per_file.into_iter()
			.map(|(path, lines)| (path, lines.into_iter().map(|(num,line)| vec![(num.to_string(),line)]).collect::<Vec<_>>()))
			.collect::<Vec<_>>(); // two vec collects, holy cringe
														// it'll come out in the wash
		let json = format_output_json_files(results);
		write!(stdout, "{json}").ok();
		return
	}
	// Write back to file
	for (path, mut lines) in per_file {
		lines.sort_by_key(|(line_no,_)| *line_no); // Sort lines
		let output_final = lines.into_iter()
			.map(|(_,line)| line)
			.collect::<Vec<_>>()
			.join("");

		if args.edit_inplace.unwrap_or(false) {
			if args.backup_files.unwrap_or(false) {
				let extension = args.backup_extension.as_deref().unwrap_or("bak");
				let backup_path = path.with_extension(format!(
						"{}.{extension}",
						path.extension()
						.and_then(|ext| ext.to_str())
						.unwrap_or("")
				));

				fs::copy(&path, &backup_path).unwrap_or_else(complain_and_exit);
			}
			fs::write(&path, output_final).unwrap_or_else(complain_and_exit);
		} else if files.len() > 1 {
			if !output_final.is_empty() {
				writeln!(stdout, "--- {}\n{}",path.display(), output_final).ok();
			}
		} else {
			write!(stdout, "{output_final}").ok();
		}
	}
}

/// Executes commands on lines from stdin, using multi-threaded processing
///
/// This function is used for `--linewise` execution on stdin.
/// Reads the complete input from stdin and then splits it into its lines for execution.
fn execute_linewise(mut stream: Box<dyn BufRead>, args: &Opts) -> String {
	let mut input = String::new();
	stream.read_to_string(&mut input).unwrap_or_else(complain_and_exit);
	let lines = get_lines(&input);
	// Pair each line with its original index
	let mut lines: Vec<_> = lines
		.into_par_iter()
		.enumerate()
		.map(|(i, line)| {
			let vicut = ViCut::new(args.clone(), line, 0).unwrap_or_else(complain_and_exit);
			let output = match execute(vicut, args, None) {
				Ok(line) => line,
				Err(e) => {
					eprintln!("vicut: {e}");
					std::process::exit(1)
				}
			};
			(i, output)
		})
	.collect();
	lines.sort_by_key(|(i,_)| *i);
	let mut fmt_lines = vec![];
	for (_,mut line) in lines {
		fmt_lines.append(&mut line);
	}
	format_output(args, fmt_lines)
}

/// The pathway for when the `--linewise` flag is set
///
/// Each route in this function operates on individual lines from the input
fn exec_linewise(vicut: ViCut, args: &Opts) {
	if vicut.find_opt(|o| o.single_thread).unwrap_or(false) {
		let mut stdout = io::stdout().lock();

		// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
		// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
		let mut lines = vec![];
		let mut json_data = vec![];
		if vicut.find_opt(|o| o.files.clone()).as_ref().is_some_and(|o| !o.is_empty()) {
			for path in &vicut.find_opt(|o| o.files.clone()).clone().unwrap() {
				let input = fs::read_to_string(path).unwrap_or_else(complain_and_exit);
				for line in get_lines(&input) {
					let vicut = ViCut::new(vicut.opts().clone(), line, 0).unwrap_or_else(complain_and_exit);
					match execute(vicut, args, Some(path.clone())) {
						Ok(mut new_line) => {
							lines.append(&mut new_line);
						}
						Err(e) => {
							eprintln!("vicut: {e}");
							return;
						}
					}
				}
				if vicut.find_opt(|o| o.json).unwrap_or(false) {
					json_data.push((path.clone(), std::mem::take(&mut lines)));
					continue
				}
				let mut output = format_output(args, std::mem::take(&mut lines));
				if vicut.find_opt(|o| o.edit_inplace).unwrap_or(false) {
					if vicut.find_opt(|o| o.backup_files).unwrap_or(false) {
						let extension = vicut.find_opt(|o| o.backup_extension.clone()).unwrap_or("bak".into());
						let backup_path = path.with_extension(format!(
								"{}.{extension}",
								path.extension()
								.and_then(|ext| ext.to_str())
								.unwrap_or("")
						));

						fs::copy(path, &backup_path).unwrap_or_else(complain_and_exit);
					}
					fs::write(path, std::mem::take(&mut output)).unwrap_or_else(complain_and_exit);
				} else {
					if vicut.find_opt(|o| o.files.clone()).as_ref().is_some_and(|o| o.len() > 1) {
						writeln!(stdout,"--- {}", path.display()).ok();
					}
					writeln!(stdout, "{output}").ok();
				}
			}
			if !vicut.find_opt(|o| o.json).unwrap_or(false) {
				// If we are not outputting JSON, we can just return here
				return;
			}
			let json = format_output_json_files(json_data);
			write!(stdout, "{json}").ok();
		} else {
			let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
			let mut input = String::new();
			stream.read_to_string(&mut input).unwrap_or_else(complain_and_exit);
			for line in get_lines(&input) {
				let vicut = ViCut::new(args.clone(), line, 0).unwrap_or_else(complain_and_exit);
				match execute(vicut, args, None) {
					Ok(mut new_line) => {
						lines.append(&mut new_line);
					}
					Err(e) => {
						eprintln!("vicut: {e}");
						return;
					}
				}
			}
		}
		let output = format_output(args, lines);
		writeln!(stdout, "{output}").ok();

	} else if let Some(num) = vicut.find_opt(|o| o.max_jobs) {
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(num as usize)
			.build()
			.unwrap_or_else(complain_and_exit);
		let has_files = vicut.find_opt(|o| o.files.clone()).as_ref().is_some_and(|o| !o.is_empty());
		pool.install(|| {
			let mut stdout = io::stdout().lock();
			let output = if has_files {
				execute_multi_thread_files_linewise(stdout, args);
				// Output has already been handled
				std::process::exit(0);
			} else {
				let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
				execute_linewise(stream, args)
			};
			writeln!(stdout, "{output}").ok();
		});
	} else {
		let mut stdout = io::stdout().lock();
		let output = if vicut.find_opt(|o| o.files.clone()).as_ref().is_some_and(|o| !o.is_empty()) {
			execute_multi_thread_files_linewise(stdout, args);
			// Output has already been handled
			std::process::exit(0);
		} else {
			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
			execute_linewise(stream, args)
		};
		writeln!(stdout, "{output}").ok();
	}

}

/// Execution pathway for handling filenames given as arguments
///
/// Operates on the content of the files, and either prints to stdout, or edits the files in-place
fn exec_files(vicut: ViCut, args: &Opts) {
	let mut json_data = vec![];
	if vicut.find_opt(|o| o.single_thread).unwrap_or(false) {
		let mut stdout = io::stdout().lock();
		for path in &vicut.find_opt(|o| o.files.clone()).unwrap() {
			let content = fs::read_to_string(path).unwrap_or_else(complain_and_exit);
			let new_vicut = ViCut::new(vicut.opts().clone(), content, 0).unwrap_or_else(complain_and_exit);
			match execute(new_vicut, args, Some(path.clone())) {
				Ok(output) => {
					if vicut.find_opt(|o| o.json).unwrap_or(false) {
						json_data.push((path.clone(), output));
						continue
					}
					let mut output = format_output(args, output);
					if vicut.find_opt(|o| o.edit_inplace).unwrap_or(false) {
						if vicut.find_opt(|o| o.backup_files).unwrap_or(false) {
							let extension = args.backup_extension.as_deref().unwrap_or("bak");
							let backup_path = path.with_extension(format!(
									"{}.{extension}",
									path.extension()
									.and_then(|ext| ext.to_str())
									.unwrap_or("")
							));

							fs::copy(path, &backup_path).unwrap_or_else(complain_and_exit);
						}
						fs::write(path, std::mem::take(&mut output)).unwrap_or_else(complain_and_exit);
					} else {
						if vicut.find_opt(|o| o.files.clone()).as_ref().unwrap().len() > 1 {
							writeln!(stdout,"--- {}", path.display()).ok();
						}
						writeln!(stdout,"{output}").ok();
					}
				}
				Err(e) => eprintln!("vicut: {e}"),
			};
		}
		if vicut.find_opt(|o| o.json).unwrap_or(false) {
			let json = format_output_json_files(json_data);
			write!(stdout, "{json}").ok();
		}
	} else if let Some(num) = vicut.find_opt(|o| o.max_jobs) {
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(num as usize)
			.build()
			.unwrap_or_else(complain_and_exit);
		pool.install(|| {
			let stdout = io::stdout().lock();
			execute_multi_thread_files(stdout, args);
		});
	} else {
		let stdout = io::stdout().lock();
		execute_multi_thread_files(stdout, args);
	}

}

/// Default execution pathway. Operates on `stdin`.
///
/// Simplest of the three routes.
fn exec_stdin(mut vicut: ViCut, args: &Opts) {
	let mut stdout = io::stdout().lock();
	let mut lines = vec![];
	let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
	let mut input = String::new();
	match stream.read_to_string(&mut input) {
		Ok(_) => {}
		Err(e) => {
			eprintln!("vicut: {e}");
			return;
		}
	}
	*vicut.current_buffer_mut() = LineBuf::new().with_initial(input, 0);
	match execute(vicut, args, None) {
		Ok(mut output) => {
			lines.append(&mut output);
		}
		Err(e) => eprintln!("vicut: {e}"),
	};
	let output = format_output(args, lines);
	writeln!(stdout,"{output}").ok();

}

/// Testing fixture for the debug profile
#[cfg(all(test,debug_assertions))]
fn do_test_stuff() {
	// Testing
		let input = "abcdefgh\nabcd\nabcdefghi\nabcde\nabcdefg";
	println!("{input}\n");

	let args = [
			"-m", "$<c-v>0lGdp",
	];
	let output = tests::call_main(&args, input).unwrap();
	//assert_eq!(output, "adbcefgh\nadbc\nadbcefghi\nadbce\nadbcefg");
	println!("{output}");
	std::process::exit(0);

}

/// Print help or version info and exit early if `--help` or `--version` are found
fn print_help_or_version() {
	if std::env::args().skip(1).count() == 0 {
		eprintln!("USAGE:");
		eprintln!("\tvicut [OPTIONS] [COMMANDS]...");
		eprintln!();
		eprintln!("use '--help' for more information");
		std::process::exit(0);
	}
	if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
		print!("{}",get_help());
		std::process::exit(0);
	}
	if std::env::args().any(|arg| arg == "--version") {
		println!("vicut {}", env!("CARGO_PKG_VERSION"));
		std::process::exit(0);
	}

}

fn main_script() {
	// Is it a script file? or an in-line script?
	let maybe_script = std::env::args().nth(1).unwrap();
	let mut opts = Opts::default();
	let script_content = if Path::new(&maybe_script).is_file() {
		// It's a file?
		// Let's read it and see if it parses
		fs::read_to_string(&maybe_script).unwrap()
	} else {
		// It's a raw script as an argument?
		// Let's just parse it
		maybe_script
	};
	opts.vic_raw = Some(script_content);
	let vicut = ViCut::new(opts.clone(), String::new(), 0).unwrap_or_else(complain_and_exit);


	init_logger(opts.trace.unwrap_or(false));

	if vicut.find_opt(|o| o.no_input).unwrap_or(false) {
		let output = execute(vicut, &opts, None).unwrap_or_else(complain_and_exit);
		let mut stdout = io::stdout().lock();
		let output = format_output(&opts, output);
		write!(stdout, "{output}").ok();
	} else if vicut.find_opt(|o| o.linewise).unwrap_or(false) {
		exec_linewise(vicut,&opts);
	} else if vicut.find_opt(|o| o.files.clone()).as_ref().is_some_and(|f| !f.is_empty()) {
		exec_files(vicut,&opts);
	} else {
		exec_stdin(vicut,&opts);
	}
}

#[allow(unreachable_code)]
fn main() {
	//#[cfg(all(test,debug_assertions))]
	//do_test_stuff();

	print_help_or_version();

	if std::env::args().count() == 2 {
		// We're probably running in a standalone vic script
		return main_script()
	}

	let opts = Opts::from_cmd_line_args().unwrap_or_else(complain_and_exit);
	let vicut = ViCut::new(opts.clone(), String::new(), 0).unwrap_or_else(complain_and_exit);
	dbg!(&vicut.opts);

	init_logger(vicut.find_opt(|o| o.trace).unwrap_or_default());

	if vicut.find_opt(|o| o.no_input).unwrap_or_default() {
		let output = execute(vicut, &opts, None).unwrap_or_else(complain_and_exit);
		let mut stdout = io::stdout().lock();
		let output = format_output(&opts, output);
		write!(stdout, "{output}").ok();
	} else if vicut.find_opt(|o| o.linewise).unwrap_or_default() {
		exec_linewise(vicut,&opts);
	} else if vicut.find_opt(|o| o.files.clone()).as_ref().is_some_and(|f| !f.is_empty()) {
		exec_files(vicut,&opts);
	} else {
		exec_stdin(vicut,&opts);
	}
}

pub fn validate_filename(filename: &str) -> Result<(),String> {
	let path = PathBuf::from(filename.trim().to_string());
	if !path.exists() {
		return Err(format!("vicut: file not found '{}'",path.display()));
	}
	if !path.is_file() {
		return Err(format!("vicut: '{}' is not a file",path.display()));
	}
	if fs::File::open(&path).is_err() {
		return Err(format!("vicut: failed to read file '{}'",path.display()));
	}
	Ok(())
}
