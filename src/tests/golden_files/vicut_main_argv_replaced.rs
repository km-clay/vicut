#![allow(clippy::unnecessary_to_owned,clippy::while_let_on_iterator)]
#![feature(let_chains)]
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
use std::{collections::BTreeMap, env::Args, fmt::Write, fs, io::{self, BufRead, Write as IoWrite}, iter::{Peekable, Skip}, path::PathBuf};

extern crate tikv_jemallocator;

#[cfg(target_os = "linux")]
#[global_allocator]
/// For linux we use Jemalloc. It is ***significantly*** faster than the default allocator in this case, for some reason.
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use exec::ViCut;
use log::trace;
use serde_json::{Map, Value};
use rayon::prelude::*;

use crate::{linebuf::MotionKind, vicmd::{LineAddr, Motion, MotionCmd}};

pub mod vicmd;
pub mod modes;
pub mod exec;
pub mod linebuf;
pub mod keys;
pub mod register;
pub mod reader;
#[cfg(test)]
pub mod tests;

/// The field name used in `Cmd::NamedField`
pub type Name = String;

#[derive(Clone,Debug)]
enum Cmd {
	Motion(String),
	Field(String),
	NamedField(Name,String),
	Repeat(usize,usize),
	Global{
		pattern: String,
		then_cmds: Vec<Cmd>,
		else_cmds: Option<Vec<Cmd>>,
		polarity: bool // Whether to execute on a match, or on no match
	},
	BreakGroup
}

/// The arguments passed to the program by the user
#[derive(Default,Clone,Debug)]
struct Arguments {
	delimiter: Option<String>,
	template: Option<String>,
	max_jobs: Option<u32>,
	backup_extension: Option<String>,

	edit_inplace: bool,
	json: bool,
	trace: bool,
	linewise: bool,
	trim_fields: bool,
	keep_mode: bool,
	backup_files: bool,
	single_thread: bool,

	cmds: Vec<Cmd>,
	files: Vec<PathBuf>
}

impl Arguments {
	/// Parse the user's arguments
	pub fn parse() -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = std::env::args().skip(1).peekable();
		while let Some(arg) = args.next() {
			match arg.as_str() {
				"--json" | "-j" => {
					new.json = true;
				}
				"--trace" => {
					new.trace = true;
				}
				"--linewise" => {
					new.linewise = true;
				}
				"--serial" => {
					new.single_thread = true;
				}
				"--trim-fields" => {
					new.trim_fields = true;
				}
				"--keep-mode" => {
					new.keep_mode = true;
				}
				"--backup" => {
					new.backup_files = true;
				}
				"-i" => {
					new.edit_inplace = true;
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
				"-n" | "--next" => new.cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.map_err(|_| format!("Expected a number after '{arg}'"))?;
					let repeat_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.map_err(|_| format!("Expected a number after '{arg}'"))?;

					new.cmds.push(Cmd::Repeat(cmd_count, repeat_count));
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a motion command after '-m', found {arg}"))
					}
					new.cmds.push(Cmd::Motion(arg))
				}
				"-c" | "--cut" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with("name=") {
						let name = arg.strip_prefix("name=").unwrap().to_string();
						if name == "0" {
							// We use '0' as a sentinel value to say "We didn't slice any fields, so this field is the entire buffer"
							// So we can't let people use it arbitrarily, or weird shit starts happening
							return Err("Field name '0' is a reserved field name.".into())
						}
						let Some(arg) = args.next() else { continue };
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::NamedField(name,arg));
					} else {
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::Field(arg));
					}
				}
				"-v" | "--not-global" |
				"-g" | "--global" => {
					let global = new.handle_global_arg(arg.as_str(), &mut args);
					new.cmds.push(global);
				}
				_ => new.handle_filename(arg)
			}
		}
		Ok(new)
	}
	/// Handles `-g` and `-v` global conditionals.
	///
	/// `-g` and `-v` are special cases: each introduces a scoped block of commands
	/// that will only execute if a pattern match (or non-match) succeeds. These blocks
	/// can contain other nested `-g` or `-v` invocations, as well as `--else` branches
	/// and `-r` repeats.
	///
	/// Because of this recursive structure, we use a recursive descent parser to
	/// build a nested command execution tree from the input. This allows arbitrarily
	/// deep combinations of conditionals and scopes, like:
	///
	/// ```bash
	/// vicut -g 'foo' -g 'bar' -c 'd' --else -v 'baz' -c 'y' --end --end
	/// ```
	fn handle_global_arg(&mut self,arg: &str, args: &mut Peekable<Skip<Args>>) -> Cmd {
		let polarity = match arg {
			"-v" | "--not-global" => false,
			"-g" | "--global" => true,
			_ => unreachable!("found arg: {arg}")
		};
		let mut then_cmds = vec![];
		let mut else_cmds = None;
		let Some(arg) = args.next() else {
			return Cmd::Global {
				pattern: arg.into(),
				then_cmds,
				else_cmds,
				polarity
			};
		};
		if arg.starts_with('-') {
			eprintln!("Expected a selection command after '-c', found {arg}");
			std::process::exit(1)
		}
		while let Some(global_arg) = args.next() {
			match global_arg.as_str() {
				"-n" | "--next" => self.cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or_else(|_| {
							eprintln!("Expected a number after '{global_arg}'");
							std::process::exit(1)
						});
					let repeat_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or_else(|_| {
							eprintln!("Expected a number after '{global_arg}'");
							std::process::exit(1)
						});

					if let Some(else_cmds) = else_cmds.as_mut() {
						else_cmds.push(Cmd::Repeat(cmd_count, repeat_count));
					} else {
						then_cmds.push(Cmd::Repeat(cmd_count, repeat_count));
					}
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						eprintln!("Expected a motion command after '-m', found {arg}");
						std::process::exit(1);
					}
					if let Some(else_cmds) = else_cmds.as_mut() {
						else_cmds.push(Cmd::Motion(arg))
					} else {
						then_cmds.push(Cmd::Motion(arg))
					}
				}
				"-c" | "--cut" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with("name=") {
						let name = arg.strip_prefix("name=").unwrap().to_string();
						let Some(arg) = args.next() else { continue };
						if arg.starts_with('-') {
							eprintln!("Expected a selection command after '-c', found {arg}");
							std::process::exit(1);
						}
						if let Some(cmds) = else_cmds.as_mut() {
							cmds.push(Cmd::NamedField(name,arg));
						} else {
							then_cmds.push(Cmd::NamedField(name,arg));
						}
					} else {
						if arg.starts_with('-') {
							eprintln!("Expected a selection command after '-c', found {arg}");
							std::process::exit(1);
						}
						if let Some(cmds) = else_cmds.as_mut() {
							cmds.push(Cmd::Field(arg));
						} else {
							then_cmds.push(Cmd::Field(arg));
						}
					}
				}
				"-g" | "--global" |
				"-v" | "--not-global" => {
					let nested = self.handle_global_arg(&global_arg, args);
					if let Some(cmds) = else_cmds.as_mut() {
						cmds.push(nested);
					} else {
						then_cmds.push(nested);
					}
				}
				"--else" => {
					// Now we start working on this
					else_cmds = Some(vec![]);
				}
				"--end" => {
					// We're done here
					return Cmd::Global {
						pattern: arg,
						then_cmds,
						else_cmds,
						polarity
					};
				}
				_ => {
					eprintln!("Expected command flag in '-g' scope\nDid you forget to close '-g' with '--end'?");
					std::process::exit(1);
				}
			}
			if args.peek().is_some_and(|arg| !arg.starts_with('-')) { break }
		}

		// If we got here, we have run out of arguments
		// Let's just submit the current -g commands.
		// no need to be pressed about a missing '--end' when nothing would come after it
		Cmd::Global {
			pattern: arg,
			then_cmds,
			else_cmds,
			polarity
		}
	}
	/// Handle a filename passed as an argument.
	///
	/// Checks to make sure the following invariants are met:
	/// 1. The path given exists.
	/// 2. The path given refers to a file.
	/// 3. The path given refers to a file that we are allowed to read.
	///
	/// We check all three separately instead of just the last one, so that we can give better error messages
	fn handle_filename(&mut self, filename: String) {
		let path = PathBuf::from(filename.trim().to_string());
		if !path.exists() {
			eprintln!("vicut: file not found '{}'",path.display());
			std::process::exit(1);
		}
		if !path.is_file() {
			eprintln!("vicut: '{}' is not a file",path.display());
			std::process::exit(1);
		}
		if fs::File::open(&path).is_err() {
			eprintln!("vicut: failed to read file '{}'",path.display());
			std::process::exit(1);
		}
		if !self.files.contains(&path) {
			self.files.push(path)
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
fn format_output(args: &Arguments, lines: Vec<Vec<(String,String)>>) -> String {
	if args.json {
		Ok(format_output_json(lines))
	} else if let Some(template) = args.template.as_deref() {
		format_output_template(template, lines)
	} else {
		let delimiter = args.delimiter.as_deref().unwrap_or(" ");
		Ok(format_output_standard(delimiter, lines))
	}.unwrap_or_else(|e| {
		eprintln!("vicut: failed to format output: {e}");
		std::process::exit(1)
	})
}

/// Format the output as JSON
fn format_output_json(lines: Vec<Vec<(String,String)>>) -> String {
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
fn execute(args: &Arguments, input: String) -> Result<Vec<Vec<(String,String)>>,String> {
	let mut fields: Vec<(String,String)> = vec![];
	let mut fmt_lines: Vec<Vec<(String,String)>> = vec![];

	let mut vicut = ViCut::new(input, 0)?;

	let mut spent_cmds: Vec<&Cmd> = vec![];

	let mut field_num = 0;
	let has_global = args.cmds.iter().any(|cmd| matches!(cmd,Cmd::Global {..}));
	for cmd in &args.cmds {
		exec_cmd(
			cmd,
			args,
			&mut vicut,
			&mut field_num,
			&mut spent_cmds,
			&mut fields,
			&mut fmt_lines
		);
		spent_cmds.push(cmd);
		if !args.keep_mode {
			vicut.set_normal_mode();
		}
	}

	if !fields.is_empty() {
		fmt_lines.push(std::mem::take(&mut fields));
	}

	// Let's figure out if we want to print the whole buffer
	// fmt_lines is empty, so the user didn't write any -c commands
	// if args has files it is working on, and the command list has a global, that means
	// that the user is probably searching for something, potentially in a group of files.
	// We don't want to spam the output with entire files with no matches in that case,
	// But if the files vector is empty, the user is working on stdin, so they will probably
	// want to see that output, with or without globals.
	if fmt_lines.is_empty() && ((!args.files.is_empty() && !has_global) || (args.files.is_empty())) {
		let big_line = vicut.editor.buffer;
		fmt_lines.push(vec![("0".into(),big_line)]);
	}

	if args.trim_fields {
		trim_fields(&mut fmt_lines);
	}

	Ok(fmt_lines)
}

/// Trim the fields 🧑‍🌾
fn trim_fields(lines: &mut Vec<Vec<(String,String)>>) {
	for line in lines {
		for (_, field) in line {
			*field = field.trim().to_string()
		}
	}
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

/// Execute a single `Cmd`
fn exec_cmd(
	cmd: &Cmd,
	args: &Arguments,
	vicut: &mut ViCut,
	field_num: &mut usize,
	spent_cmds: &mut Vec<&Cmd>,
	fields: &mut Vec<(String,String)>,
	fmt_lines: &mut Vec<Vec<(String,String)>>
) {
	match cmd {
		// -r <N> <R>
		Cmd::Repeat(n_cmds, n_repeats) => {
			trace!("Repeating {n_cmds} commands, {n_repeats} times");
			for _ in 0..*n_repeats {

				let mut pulled_cmds = vec![];
				let total = spent_cmds.len();
				let start = total.saturating_sub(*n_cmds);
				pulled_cmds.extend(spent_cmds.drain(start..));

				for r_cmd in pulled_cmds {
					// We use recursion so that we can nest repeats easily
					exec_cmd(
						r_cmd,
						args,
						vicut,
						field_num,
						spent_cmds,
						fields,
						fmt_lines
					);
					spent_cmds.push(r_cmd);
				}
			}
		}
		// -g/-v <PATTERN> <COMMANDS> [--else <COMMANDS>]
		Cmd::Global { pattern, then_cmds, else_cmds, polarity } => {
			let motion = match polarity {
				false  => Motion::NotGlobal(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern.to_string()),
				true => Motion::Global(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern.to_string())
			};

			// Here we ask ViCut's editor directly to evaluate the Global motion for us.
			// LineBuf::eval_motion() *always* returns MotionKind::Lines() for Motion::Global/NotGlobal.
			let MotionKind::Lines(lines) = vicut.editor.eval_motion(None, MotionCmd(1,motion)) else { unreachable!() };
			let mut scoped_spent_cmds = vec![];
			if !lines.is_empty() {
				// Positive branch
				for line in lines {
					let Some((start,_)) = vicut.editor.line_bounds(line) else { continue };
					// Set the cursor on the start of the line
					vicut.editor.cursor.set(start);
					// Execute our commands
					for cmd in then_cmds {
						exec_cmd(
							cmd,
							args,
							vicut,
							field_num,
							&mut scoped_spent_cmds,
							fields,
							fmt_lines
						);
						scoped_spent_cmds.push(cmd);
						if !args.keep_mode {
							vicut.set_normal_mode();
						}
					}
				}
			} else if let Some(else_cmds) = else_cmds {
				// Negative branch
				for cmd in else_cmds {
					exec_cmd(
						cmd,
						args,
						vicut,
						field_num,
						&mut scoped_spent_cmds,
						fields,
						fmt_lines
					);
					scoped_spent_cmds.push(cmd);
					if !args.keep_mode {
						vicut.set_normal_mode();
					}
				}
			}
		}
		// -m <VIM_CMDS>
		Cmd::Motion(motion) => {
			trace!("Executing non-capturing command: {motion}");
			if let Err(e) = vicut.move_cursor(motion) {
				eprintln!("vicut: {e}");
			}
		}
		// -c <VIM_CMDS>
		Cmd::Field(motion) => {
			trace!("Executing capturing command: {motion}");
			*field_num += 1;
			match vicut.read_field(motion) {
				Ok(field) => {
					let name = format!("{field_num}");
					fields.push((name,field))
				}
				Err(e) => {
					eprintln!("vicut: {e}");
				}
			}
		}
		// -c name=<NAME> <VIM_CMDS>
		Cmd::NamedField(name, motion) => {
			trace!("Executing capturing command with name '{name}': {motion}");
			*field_num += 1;
			match vicut.read_field(motion) {
				Ok(field) => fields.push((name.clone(),field)),
				Err(e) => {
					eprintln!("vicut: {e}");
				}
			}
		}
		// -n
		Cmd::BreakGroup => {
			if args.trace {
				trace!("Breaking field group with fields: ");
				for field in &mut *fields {
					let name = &field.0;
					let content = &field.1;
					trace!("\t{name}: {content}");
				}
			}
			*field_num = 0;
			if !fields.is_empty() {
				fmt_lines.push(std::mem::take(fields));
			}
		}
	}
}

/// Multi-thread the execution of file input.
///
/// The steps this function walks through are as follows:
/// 1. Create a `work` vector containing a tuple of the file's path, and it's contents.
/// 2. Call `execute()` on each file's contents
/// 3. Decide how to handle output depending on whether args.edit_inplace is set.
fn execute_multi_thread_files(mut stdout: io::StdoutLock, args: &Arguments) {
	let work: Vec<(PathBuf, String)> = args.files.par_iter()
		.fold(Vec::new, |mut acc,file| {
			let contents = fs::read_to_string(file).unwrap_or_else(|e| {
				eprintln!("vicut: failed to read file '{}': {e}",file.display());
				std::process::exit(1);
			});
			acc.push((file.clone(), contents.to_string()));
			acc
		}).reduce(Vec::new, |mut a, mut b| {
			a.append(&mut b);
			a
		});

	// Process each file's content
	let results = work.into_par_iter()
		.map(|(path, content)| {
			let processed = match execute(args, content) {
				Ok(content) => content,
				Err(e) => {
					eprintln!("vicut: error in file '{}': {e}",path.display());
					std::process::exit(1)
				}
			};
			(path, processed)
		}).collect::<Vec<_>>();

	// Write back to file
	for (path, contents) in results {
		let output = format_output(args, contents);

		if args.edit_inplace {
			if args.backup_files {
				let extension = args.backup_extension.as_deref().unwrap_or("bak");
				let backup_path = path.with_extension(format!(
						"{}.{extension}",
						path.extension()
						.and_then(|ext| ext.to_str())
						.unwrap_or("")
				));

				fs::copy(&path, &backup_path).unwrap_or_else(|e| {
					eprintln!("vicut: failed to back up file '{}': {e}", path.display());
					std::process::exit(1)
				});
			}
			fs::write(&path, output).unwrap_or_else(|e| {
				eprintln!("vicut: failed to write to file '{}': {e}",path.display());
				std::process::exit(1)
			});
		} else if args.files.len() > 1 {
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
fn execute_multi_thread_files_linewise(mut stdout: io::StdoutLock, args: &Arguments) {

	let work: Vec<(PathBuf, usize, String)> = args.files.par_iter()
		.fold(Vec::new, |mut acc,file| {
			let contents = fs::read_to_string(file).unwrap_or_else(|e| {
				eprintln!("vicut: failed to read file '{}': {e}",file.display());
				std::process::exit(1);
			});
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
			let processed = match execute(args, line) {
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
	// Write back to file
	for (path, mut lines) in per_file {
		lines.sort_by_key(|(line_no,_)| *line_no); // Sort lines
		let output_final = lines.into_iter()
			.map(|(_,line)| line)
			.collect::<Vec<_>>()
			.join("");

		if args.edit_inplace {
			if args.backup_files {
				let extension = args.backup_extension.as_deref().unwrap_or("bak");
				let backup_path = path.with_extension(format!(
						"{}.{extension}",
						path.extension()
						.and_then(|ext| ext.to_str())
						.unwrap_or("")
				));

				fs::copy(&path, &backup_path).unwrap_or_else(|e| {
					eprintln!("vicut: failed to back up file '{}': {e}", path.display());
					std::process::exit(1)
				});
			}
			fs::write(&path, output_final).unwrap_or_else(|e| {
				eprintln!("vicut: failed to write to file '{}': {e}",path.display());
				std::process::exit(1)
			});
		} else if args.files.len() > 1 {
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
fn execute_linewise(mut stream: Box<dyn BufRead>, args: &Arguments) -> String {
	let mut input = String::new();
	stream.read_to_string(&mut input).unwrap_or_else(|e| {
		eprintln!("vicut: failed to read input: {e}");
		std::process::exit(1)
	});
	let lines = get_lines(&input);
	// Pair each line with its original index
	let mut lines: Vec<_> = lines
		.into_par_iter()
		.enumerate()
		.map(|(i, line)| {
			let output = match execute(args, line) {
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
fn exec_linewise(args: &Arguments) {
	if args.single_thread {
		let mut stdout = io::stdout().lock();

		// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
		// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
		let mut lines = vec![];
		if !args.files.is_empty() {
			for path in &args.files {
				let input = fs::read_to_string(path).unwrap_or_else(|e| {
					eprintln!("vicut: failed to read file '{}': {e}",path.display());
					std::process::exit(1)
				});
				for line in get_lines(&input) {
					match execute(args,line) {
						Ok(mut new_line) => {
							lines.append(&mut new_line);
						}
						Err(e) => {
							eprintln!("vicut: {e}");
							return;
						}
					}
				}
				let mut output = format_output(args, std::mem::take(&mut lines));
				if args.edit_inplace {
					if args.backup_files {
						let extension = args.backup_extension.as_deref().unwrap_or("bak");
						let backup_path = path.with_extension(format!(
								"{}.{extension}",
								path.extension()
								.and_then(|ext| ext.to_str())
								.unwrap_or("")
						));

						fs::copy(path, &backup_path).unwrap_or_else(|e| {
							eprintln!("vicut: failed to back up file '{}': {e}", path.display());
							std::process::exit(1)
						});
					}
					fs::write(path, std::mem::take(&mut output)).unwrap_or_else(|e| {
						eprintln!("vicut: failed to write to file '{}': {e}",path.display());
						std::process::exit(1)
					});
				} else {
					if args.files.len() > 1 {
						writeln!(stdout,"--- {}", path.display()).ok();
					}
					writeln!(stdout, "{output}").ok();
				}
			}
		} else {
			let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
			let mut input = String::new();
			stream.read_to_string(&mut input).unwrap_or_else(|e| {
				eprintln!("vicut: failed to read input: {e}");
				std::process::exit(1)
			});
			for line in get_lines(&input) {
				match execute(args,line) {
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

	} else if let Some(num) = args.max_jobs {
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(num as usize)
			.build()
			.unwrap_or_else(|e| {
				eprintln!("vicut: Failed to build thread pool: {e}");
				std::process::exit(1)
			});
		pool.install(|| {
			let mut stdout = io::stdout().lock();
			let output = if !args.files.is_empty() {
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
		let output = if !args.files.is_empty() {
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
fn exec_files(args: &Arguments) {
	if args.single_thread {
		let mut stdout = io::stdout().lock();
		for path in &args.files {
			let content = fs::read_to_string(path).unwrap_or_else(|e| {
				eprintln!("vicut: failed to read file '{}': {e}",path.display());
				std::process::exit(1)
			});
			match execute(args,content) {
				Ok(output) => {
					let mut output = format_output(args, output);
					if args.edit_inplace {
						if args.backup_files {
							let extension = args.backup_extension.as_deref().unwrap_or("bak");
							let backup_path = path.with_extension(format!(
									"{}.{extension}",
									path.extension()
									.and_then(|ext| ext.to_str())
									.unwrap_or("")
							));

							fs::copy(path, &backup_path).unwrap_or_else(|e| {
								eprintln!("vicut: failed to back up file '{}': {e}", path.display());
								std::process::exit(1)
							});
						}
						fs::write(path, std::mem::take(&mut output)).unwrap_or_else(|e| {
							eprintln!("vicut: failed to write to file '{}': {e}",path.display());
							std::process::exit(1)
						});
					} else {
						if args.files.len() > 1 {
							writeln!(stdout,"--- {}", path.display()).ok();
						}
						writeln!(stdout,"{output}").ok();
					}
				}
				Err(e) => eprintln!("vicut: {e}"),
			};
		}
	} else if let Some(num) = args.max_jobs {
		let pool = rayon::ThreadPoolBuilder::new()
			.num_threads(num as usize)
			.build()
			.unwrap_or_else(|e| {
				eprintln!("vicut: Failed to build thread pool: {e}");
				std::process::exit(1)
			});
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
fn exec_stdin(args: &Arguments) {
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
	match execute(args,input) {
		Ok(mut output) => {
			lines.append(&mut output);
		}
		Err(e) => eprintln!("vicut: {e}"),
	};
	let output = format_output(args, lines);
	writeln!(stdout,"{output}").ok();

}

/// Testing fixture for the debug profile
#[cfg(debug_assertions)]
fn do_test_stuff() {
	// Testing
		let input = "abcdefgh\nabcd\nabcdefghi\nabcde\nabcdefg";
	println!("{input}\n");

	let args = [
			"-m", "$<c-v>0lGdp",
	];
	let output = call_main(&args, input).unwrap();
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


#[allow(unreachable_code)]
fn main() {
	#[cfg(debug_assertions)]
	do_test_stuff();

	print_help_or_version();

	let args = match Arguments::parse() {
		Ok(args) => args,
		Err(e) => {
			eprintln!("vicut: {e}");
			return;
		}
	};

	init_logger(args.trace);

	if args.linewise {
		exec_linewise(&args);
	} else if !args.files.is_empty() {
		exec_files(&args);
	} else {
		exec_stdin(&args);
	}
}

/*
 * Stuff down here is for testing
 */

/// Testing fixture
/// Used to call the main logic internally
#[cfg(any(test,debug_assertions))]
fn call_main(args: &[&str], input: &str) -> Result<String,String> {
	if args.is_empty() {
		let mut output = String::new();
		write!(output,"USAGE:").ok();
		write!(output,"\tvicut [OPTIONS] [COMMANDS]...").ok();
		writeln!(output).ok();
		write!(output,"use '--help' for more information").ok();
		return Err(output)
	}
	if args.iter().any(|arg| *arg == "--help" || *arg == "-h") {
		return Ok(get_help())
	}
	let args = match Arguments::parse_raw(args) {
		Ok(args) => args,
		Err(e) => {
			return Err(format!("vicut: {e}"));
		}
	};

	if args.json && args.delimiter.is_some() {
		eprintln!("vicut: WARNING: --delimiter flag is ignored when --json is used")
	}

	use std::io::Cursor;
	if args.linewise {
		if args.single_thread {
			// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
			// So using it in pool.install() doesn't work. We have to initialize it in the closure there.

			let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input)));
			let mut input = String::new();
			stream.read_to_string(&mut input).unwrap();
			let mut lines = vec![];
			for line in get_lines(&input) {
				match execute(&args,line) {
					Ok(mut new_line) => {
						lines.append(&mut new_line);
					}
					Err(e) => {
						return Err(format!("vicut: {e}"));
					}
				}
			}
			let output = format_output(&args, lines);
			Ok(output)
		} else if let Some(num) = args.max_jobs {
			let pool = rayon::ThreadPoolBuilder::new()
				.num_threads(num as usize)
				.build()
				.unwrap_or_else(|e| {
					eprintln!("vicut: Failed to build thread pool: {e}");
					std::process::exit(1)
				});
			Ok(pool.install(|| {
				let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input.to_string())));
				execute_linewise(stream, &args)
			}))
		} else {
			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input.to_string())));
			Ok(execute_linewise(stream, &args))
		}
	} else {
		let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input)));
		let mut input = String::new();
		let mut lines = vec![];
		match stream.read_to_string(&mut input) {
			Ok(_) => {}
			Err(e) => {
				return Err(format!("vicut: {e}"));
			}
		}
		match execute(&args,input) {
			Ok(mut output) => {
				lines.append(&mut output);
			}
			Err(e) => eprintln!("vicut: {e}"),
		};
		let output = format_output(&args, lines);
		Ok(output)
	}
}
#[cfg(any(test,debug_assertions))]
impl Arguments {
	pub fn parse_raw(args: &[&str]) -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = args.iter();
		while let Some(arg) = args.next() {
			match *arg {
				"--json" | "-j" => {
					new.json = true;
				}
				"--trace" => {
					new.trace = true;
				}
				"--linewise" => {
					new.linewise = true;
				}
				"--serial" => {
					new.single_thread = true;
				}
				"--trim-fields" => {
					new.trim_fields = true;
				}
				"--keep-mode" => {
					new.keep_mode = true;
				}
				"--backup" => {
					new.backup_files = true;
				}
				"-i" => {
					new.edit_inplace = true;
				}
				"--backup-extension" => {
					let Some(next_arg) = args.next() else {
						return Err(format!("Expected a string after '{arg}'"))
					};
					if next_arg.starts_with('-') {
						return Err(format!("Expected a string after '{arg}', found {next_arg}"))
					}
					new.backup_extension = Some(next_arg.to_string())
				}
				"--template" | "-t" => {
					let Some(next_arg) = args.next() else {
						return Err(format!("Expected a format string after '{arg}'"))
					};
					if next_arg.starts_with('-') {
						return Err(format!("Expected a format string after '{arg}', found {next_arg}"))
					}
					new.template = Some(next_arg.to_string())
				}
				"--delimiter" | "-d" => {
					let Some(next_arg) = args.next() else { continue };
					if next_arg.starts_with('-') {
						return Err(format!("Expected a delimiter after '{arg}', found {next_arg}"))
					}
					new.delimiter = Some(next_arg.to_string())
				}
				"-n" | "--next" => new.cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or(&"1")
						.parse::<usize>()
						.map_err(|_| format!("Expected a number after '{arg}'"))?;
					let repeat_count = args
						.next()
						.unwrap_or(&"1")
						.parse::<usize>()
						.map_err(|_| format!("Expected a number after '{arg}'"))?;

					new.cmds.push(Cmd::Repeat(cmd_count, repeat_count));
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a motion command after '-m', found {arg}"))
					}
					new.cmds.push(Cmd::Motion(arg.to_string()))
				}
				"-c" | "--cut" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with("name=") {
						let name = arg.strip_prefix("name=").unwrap().to_string();
						let Some(arg) = args.next() else { continue };
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::NamedField(name,arg.to_string()));
					} else {
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::Field(arg.to_string()));
					}
				}
				_ => new.handle_filename(arg.to_string())
			}
		}
		Ok(new)
	}
}
