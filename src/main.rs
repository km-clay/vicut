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
use std::{collections::BTreeMap, env::Args, fmt::{Display, Write}, fs, io::{self, BufRead, Write as IoWrite}, iter::{Peekable, Skip}, path::{Path, PathBuf}};

extern crate tikv_jemallocator;

#[cfg(target_os = "linux")]
#[global_allocator]
/// For linux we use Jemalloc. It is ***significantly*** faster than the default allocator in this case, for some reason.
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use unicode_segmentation::UnicodeSegmentation;
use vic::parse::Val;
use exec::ViCut;
use log::trace;
use register::{append_register, write_register, RegisterContent};
use serde_json::{Map, Value};
use rayon::prelude::*;

use crate::{linebuf::MotionKind, vic::parse::{expr_error, Command, Expr, ExprKind, RcSpan}, vicmd::{LineAddr, Motion, MotionCmd}};

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

pub fn blame_span(span: RcSpan, err: impl Display) -> ! {
	let mut err = err.to_string();
	if !err.starts_with("vicut: ") {
		err = format!("vicut: {err}");
	}
	let output = expr_error(err, span.clone());
	eprintln!("{output}");
	std::process::exit(1)
}

#[derive(Clone, Debug, Default)]
pub struct ExecCtx {
	field_num: usize,
	fields: Vec<(String,String)>, // (name, value)
	fmt_lines: Vec<Vec<(String,String)>>, // Lines to format output from
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
    pub vic: Option<String>,
    pub files: Option<Vec<PathBuf>>,
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
	for cmd in &args.vic {
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
	let editing_inplace = args.edit_inplace; // We are not editing in place

	// If we have not extracted any fields, and the following conditions are true:
	// * We have files without editing in place, or
	// * We don't have any files, order
	// * We have a pattern search with at least one field extraction
	//
	// then we print the entire buffer
	let should_print_entire_buffer = (!editing_inplace || !has_files) && no_fields;

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
	cmd: &Expr,
	vicut: &mut ViCut,
	ctx: &mut ExecCtx,
) -> Option<Val>{
	let Expr { value: cmd, index, span } = cmd;
	match cmd {
    ExprKind::Command(Command::ShellCmd { cmd }) => {
			// Evaluate the shell command and execute it
			let _ = vicut.eval_cmd_arg(cmd, ctx).unwrap_or_else(complain_and_exit);
		}
    ExprKind::Command(Command::BufSwitch { id }) => {
			let Val::Num(id) = vicut.eval_cmd_arg(id,ctx).unwrap_or_else(|err| blame_span(*span, err)) else {
				blame_span(*span, "vicut: expected a number for buffer ID")
			}; 
			vicut.editor.set(id as usize);
		}
		ExprKind::Command(Command::Include { path }) => {
			todo!()
		}
    ExprKind::Command(Command::BufId) => {
			// Get the current buffer's ID
			let buf_id = vicut.editor.get();
			return Some(Val::Num(buf_id as isize));
		}
    ExprKind::Command(Command::Push { stack, value }) => {
			let stack_var = vicut.eval_expr(stack).unwrap_or_else(|| blame_span(span,err));
			let value = vicut.eval_cmd_arg(value, ctx).unwrap_or_else(complain_and_exit).clone();
			if &stack_var.to_string() == "buffers" {
				// the 'buffers' variable is a built-in which holds all of the currently open buffers
				// so now we push the given data onto it as a new LineBuf
				vicut.push_buffer(value);
				return None
			}

			let stack = vicut.get_var_mut(&stack_var.to_string())
				.ok_or_else(|| format!("vicut: variable '{stack_var}' not found"))
				.unwrap_or_else(complain_and_exit);
			match stack {
				Val::Str(str) => {
					str.push_str(&value.to_string());
				}
				Val::Arr(arr) => {
					arr.push(value);
				}
				_ => blame_span(span, format!("vicut: expected a list or string for variable '{stack_var}', found {stack}"))
			}
		}
    ExprKind::Command(Command::Pop { stack }) => {
			let stack_var = vicut.eval_expr(stack, ctx).unwrap_or_else(|err| blame_span(span,err)).to_string();
			if &stack_var == "buffers" {
				// the 'buffers' variable is a built-in which holds all of the currently open buffers
				// so now we pop the last buffer off of it
				// we are in a command context, so we can ignore the return value
				vicut.pop_buffer();
				return None
			}
			let Some(stack_val) = vicut.get_var_mut(&stack_var) else {
				blame_span(span, format!("vicut: variable '{stack_var}' not found"))
			};

			let popped_value = match stack_val {
				Val::Str(str) => {
					let mut graphemes = str.graphemes(true);
					let popped = graphemes.next_back();
					*str = graphemes.collect::<String>();
					popped.map(|gr| Val::Str(gr.into()))
				}
				Val::Arr(arr) => {
					arr.pop()
				}
				_ => blame_span(span, format!("vicut: expected a list or string for variable '{stack_var}', found {stack_val}"))
			};
			return popped_value
		}
    ExprKind::Command(Command::Break) |
		ExprKind::Command(Command::Continue) => {
			// These are only checked for in loop contexts
			// We can just return
			return None
		}
    ExprKind::Command(Command::Yank { register, motion }) => {
			// Evaluate the arg and yank it into the given register
			let reg = vicut.eval_expr(register, ctx).unwrap_or_else(|err| blame_span(span, err)).to_string()
				.chars().next().unwrap_or_else(|| blame_span(span, format!("vicut: expected a register name, found empty string")));
			let motion = vicut.eval_expr(motion, ctx).unwrap_or_else(|err| blame_span(span, err)).to_string();

			let value = vicut.read_field(&motion).unwrap_or_else(|err| blame_span(span, err));

			// Uppercase register name means "append to the register"
			if reg.is_ascii_uppercase() {
				append_register(Some(reg), RegisterContent::Span(value.to_string()));
			} else {
				write_register(Some(reg), RegisterContent::Span(value.to_string()));
			}
		}
    ExprKind::Command(Command::Return { ret }) => {
			let Some(ret) = ret else {
				return Some(Val::Null)
			};
			// Evaluate the argument and return it
			// This is the only branch that returns a value
			let value = vicut.eval_expr(ret, ctx).unwrap_or_else(|err| blame_span(span, err));
			return Some(value)
		}
    ExprKind::Command(Command::Echo { args }) => {
			if args.is_empty() {
				println!();
				return None
			}
			let mut display_args = vec![];
			for arg in args {
				let value = vicut.eval_expr(arg, ctx).unwrap_or_else(|err| blame_span(span, err));

				display_args.push(value.to_string());
			}
			let output = display_args.join(" ");
			println!("{output}");
		}
    ExprKind::Command(Command::Repeat { count, block }) => {
			let n_repeats = vicut.eval_count(count).unwrap_or_else(complain_and_exit);
			vicut.descend(); // new scope
			for _ in 0..n_repeats {

				for r_cmd in block {
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
    ExprKind::Command(Command::NotGlobal { pattern, block }) |
		ExprKind::Command(Command::Global { pattern, block }) => {
			let polarity = matches!(cmd, ExprKind::Command(Command::Global { .. }));
			let pattern = vicut.eval_expr(pattern, ctx).unwrap_or_else(|err| blame_span(span, err)).to_string();
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
					for cmd in block {
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
    ExprKind::Command(Command::Move { motion }) => {
			let motion = vicut.eval_expr(motion, ctx).unwrap_or_else(|err| blame_span(span, err)).to_string();
			if let Err(e) = vicut.move_cursor(&motion) {
				blame_span(span, e);
			}
		}
    ExprKind::Command(Command::Cut { motion }) => {
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
    ExprKind::Command(Command::Next) => {
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
    ExprKind::FuncDef { name, params, body } => {
			// Define a function
			vicut.set_function(name.clone(), params.clone(), body.clone());
		}
    ExprKind::FuncCall { name, args } => {
			// Func calls use evaluated names, so that stuff like func_ptr_array[0](arg1,arg2) is valid
			let name = vicut.eval_expr(name, ctx).unwrap_or_else(|err| blame_span(span, err)).to_string();
			let func_args = args
				.iter()
				.map(|arg| vicut.eval_expr(arg, ctx).unwrap_or_else(|err| blame_span(span, err)))
				.collect::<Vec<_>>();
			vicut.eval_function(name, func_args, ctx).unwrap_or_else(complain_and_exit);
		}
    ExprKind::VarDec { name, value } => {
			let value = vicut.eval_expr(name, ctx).unwrap_or_else(|err| blame_span(span, err));
			vicut.set_var(name.clone(), value.clone()).unwrap_or_else(|err| blame_span(span, err));
		}
    ExprKind::VarMut { name, op, value } => {
			let value = vicut.eval_expr(name, ctx).unwrap_or_else(|err| blame_span(span, err));
			if let Some(index) = index {
				let index = vicut.eval_expr(index,ctx).unwrap_or_else(complain_and_exit);
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
    ExprKind::IfBlock { cond_blocks, else_block } => {
			let mut executed = false;
			for block in cond_blocks {
				let Expr { value, .. } = block;
				let ExprKind::CondBlock { cond, body } = value else { unreachable!() };
				let cond_value = vicut.eval_expr(cond, ctx).unwrap_or_else(|err| blame_span(span, err));
				let result = cond_value.is_truthy(vicut);
				if result {
					executed = true;
					vicut.descend(); // new scope
					for cmd in body {
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
    ExprKind::ForBlock { var_name, list, body } => {
			let val = vicut.eval_expr(var_name,ctx).unwrap_or_else(|err| blame_span(span, err));
			let val_iter = val.try_into_iter().unwrap_or_else(|err| blame_span(span, err));

			'main: for item in val_iter {
				vicut.descend(); // new scope
				vicut.set_var(var_name.clone(), item).unwrap_or_else(complain_and_exit);
				for cmd in body {

					if cmd.is_break() {
						break 'main;
					}
					if cmd.is_continue() {
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
    ExprKind::UntilBlock { cond, body } |
		ExprKind::WhileBlock { cond, body } => {
			// This is the function we will use to see if we are still running
			let running = |vicut: &mut ViCut, ctx: &mut ExecCtx<'_>| {
				let result = vicut.eval_expr(cond, ctx).unwrap_or_else(|err| blame_span(span, err)).is_truthy(vicut); 
				if matches!(cmd, ExprKind::WhileBlock { .. }) {
					result
				} else {
					!result
				}
			};

			while running(vicut,ctx) {
				vicut.descend(); // new scope
				for cmd in body {
					if cmd.is_break() {
						break;
					}
					if cmd.is_continue() {
						continue;
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
    ExprKind::Vic(exprs) => todo!(),
    ExprKind::TopLevel(expr) => todo!(),
    ExprKind::Block(exprs) => todo!(),
    ExprKind::Value(val) => todo!(),
    ExprKind::Opts(exprs) => todo!(),
    ExprKind::Opt { name, arg } => todo!(),
    ExprKind::CondBlock { cond, body } => todo!(),
    ExprKind::Range { start, end } => todo!(),
    ExprKind::BinaryExpr { left, op, right } => todo!(),
    ExprKind::BoolExpr { left, op, right } => todo!(),
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
	let mut opts = Opts::default();
	let script_content = if Path::new(&maybe_script).is_file() {
		// It's a file?
		// Let's read it and see if it parses
		fs::read_to_string(&maybe_script).unwrap()
	} else {
		// It's a raw script as an argument
		// Let's just parse it
		maybe_script
	};
	opts.parse_vic(&script_content);


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
	let mut vicut = ViCut::empty();

	let script_src = if let Some(script) = script {
		let script = PathBuf::from(script);
		fs::read_to_string(script).unwrap_or_else(complain_and_exit)
	} else {
		// Let's see if we got a literal in-line script instead then
		let mut flags = std::env::args().take_while(|arg| arg != "--");
		let use_inline = flags.all(|arg| !arg.starts_with('-'));

		if use_inline {
			// We know that there's at least one argument, so we can safely unwrap
			let mut args = std::env::args().skip(1);
			let maybe_script = args.next().unwrap();
			let script_src = if Opts::validate_filename(&maybe_script).is_err() {
				// It's not a file...
				// Let's see if it's a valid in-line script
				maybe_script
			} else {
				// It's a file, let's see if it's a valid script
				let script_path = PathBuf::from(maybe_script);
				fs::read_to_string(script_path).unwrap_or_else(complain_and_exit)
			};
			// Now let's grab the file names
			for arg in args {
				if let Err(e) = validate_filename(&arg) {
					eprintln!("vicut: {e}");
					std::process::exit(1);
				}
				vicut.push_file(PathBuf::from(arg));
			}
			script_src
		} else {
			// We're using command line arguments
			// boo
			complain_and_exit("Did not find a vic script to parse")
		}
	};

	vicut.parse_vic(&script_src).unwrap_or_else(complain_and_exit);

	init_logger(vicut.find_opt(|opts| opts.trace).unwrap_or_default());

	if vicut.find_opt(|opts| opts.no_input).unwrap_or_default() {
		let output = execute(&opts, String::new(), None).unwrap_or_else(complain_and_exit);
		let mut stdout = io::stdout().lock();
		let output = format_output(&opts, output);
		write!(stdout, "{output}").ok();
	} else if vicut.find_opt(|o| o.linewise).unwrap_or_default() {
		exec_linewise(&opts);
	} else if vicut.find_opt(|o| o.files).is_some_and(|f| !f.is_empty()) {
		exec_files(&opts);
	} else {
		exec_stdin(&opts);
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
