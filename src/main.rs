#![allow(clippy::unnecessary_to_owned,clippy::while_let_on_iterator)]
use std::{fmt::Write,io::{self, Write as IoWrite, BufRead}};


use exec::ViCut;
use log::trace;
use serde_json::{Map, Value};
use rayon::prelude::*;

pub mod vicmd;
pub mod modes;
pub mod exec;
pub mod linebuf;
pub mod keys;
pub mod register;
pub mod reader;
#[cfg(test)]
pub mod tests;

pub type Name = String;

#[derive(Clone,Debug)]
enum Cmd {
	Motion(String),
	Field(String),
	NamedField(Name,String),
	Repeat(usize,usize),
	BreakGroup
}

#[derive(Default,Clone,Debug)]
struct Argv {
	delimiter: Option<String>,
	template: Option<String>,
	max_jobs: Option<u32>,

	json: bool,
	trace: bool,
	linewise: bool,
	trim_fields: bool,
	keep_mode: bool,
	single_thread: bool,

	cmds: Vec<Cmd>
}

impl Argv {
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
				arg => { return Err(format!("Unrecognized argument '{arg}'")) }
			}
		}
		Ok(new)
	}
	pub fn parse() -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = std::env::args().skip(1);
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
				arg => { return Err(format!("Unrecognized argument '{arg}'")) }
			}
		}
		Ok(new)
	}
}

fn get_help() -> String {
	let mut help = String::new();
	writeln!(help).unwrap();
	writeln!(help, "\x1b[1mvicut\x1b[0m").unwrap();
	writeln!(help, "A text processor that uses Vim motions to slice and extract structured data from stdin.").unwrap();
	writeln!(help).unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\x1b[1;4mUSAGE:\x1b[0m").unwrap();
	writeln!(help, "\tvicut [OPTIONS] [COMMANDS]...").unwrap();
	writeln!(help).unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\x1b[1;4mOPTIONS:\x1b[0m").unwrap();
	writeln!(help, "\t-t, --template <STR>").unwrap();
	writeln!(help, "\t\tProvide a format template to use for custom output formats. Example:").unwrap();
	writeln!(help, "\t\t--template \"< {{{{1}}}} > ( {{{{2}}}} ) {{ {{{{3}}}} }}\"").unwrap();
	writeln!(help, "\t\tNames given to fields explicitly using '-c name=<name>' should be used instead of field numbers.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t-d, --delimiter <STR>").unwrap();
	writeln!(help, "\t\tProvide a delimiter to place between fields in the output. No effect when used with --json.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--keep-mode").unwrap();
	writeln!(help, "\t\tThe internal editor will not return to normal mode after each command.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--json").unwrap();
	writeln!(help, "\t\tOutput the result as structured JSON.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--linewise").unwrap();
	writeln!(help, "\t\tApply given commands to each line in the given input.").unwrap();
	writeln!(help, "\t\tEach line in the input is treated as it's own separate buffer.").unwrap();
	writeln!(help, "\t\tThis operation is multi-threaded.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--serial").unwrap();
	writeln!(help, "\t\tWhen used with --linewise, operates on each line sequentially instead of using multi-threading.").unwrap();
	writeln!(help, "\t\tNote that the order of lines is maintained regardless of whether or not multi-threading is used.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--jobs").unwrap();
	writeln!(help, "\t\tWhen used with --linewise, limits the number of threads that the program can use.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--trim-fields").unwrap();
	writeln!(help, "\t\tTrim leading and trailing whitespace from captured fields.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t--trace").unwrap();
	writeln!(help, "\t\tPrint debug trace of command execution").unwrap();
	writeln!(help).unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\x1b[1;4mCOMMANDS:\x1b[0m").unwrap();
	writeln!(help, "\t-c, --cut [name=<NAME>] <VIM_COMMAND>").unwrap();
	writeln!(help, "\t\tExecute a Vim command on the buffer, and capture the text between the cursor's start and end positions as a field.").unwrap();
	writeln!(help, "\t\tFields can be optionally given a name, which will be used as the key for that field in formatted JSON output.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t-m, --move <VIM_COMMAND>").unwrap();
	writeln!(help, "\t\tLogically identical to -c/--cut, except it does not capture a field.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t-r, --repeat <N> <R>").unwrap();
	writeln!(help, "\t\tRepeat the last N commands R times. Repeats can be nested.").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\t-n, --next").unwrap();
	writeln!(help, "\t\tStart a new field group. Each field group becomes one output record.").unwrap();
	writeln!(help).unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\x1b[1;4mNOTES:\x1b[0m").unwrap();
	writeln!(help, "\t* Commands are executed left to right.").unwrap();
	writeln!(help, "\t* Cursor state is maintained between commands, but the editor returns to normal mode between each command.").unwrap();
	writeln!(help, "\t* Commands are not limited to only motions. Commands which edit the buffer can be executed as well.").unwrap();
	writeln!(help).unwrap();
	writeln!(help).unwrap();
	writeln!(help, "\x1b[1;4mEXAMPLE:\x1b[0m").unwrap();
	writeln!(help, "\t$ echo 'foo bar (boo far) [bar foo]' | vicut --delimiter ' -- ' \\
\t-c 'e' -m 'w' -r 2 1 -c 'va)' -c 'va]'").unwrap();
	writeln!(help, "\toutputs:").unwrap();
	writeln!(help, "\tfoo -- bar -- (boo far) -- [bar foo]").unwrap();
	writeln!(help).unwrap();
	writeln!(help, "For more info, see: https://github.com/km-clay/vicut").unwrap();
	help
}

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

fn format_output_standard(delimiter: &str, lines: Vec<Vec<(String,String)>>) -> String {
	lines.into_iter()
		.fold(String::new(), |mut acc,line| {
			// Accumulate all line fields into one string,
			// Fold all lines into one string
			let fmt_line = line
				.into_iter()
				.map(|(_,f)| f) // Ignore the name here, if any
				.collect::<Vec<String>>()
				.join(delimiter);
			acc.push_str(&fmt_line);
			acc.push('\n');
			acc
		})
}

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
							write!(e,"Did not find a field called '{field_name}' for output template").unwrap();
							write!(e,"Captured field names were:").unwrap();
							for (name,_) in line {
								write!(e,"\t{name}").unwrap();
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
			output.push_str(&std::mem::take(&mut cur_line));
		}
		output.push('\n')
	}
	Ok(output)
}

fn execute(args: &Argv, input: String) -> Result<String,String> {
	let delimiter = args.delimiter.as_deref().unwrap_or("\t");
	let mut fields: Vec<(String,String)> = vec![];
	let mut fmt_lines: Vec<Vec<(String,String)>> = vec![];

	let mut vicut = ViCut::new(input, 0)?;

	let mut spent_cmds: Vec<&Cmd> = vec![];

	let mut field_num = 0;
	for cmd in &args.cmds {
		exec_cmd(
			cmd,
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

	if args.trim_fields {
		trim_fields(&mut fmt_lines);
	}

	if args.json {
		Ok(format_output_json(fmt_lines))
	} else if let Some(template) = args.template.as_deref() {
		format_output_template(template, fmt_lines)
	} else {
		Ok(format_output_standard(delimiter, fmt_lines))
	}
}

fn trim_fields(lines: &mut Vec<Vec<(String,String)>>) {
	for line in lines {
		for (_, field) in line {
			*field = field.trim().to_string()
		}
	}
}

fn exec_cmd(
	cmd: &Cmd,
	vicut: &mut ViCut,
	field_num: &mut usize,
	spent_cmds: &mut Vec<&Cmd>,
	fields: &mut Vec<(String,String)>,
	fmt_lines: &mut Vec<Vec<(String,String)>>
) {
	match cmd {
		Cmd::Repeat(n_cmds, n_repeats) => {
			trace!("Repeating {n_cmds} commands, {n_repeats} times");
			for _ in 0..*n_repeats {

				let mut pulled_cmds = vec![];
				let end = spent_cmds.len().saturating_sub(1);
				let offset = end.saturating_sub(*n_cmds);
				pulled_cmds.extend(spent_cmds.drain(offset..));

				for r_cmd in pulled_cmds {
					// We use recursion so that we can nest repeats easily
					exec_cmd(
						r_cmd,
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
		Cmd::Motion(motion) => {
			trace!("Executing non-capturing command: {motion}");
			if let Err(e) = vicut.move_cursor(motion) {
				eprintln!("vicut: {e}");
			}
		}
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
		Cmd::BreakGroup => {
			trace!("Breaking field group with fields: ");
			for field in &mut *fields {
				let name = &field.0;
				let content = &field.1;
				trace!("\t{name}: {content}");
			}
			*field_num = 0;
			if !fields.is_empty() {
				fmt_lines.push(std::mem::take(fields));
			}
		}
	}
}

fn execute_multi_thread(stream: Box<dyn BufRead>, args: &Argv) -> String {
	let lines: Vec<_> = stream.lines().collect::<Result<_,_>>().unwrap_or_else(|e| {
		eprintln!("vicut: {e}");
		std::process::exit(1);
	});

	// Pair each line with its original index
	let mut results: Vec<_> = lines
		.into_par_iter()
		.enumerate()
		.map(|(i, line)| {
			let output = execute(args, line);
			(i, output)
		})
	.collect();
	results.sort_by_key(|(i,_)| *i);
	let lines = results.into_iter().map(|(_,result)| {
		match result {
			Ok(line) => line,
			Err(e) => {
				eprintln!("vicut: {e}");
				std::process::exit(1)
			}
		}
	}).collect::<Vec<_>>();
	let mut output = String::new();
	for line in lines {
		write!(output,"{line}").unwrap();
	}
	output
}

#[cfg(test)]
fn call_main(args: &[&str], input: &str) -> Result<String,String> {
	if args.is_empty() {
		let mut output = String::new();
		write!(output,"USAGE:").unwrap(); 
		write!(output,"\tvicut [OPTIONS] [COMMANDS]...").unwrap();
		writeln!(output).unwrap();
		write!(output,"use '--help' for more information").unwrap();
		return Err(output)
	}
	if args.iter().any(|arg| *arg == "--help" || *arg == "-h") {
		return Ok(get_help())
	}
	let args = match Argv::parse_raw(args) {
		Ok(args) => args,
		Err(e) => {
			return Err(format!("vicut: {e}"));
		}
	};

	if args.json && args.delimiter.is_some() {
		eprintln!("vicut: WARNING: --delimiter flag is ignored when --json is used")
	}

	let mut stdout = String::new();

	use std::io::Cursor;
	if args.linewise {
		if args.single_thread {
			// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
			// So using it in pool.install() doesn't work. We have to initialize it in the closure there.

			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input)));
			let mut output = String::new();
			for result in stream.lines() {
				match result {
					Ok(line) => {
						match execute(&args,line) {
							Ok(new_line) => write!(output,"{new_line}").unwrap(),
							Err(e) => {
								return Err(format!("vicut: {e}"));
							}
						}
					}
					Err(e) => {
						return Err(format!("vicut: {e}"));
					}
				}
			}
			write!(stdout, "{output}").unwrap()
		} else if let Some(num) = args.max_jobs {
			let pool = rayon::ThreadPoolBuilder::new()
				.num_threads(num as usize)
				.build()
				.unwrap_or_else(|e| {
					eprintln!("vicut: Failed to build thread pool: {e}");
					std::process::exit(1)
				});
			let output = pool.install(|| {
				let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input.to_string())));
				execute_multi_thread(stream, &args)
			});
			write!(stdout, "{output}").unwrap();
		} else {
			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input.to_string())));
			let output = execute_multi_thread(stream, &args);
			write!(stdout, "{output}").unwrap();
		}
	} else {
		let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input)));
		let mut input = String::new();
		match stream.read_to_string(&mut input) {
			Ok(_) => {}
			Err(e) => {
				return Err(format!("vicut: {e}"));
			}
		}
		match execute(&args,input) {
			Ok(output) => write!(stdout,"{output}").unwrap(),
			Err(e) => return Err(format!("vicut: {e}")),
		}
	}
	Ok(stdout)
}

fn main() {
	if std::env::args().skip(1).count() == 0 {
		eprintln!("USAGE:"); 
		eprintln!("\tvicut [OPTIONS] [COMMANDS]...");
		eprintln!();
		eprintln!("use '--help' for more information");
		return
	}
	if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
		print!("{}",get_help());
		return
	}
	let args = match Argv::parse() {
		Ok(args) => args,
		Err(e) => {
			eprintln!("vicut: {e}");
			return;
		}
	};

	if args.json && args.delimiter.is_some() {
		eprintln!("vicut: WARNING: --delimiter flag is ignored when --json is used")
	}

	init_logger(args.trace);

	let mut stdout = io::stdout().lock();

	if args.linewise {
		if args.single_thread {
			// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
			// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
			let mut output = String::new();
			for result in stream.lines() {
				match result {
					Ok(line) => {
						match execute(&args,line) {
							Ok(new_line) => write!(output,"{new_line}").unwrap(),
							Err(e) => {
								eprintln!("vicut: {e}");
								return;
							}
						}
					}
					Err(e) => {
						eprintln!("vicut: {e}");
						return;
					}
				}
			}
			write!(stdout, "{output}").unwrap()
		} else if let Some(num) = args.max_jobs {
			let pool = rayon::ThreadPoolBuilder::new()
				.num_threads(num as usize)
				.build()
				.unwrap_or_else(|e| {
					eprintln!("vicut: Failed to build thread pool: {e}");
					std::process::exit(1)
				});
			let output = pool.install(|| {
				let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
				execute_multi_thread(stream, &args)
			});
			write!(stdout, "{output}").unwrap();
		} else {
			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
			let output = execute_multi_thread(stream, &args);
			write!(stdout, "{output}").unwrap();
		}
	} else {
		let mut stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
		let mut input = String::new();
		match stream.read_to_string(&mut input) {
			Ok(_) => {}
			Err(e) => {
				eprintln!("vicut: {e}");
				return;
			}
		}
		match execute(&args,input) {
			Ok(output) => write!(stdout,"{output}").unwrap(),
			Err(e) => eprintln!("vicut: {e}"),
		}
	}
}
