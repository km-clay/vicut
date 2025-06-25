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
use std::{collections::BTreeMap, env::Args, fmt::{Display, Write}, fs, io::{self, BufRead, Write as IoWrite}, iter::{Peekable, Skip}, path::PathBuf};

extern crate tikv_jemallocator;

#[cfg(target_os = "linux")]
#[global_allocator]
/// For linux we use Jemalloc. It is ***significantly*** faster than the default allocator in this case, for some reason.
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use exec::{CompoundVal, Val, ViCut};
use log::trace;
use register::{append_register, write_register, RegisterContent};
use serde_json::{map, Map, Value};
use rayon::prelude::*;
use vic::{BinOp, CmdArg, Expr};

use crate::{linebuf::MotionKind, vicmd::{LineAddr, Motion, MotionCmd}};

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
	let mut err = err.to_string();
	if !err.starts_with("vicut: ") {
		err = format!("vicut: {err}");
	}
	eprintln!("{err}");
	std::process::exit(1)
}

pub struct ExecCtx {
	args: Opts,
	field_num: usize,
	fields: Vec<(String,String)>, // (name, value)
	fmt_lines: Vec<Vec<(String,String)>>, // Lines to format output from
}

#[derive(Clone,Debug, PartialEq)]
pub enum Cmd {
	BreakGroup,
	LoopContinue,
	LoopBreak,
	GetBufId,
	SwitchBuf(CmdArg), // Switch to a different buffer
	Echo(Vec<CmdArg>),
	Motion(CmdArg),
	Field(CmdArg),
	Return(CmdArg),
	Push(CmdArg,CmdArg), // Push a value onto an array or string
	Pop(CmdArg), 				 // Pop a value from an array or string
	Yank(CmdArg,char), // The char is the register to yank into
	NamedField(Name,CmdArg),
	Repeat {
		body: Vec<Cmd>,
		count: CmdArg
	},
	Global{
		pattern: CmdArg,
		then_cmds: Vec<Cmd>,
		else_cmds: Option<Vec<Cmd>>,
		polarity: bool // Whether to execute on a match, or on no match
	},
	VarDec {
		name: String,
		value: CmdArg
	},
	MutateVar {
		name: String,
		index: Option<CmdArg>,
		op: BinOp,
		value: CmdArg
	},
	FuncCall {
		name: String,
		args: Vec<CmdArg>
	},
	FuncDef {
		name: String,
		args: Vec<String>, // Names of the arguments
		body: Vec<Cmd>
	},
	ForBlock {
		var_name: String,
		iterable: CmdArg, // Must be a String or Array
											// Strings iterate over characters
		body: Vec<Cmd>
	},
	IfBlock {
		cond_blocks: Vec<CondBlock>,
		else_block: Option<Vec<Cmd>>
	},
	WhileBlock(CondBlock),
	UntilBlock(CondBlock),
}

#[derive(Clone,Debug,PartialEq)]
pub struct CondBlock {
	cond: CmdArg, // Must be a CmdArg::Expr(Expr::BoolExp{..})
	cmds: Vec<Cmd>,
}

/// The arguments passed to the program by the user
#[derive(Default,Clone,Debug)]
pub struct Opts {
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
	global_uses_line_numbers: bool,
	no_input: bool,
	silent: bool,

	pipe_in: Option<String>,
	pipe_out: Option<String>,
	out_file: Option<PathBuf>,

	cmds: Vec<Cmd>,
	files: Vec<PathBuf>
}

impl Opts {
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
				"--global-uses-line-numbers" => {
					new.global_uses_line_numbers = true;
				}
				"--silent" => {
					new.silent = true;
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

					let mut body = vec![];
					let drain_count = new.cmds.len() - cmd_count;
					body.extend(new.cmds.drain(drain_count..));
					new.cmds.push(Cmd::Repeat{ body, count: CmdArg::Count(repeat_count + 1) });
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a motion command after '-m', found {arg}"))
					}
					new.cmds.push(Cmd::Motion(CmdArg::Literal(Val::Str(arg))));
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
						new.cmds.push(Cmd::NamedField(name,CmdArg::Literal(Val::Str(arg))));
					} else {
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::Field(CmdArg::Literal(Val::Str(arg))));
					}
				}
				"-v" | "--not-global" |
				"-g" | "--global" => {
					let global = Self::handle_global_arg(arg.as_str(), &mut args);
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
	fn handle_global_arg(arg: &str, args: &mut Peekable<Skip<impl Iterator<Item = String>>>) -> Cmd {
		let polarity = match arg {
			"-v" | "--not-global" => false,
			"-g" | "--global" => true,
			_ => unreachable!("found arg: {arg}")
		};
		let mut then_cmds = vec![];
		let mut else_cmds = None;
		let Some(arg) = args.next() else {
			return Cmd::Global {
				pattern: CmdArg::Literal(Val::Str(arg.into())),
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
				"-n" | "--next" => then_cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or_else(complain_and_exit);
					let repeat_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or_else(complain_and_exit);

					if let Some(else_cmds) = else_cmds.as_mut() {
						let mut body = vec![];
						for _ in 0..cmd_count {
							let Some(cmd) = else_cmds.pop() else { break };
							body.push(cmd);
						}
						else_cmds.push(Cmd::Repeat{ body, count: CmdArg::Count(repeat_count) });
					} else {
						let mut body = vec![];
						for _ in 0..cmd_count {
							let Some(cmd) = then_cmds.pop() else { break };
							body.push(cmd);
						}
						then_cmds.push(Cmd::Repeat{ body, count: CmdArg::Count(repeat_count) });
					}
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						eprintln!("Expected a motion command after '-m', found {arg}");
						std::process::exit(1);
					}
					if let Some(else_cmds) = else_cmds.as_mut() {
						else_cmds.push(Cmd::Motion(CmdArg::Literal(Val::Str(arg))));
					} else {
						then_cmds.push(Cmd::Motion(CmdArg::Literal(Val::Str(arg))));
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
							cmds.push(Cmd::NamedField(name,CmdArg::Literal(Val::Str(arg))));
						} else {
							then_cmds.push(Cmd::NamedField(name,CmdArg::Literal(Val::Str(arg))));
						}
					} else {
						if arg.starts_with('-') {
							eprintln!("Expected a selection command after '-c', found {arg}");
							std::process::exit(1);
						}
						if let Some(cmds) = else_cmds.as_mut() {
							cmds.push(Cmd::Field(CmdArg::Literal(Val::Str(arg))));
						} else {
							then_cmds.push(Cmd::Field(CmdArg::Literal(Val::Str(arg))));
						}
					}
				}
				"-g" | "--global" |
				"-v" | "--not-global" => {
					let nested = Self::handle_global_arg(&global_arg, args);
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
						pattern: CmdArg::Literal(Val::Str(arg)),
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
			pattern: CmdArg::Literal(Val::Str(arg)),
			then_cmds,
			else_cmds,
			polarity
		}
	}
	pub fn from_script(script: PathBuf) -> Result<Self,String> {
		let script_content = fs::read_to_string(&script)
			.map_err(|_| format!("vicut: failed to read script file '{}'",script.display()))?;
		vic::parse_vic(&script_content)
			.map_err(|e| format!("vicut: failed to parse script file '{}': {e}",script.display()))
	}
	pub fn from_raw(script: &str) -> Result<Self,String> {
		vic::parse_vic(script)
			.map_err(|e| format!("vicut: failed to parse script: {e}"))
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
	fn handle_filename(&mut self, filename: String) {
		if let Err(e) = Self::validate_filename(&filename) {
			eprintln!("{e}");
			std::process::exit(1);
		}
		let path = PathBuf::from(filename.trim().to_string());
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
fn format_output(args: &Opts, lines: Vec<Vec<(String,String)>>) -> String {
	if args.json {
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
fn execute(args: &Opts, input: String, filename: Option<PathBuf>) -> Result<Vec<Vec<(String,String)>>,String> {
	let fields: Vec<(String,String)> = vec![];
	let fmt_lines: Vec<Vec<(String,String)>> = vec![];

	let mut vicut = ViCut::new(input, 0)?;
	let basename = filename.clone()
		.map(|s| s.file_name().unwrap_or_default().to_string_lossy().to_string())
		.unwrap_or_else(|| String::from("stdin"));
	let filepath = filename.map(|s| s.to_string_lossy().to_string()).unwrap_or(String::from("stdin"));
	vicut.set_var("filename".into(), Val::Str(basename))?;
	vicut.set_var("filepath".into(), Val::Str(filepath))?;


	let field_num = 0;
	let mut ctx = ExecCtx {
		args: args.clone(),
		field_num,
		fields,
		fmt_lines
	};
	for cmd in &args.cmds {
		exec_cmd(
			cmd,
			&mut vicut,
			&mut ctx
		);
		if !ctx.args.keep_mode {
			vicut.set_normal_mode();
		}
	}

	if !ctx.fields.is_empty() {
		ctx.fmt_lines.push(std::mem::take(&mut ctx.fields));
	}

	if ctx.fmt_lines.is_empty() && args.silent {
		return Ok(vec![]);
	}

	// Let's figure out if we want to print the whole buffer
	let no_fields = ctx.fmt_lines.is_empty(); // No fields were extracted
	let has_files = !ctx.args.files.is_empty(); // We have files to edit
	let has_pattern_search = ctx.args.cmds.iter().any(|cmd| {
		if let Cmd::Global { then_cmds, .. } = cmd {
			then_cmds.iter().any(|cmd| matches!(cmd, Cmd::Field(_) | Cmd::NamedField(_, _)))
		} else {
			false
		}
	});
	let editing_inplace = args.edit_inplace; // We are not editing in place

	// If we have not extracted any fields, and the following conditions are true:
	// * We have files without editing in place, or
	// * We don't have any files, order
	// * We have a pattern search with at least one field extraction
	//
	// then we print the entire buffer
	let should_print_entire_buffer = (!has_pattern_search && (!editing_inplace || !has_files)) && no_fields;

	if should_print_entire_buffer {
		let big_line = vicut.current_buffer().buffer.clone();
		ctx.fmt_lines.push(vec![("0".into(),big_line)]);
	}

	if ctx.args.trim_fields {
		trim_fields(&mut ctx.fmt_lines);
	}

	Ok(ctx.fmt_lines)
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
	vicut: &mut ViCut,
	ctx: &mut ExecCtx,
) -> Option<Val>{
	match cmd {
		Cmd::SwitchBuf(id) => {
			let Val::Num(id) = vicut.eval_cmd_arg(id,ctx).unwrap_or_else(complain_and_exit) else {
				eprintln!("vicut: expected a number for buffer ID, found {id}");
				std::process::exit(1);
			};
			vicut.editor.set(id as usize);
		}
		Cmd::GetBufId => {
			// Get the current buffer's ID
			let buf_id = vicut.editor.get();
			return Some(Val::Num(buf_id as isize));
		}
		Cmd::Push(stack_var, arg) => {
			let stack_var = match stack_var {
				CmdArg::Null => return None,
				CmdArg::Literal(val) => val.to_string(),
				CmdArg::Var(var) => var.to_string(),
				CmdArg::Count(_) => return None,
				CmdArg::Expr(expr) => vicut.eval_expr(expr, ctx).unwrap_or_else(complain_and_exit).to_string(),
			};
			let value = vicut.eval_cmd_arg(arg, ctx).unwrap_or_else(complain_and_exit).clone();
			if stack_var == "buffers" {
				// the 'buffers' variable is a built-in which holds all of the currently open buffers
				// so now we push the given data onto it as a new LineBuf
				vicut.push_buffer(value);
				return None
			}
			let stack = vicut.get_var_mut(&stack_var)
				.ok_or_else(|| format!("vicut: variable '{stack_var}' not found"))
				.unwrap_or_else(complain_and_exit);
			let Ok(iterable) = CompoundVal::try_from(stack.clone()) else {
				eprintln!("vicut: expected an array or string for variable '{stack_var}', found {value}");
				std::process::exit(1);
			};
			let new_val = match iterable {
				CompoundVal::Str(mut str) => {
					str.push_str(&value.to_string());
					Val::Str(str)
				}
				CompoundVal::Arr(mut vals) => {
					vals.push(value);
					Val::Arr(vals)
				}
			};
			*stack = new_val.clone();
		}
		Cmd::Pop(stack_var) => {
			let stack_var = match stack_var {
				CmdArg::Null => return None,
				CmdArg::Literal(val) => val.to_string(),
				CmdArg::Var(var) => var.to_string(),
				CmdArg::Count(_) => return None,
				CmdArg::Expr(expr) => vicut.eval_expr(expr, ctx).unwrap_or_else(complain_and_exit).to_string(),
			};
			if stack_var == "buffers" {
				// the 'buffers' variable is a built-in which holds all of the currently open buffers
				// so now we pop the last buffer off of it
				// we are in a command context, so we can ignore the return value
				vicut.pop_buffer();
				return None
			}
			let Some(stack_val) = vicut.get_var_mut(&stack_var) else {
				eprintln!("vicut: variable '{stack_var}' not found");
				std::process::exit(1);
			};
			let iterable = CompoundVal::try_from(stack_val.clone()).unwrap_or_else(|_| {
				eprintln!("vicut: expected a list or map for variable '{stack_var}', found {stack_val}");
				std::process::exit(1);
			});
			let popped_value = match iterable {
				CompoundVal::Str(mut str) => {
					let last_char = str.pop()?;
					*stack_val = Val::Str(str);
					Val::Str(last_char.to_string())
				}
				CompoundVal::Arr(mut vals) => {
					let popped_value = vals.pop()?;
					*stack_val = Val::Arr(vals);
					popped_value
				}
			};
			return Some(popped_value)
		}
		Cmd::LoopBreak |
		Cmd::LoopContinue => {
			// These are only checked for in loop contexts
			// We can just return
			return None
		}
		Cmd::Yank(arg,reg) => {
			// Evaluate the arg and yank it into the given register
			let value = vicut.eval_cmd_arg(arg, ctx).unwrap_or_else(complain_and_exit);

			// Uppercase register name means "append to the register"
			if reg.is_ascii_uppercase() {
				append_register(Some(*reg), RegisterContent::Span(value.to_string()));
			} else {
				write_register(Some(*reg), RegisterContent::Span(value.to_string()));
			}
		}
		Cmd::Return(arg) => {
			// Evaluate the argument and return it
			// This is the only branch that returns a value
			let value = vicut.eval_cmd_arg(arg, ctx).unwrap_or_else(complain_and_exit);
			return Some(value)
		}
		Cmd::FuncDef { name, args, body } => {
			// Define a function
			vicut.set_function(name.clone(), args.clone(), body.clone());
		}
		Cmd::FuncCall { name, args: call_args } => {
			let func_args = call_args
				.iter()
				.map(|arg| vicut.eval_cmd_arg(arg, ctx).unwrap_or_else(complain_and_exit))
				.collect::<Vec<_>>();
			vicut.eval_function(name.to_string(), func_args, ctx).unwrap_or_else(complain_and_exit);
		}
		Cmd::Echo(args) => {
			if args.is_empty() {
				println!();
				return None
			}
			let mut display_args = vec![];
			for arg in args {
				let value = vicut.eval_cmd_arg(arg,ctx).unwrap_or_else(complain_and_exit);

				display_args.push(value.to_string());
			}
			let output = display_args.join(" ");
			println!("{output}");
		}
		// -r <N> <R>
		Cmd::Repeat{ body, count } => {
			let n_repeats = vicut.eval_count(count).unwrap_or_else(complain_and_exit);
			vicut.descend(); // new scope
			for _ in 0..n_repeats {

				for r_cmd in body {
					// We use recursion so that we can nest repeats easily
					exec_cmd(
						r_cmd,
						vicut,
						ctx
					);
				}
				if !ctx.args.keep_mode {
					vicut.set_normal_mode();
				}
			}
			vicut.ascend(); // leave scope
		}
		// -g/-v <PATTERN> <COMMANDS> [--else <COMMANDS>]
		Cmd::Global { pattern, then_cmds, else_cmds, polarity } => {
			let pattern = match pattern {
				CmdArg::Literal(pattern) => pattern.clone(),
				CmdArg::Var(var) => {
					let Some(val) = vicut.get_var(var) else {
						eprintln!("vicut: variable '{var}' not found");
						std::process::exit(1)
					};
					val.clone()
				}
				CmdArg::Expr(exp) => {
					vicut.eval_expr(exp, ctx).unwrap_or_else(complain_and_exit)
				}
				_ => unreachable!()
			};
			let motion = match polarity {
				false  => Motion::NotGlobal(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern),
				true => Motion::Global(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern)
			};

			// Here we ask ViCut's editor directly to evaluate the Global motion for us.
			// LineBuf::eval_motion() *always* returns MotionKind::Lines() for Motion::Global/NotGlobal.
			let MotionKind::Lines(lines) = vicut.current_buffer().eval_motion(None, MotionCmd(1,motion)) else { unreachable!() };
			if !lines.is_empty() {
				// Positive branch
				for line in lines {
					let mut line_no = line;
					let field_num = if ctx.args.global_uses_line_numbers {
						// If we are using line numbers, we need to set the field number to the line number
						&mut line_no
					} else {
						&mut ctx.field_num.clone()
					};
					let Some((start,_)) = vicut.current_buffer().line_bounds(line) else { continue };
					// Set the cursor on the start of the line
					vicut.current_buffer().cursor.set(start);
					// Execute our commands

					vicut.descend(); // new scope
					for cmd in then_cmds {
						exec_cmd(
							cmd,
							vicut,
							ctx
						);
						if !ctx.args.keep_mode {
							vicut.set_normal_mode();
						}
					}
					vicut.ascend(); // leave scope
				}
			} else if let Some(else_cmds) = else_cmds {
				// Negative branch
				vicut.descend();
				for cmd in else_cmds {
					exec_cmd(
						cmd,
						vicut,
						ctx,
					);
					if !ctx.args.keep_mode {
						vicut.set_normal_mode();
					}
				}
				vicut.ascend();
			}
		}
		// -m <VIM_CMDS>
		Cmd::Motion(motion) => {
			let motion = vicut.eval_cmd_arg(motion,ctx).unwrap_or_else(complain_and_exit).to_string();
			if let Err(e) = vicut.move_cursor(&motion) {
				eprintln!("vicut: {e}");
			}
		}
		// -c <VIM_CMDS>
		Cmd::Field(motion) => {
			let motion = vicut.eval_cmd_arg(motion,ctx).unwrap_or_else(complain_and_exit).to_string();
			ctx.field_num += 1;
			match vicut.read_field(&motion) {
				Ok(field) => {
					let name = format!("{}",ctx.field_num);
					ctx.fields.push((name,field))
				}
				Err(e) => {
					eprintln!("vicut: {e}");
				}
			}
		}
		// -c name=<NAME> <VIM_CMDS>
		Cmd::NamedField(name, motion) => {
			trace!("Executing capturing command with name '{name}': {motion}");
			let motion = vicut.eval_cmd_arg(motion,ctx).unwrap_or_else(complain_and_exit).to_string();
			ctx.field_num += 1;
			match vicut.read_field(&motion) {
				Ok(field) => ctx.fields.push((name.clone(),field)),
				Err(e) => {
					eprintln!("vicut: {e}");
				}
			}
		}
		// -n
		Cmd::BreakGroup => {
			if ctx.args.trace {
				trace!("Breaking field group with fields: ");
				for field in &mut ctx.fields {
					let name = &field.0;
					let content = &field.1;
					trace!("\t{name}: {content}");
				}
			}
			ctx.field_num = 0;
			if !ctx.fields.is_empty() {
				ctx.fmt_lines.push(std::mem::take(&mut ctx.fields));
			}
		}
		Cmd::VarDec { name, value } => {
			let value = vicut.eval_cmd_arg(value,ctx).unwrap_or_else(complain_and_exit);
			vicut.set_var(name.clone(), value.clone()).unwrap_or_else(complain_and_exit);
		}
		Cmd::MutateVar { name, index, op, value } => {
			let value = vicut.eval_cmd_arg(value,ctx).unwrap_or_else(complain_and_exit);
			if let Some(index) = index {
				let index = vicut.eval_cmd_arg(index,ctx).unwrap_or_else(complain_and_exit);
				let Val::Num(index) =  index else {
					eprintln!("vicut: expected number for index");
					std::process::exit(1);
				};
				let index = index as usize;
				vicut.set_index_var(name.to_string(), index, value);
			} else {
				vicut.mutate_var(name.clone(), op.clone(), value.clone()).unwrap_or_else(complain_and_exit);
			}
		}
		Cmd::IfBlock { cond_blocks, else_block } => {
			let mut executed = false;
			for block in cond_blocks {
				let CondBlock { cond, cmds } = block;
				let result = cond.is_truthy(vicut,ctx);
				if result {
					executed = true;
					vicut.descend(); // new scope
					for cmd in cmds {
						exec_cmd(
							cmd,
							vicut,
							ctx
						);
						if !ctx.args.keep_mode {
							vicut.set_normal_mode();
						}
					}
					vicut.ascend(); // leave scope
					break;
				}
			}

			if let Some(else_block) = else_block {
				if !executed {
					vicut.descend(); // new scope
					for cmd in else_block {
						exec_cmd(
							cmd,
							vicut,
							ctx
						);
						if !ctx.args.keep_mode {
							vicut.set_normal_mode();
						}
					}
					vicut.ascend(); // leave scope
				}
			}
		}
		Cmd::ForBlock { var_name, iterable, body } => {
			let val = vicut.eval_cmd_arg(iterable,ctx).unwrap_or_else(complain_and_exit);
			let val_iter = CompoundVal::try_from(val).unwrap_or_else(complain_and_exit);
			let iter = val_iter.into_iter().collect::<Vec<_>>();
			if iter.is_empty() {
				return None;
			}
			'main: for item in iter {
				if cmd == &Cmd::LoopBreak {
					break;
				}
				if cmd == &Cmd::LoopContinue {
					continue;
				}
				vicut.descend(); // new scope
				vicut.set_var(var_name.clone(), item).unwrap_or_else(complain_and_exit);
				for cmd in body {
					if cmd == &Cmd::LoopBreak {
						break 'main;
					}
					if cmd == &Cmd::LoopContinue {
						continue 'main;
					}
					exec_cmd(
						cmd,
						vicut,
						ctx
					);
					if !ctx.args.keep_mode {
						vicut.set_normal_mode();
					}
				}
				vicut.ascend(); // leave scope
			}
		}
		Cmd::WhileBlock(cond_block) => {
			let CondBlock { cond, cmds } = cond_block;
			'main: while cond.is_truthy(vicut,ctx) {
				vicut.descend(); // new scope
				for cmd in cmds {
					if cmd == &Cmd::LoopBreak {
						break 'main;
					}
					if cmd == &Cmd::LoopContinue {
						continue 'main;
					}
					exec_cmd(
						cmd,
						vicut,
						ctx
					);
					if !ctx.args.keep_mode {
						vicut.set_normal_mode();
					}
				}
				vicut.ascend(); // leave scope
			}
		}
		Cmd::UntilBlock(cond_block) => {
			let CondBlock { cond, cmds } = cond_block;
			'main: while !cond.is_truthy(vicut,ctx) {
				vicut.descend(); // new scope
				for cmd in cmds {
					if cmd == &Cmd::LoopBreak {
						break 'main;
					}
					if cmd == &Cmd::LoopContinue {
						continue 'main;
					}
					exec_cmd(
						cmd,
						vicut,
						ctx
					);
					if !ctx.args.keep_mode {
						vicut.set_normal_mode();
					}
				}
				vicut.ascend(); // leave scope
			}
		}
	}
	None
}

/// Multi-thread the execution of file input.
///
/// The steps this function walks through are as follows:
/// 1. Create a `work` vector containing a tuple of the file's path, and it's contents.
/// 2. Call `execute()` on each file's contents
/// 3. Decide how to handle output depending on whether args.edit_inplace is set.
fn execute_multi_thread_files(mut stdout: io::StdoutLock, args: &Opts) {
	let work: Vec<(PathBuf, String)> = args.files.par_iter()
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
			let processed = match execute(args, content, Some(path.clone())) {
				Ok(content) => content,
				Err(e) => {
					eprintln!("vicut: error in file '{}': {e}",path.display());
					std::process::exit(1)
				}
			};
			(path, processed)
		}).collect::<Vec<_>>();

	// Write back to file
	if args.json  && args.files.len() > 1 {
		let json = format_output_json_files(results);
		write!(stdout, "{json}").ok();
		return
	}
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

				fs::copy(&path, &backup_path).unwrap_or_else(complain_and_exit);
			}
			fs::write(&path, output).unwrap_or_else(complain_and_exit);
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
fn execute_multi_thread_files_linewise(mut stdout: io::StdoutLock, args: &Opts) {

	let work: Vec<(PathBuf, usize, String)> = args.files.par_iter()
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
			let processed = match execute(args, line, Some(path.clone())) {
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
	if args.json  && args.files.len() > 1 {
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

		if args.edit_inplace {
			if args.backup_files {
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
fn execute_linewise(mut stream: Box<dyn BufRead>, args: &Opts) -> String {
	let mut input = String::new();
	stream.read_to_string(&mut input).unwrap_or_else(complain_and_exit);
	let lines = get_lines(&input);
	// Pair each line with its original index
	let mut lines: Vec<_> = lines
		.into_par_iter()
		.enumerate()
		.map(|(i, line)| {
			let output = match execute(args, line, None) {
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
fn exec_linewise(args: &Opts) {
	if args.single_thread {
		let mut stdout = io::stdout().lock();

		// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
		// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
		let mut lines = vec![];
		let mut json_data = vec![];
		if !args.files.is_empty() {
			for path in &args.files {
				let input = fs::read_to_string(path).unwrap_or_else(complain_and_exit);
				for line in get_lines(&input) {
					match execute(args,line, Some(path.clone())) {
						Ok(mut new_line) => {
							lines.append(&mut new_line);
						}
						Err(e) => {
							eprintln!("vicut: {e}");
							return;
						}
					}
				}
				if args.json {
					json_data.push((path.clone(), std::mem::take(&mut lines)));
					continue
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

						fs::copy(path, &backup_path).unwrap_or_else(complain_and_exit);
					}
					fs::write(path, std::mem::take(&mut output)).unwrap_or_else(complain_and_exit);
				} else {
					if args.files.len() > 1 {
						writeln!(stdout,"--- {}", path.display()).ok();
					}
					writeln!(stdout, "{output}").ok();
				}
			}
			if !args.json {
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
				match execute(args,line, None) {
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
			.unwrap_or_else(complain_and_exit);
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
fn exec_files(args: &Opts) {
	let mut json_data = vec![];
	if args.single_thread {
		let mut stdout = io::stdout().lock();
		for path in &args.files {
			let content = fs::read_to_string(path).unwrap_or_else(complain_and_exit);
			match execute(args,content, Some(path.clone())) {
				Ok(output) => {
					if args.json {
						json_data.push((path.clone(), output));
						continue
					}
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

							fs::copy(path, &backup_path).unwrap_or_else(complain_and_exit);
						}
						fs::write(path, std::mem::take(&mut output)).unwrap_or_else(complain_and_exit);
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
		if args.json {
			let json = format_output_json_files(json_data);
			write!(stdout, "{json}").ok();
		}
	} else if let Some(num) = args.max_jobs {
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
fn exec_stdin(args: &Opts) {
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
	match execute(args,input, None) {
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
	let opts = if Opts::validate_filename(&maybe_script).is_err() {
		// It's not a file...
		// Let's see if it's a valid in-line script
		Opts::from_raw(&maybe_script).unwrap_or_else(complain_and_exit)
	} else {
		// It's a file, let's see if it's a valid script
		let script_path = PathBuf::from(maybe_script);
		Opts::from_script(script_path).unwrap_or_else(complain_and_exit)
	};


	init_logger(opts.trace);

	if opts.no_input {
		let output = execute(&opts, String::new(), None).unwrap_or_else(complain_and_exit);
		let mut stdout = io::stdout().lock();
		let output = format_output(&opts, output);
		write!(stdout, "{output}").ok();
	} else if opts.linewise {
		exec_linewise(&opts);
	} else if !opts.files.is_empty() {
		exec_files(&opts);
	} else {
		exec_stdin(&opts);
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

	let mut args = std::env::args();
	args.find(|arg| arg == "--script"); // let's find the --script flag
	let script = args.next(); // If we found it, the next arg is the script name

	let opts = if let Some(script) = script {
		let script = PathBuf::from(script);
		Opts::from_script(script).unwrap_or_else(complain_and_exit)
	} else {
		// Let's see if we got a literal in-line script instead then
		let mut flags = std::env::args().take_while(|arg| arg != "--");
		let use_inline = flags.all(|arg| !arg.starts_with('-'));

		if use_inline {
			// We know that there's at least one argument, so we can safely unwrap
			let mut args = std::env::args().skip(1);
			let maybe_script = args.next().unwrap();
			let mut opts = if Opts::validate_filename(&maybe_script).is_err() {
				// It's not a file...
				// Let's see if it's a valid in-line script
				Opts::from_raw(&maybe_script).unwrap_or_else(complain_and_exit)
			} else {
				// It's a file, let's see if it's a valid script
				let script_path = PathBuf::from(maybe_script);
				Opts::from_script(script_path).unwrap_or_else(complain_and_exit)
			};
			// Now let's grab the file names
			for arg in args {
				if let Err(e) = Opts::validate_filename(&arg) {
					eprintln!("vicut: {e}");
					std::process::exit(1);
				}
				opts.files.push(PathBuf::from(arg));
			}
			opts
		} else {
			// We're using command line arguments
			// boo
			Opts::parse().unwrap_or_else(complain_and_exit)
		}
	};

	init_logger(opts.trace);

	if opts.no_input {
		let output = execute(&opts, String::new(), None).unwrap_or_else(complain_and_exit);
		let mut stdout = io::stdout().lock();
		let output = format_output(&opts, output);
		write!(stdout, "{output}").ok();
	} else if opts.linewise {
		exec_linewise(&opts);
	} else if !opts.files.is_empty() {
		exec_files(&opts);
	} else {
		exec_stdin(&opts);
	}
}
