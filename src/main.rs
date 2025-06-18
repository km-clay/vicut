#![allow(clippy::unnecessary_to_owned,clippy::while_let_on_iterator)]
use std::{collections::BTreeMap, fmt::Write, fs, io::{self, BufRead, Write as IoWrite}, path::{Path, PathBuf}};

extern crate tikv_jemallocator;

#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

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

impl Argv {
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
				_ => new.handle_filename(arg)
			}
		}
		Ok(new)
	}
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

	if fmt_lines.is_empty() {
		// The user did not send any '-c' commands...?
		// They might just be editing text with '-m' calls.
		// Let's just push the entire buffer
		// If they don't want to see it they can just do > /dev/null
		let big_line = vicut.editor.buffer;
		fmt_lines.push(vec![("0".into(),big_line)]);
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
				let total = spent_cmds.len();
				let start = total.saturating_sub(*n_cmds);
				pulled_cmds.extend(spent_cmds.drain(start..));

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

fn execute_multi_thread_files(mut stdout: io::StdoutLock, args: &Argv) {
	let work: Vec<(PathBuf, String)> = args.files.par_iter()
		.fold(Vec::new, |mut acc,file| {
			let contents = fs::read_to_string(file).unwrap_or_else(|e| {
				eprintln!("vicut: failed to read file '{}': {e}",file.display());
				std::process::exit(1);
			});
			if args.edit_inplace && args.backup_files {
				let extension = args.backup_extension.as_deref().unwrap_or("bak");
				let backup_path = file.with_extension(format!(
						"{}.{extension}",
						file.extension()
						.and_then(|ext| ext.to_str())
						.unwrap_or("")
				));

				fs::copy(file, &backup_path).unwrap_or_else(|e| {
					eprintln!("vicut: failed to back up file '{}': {e}", file.display());
					std::process::exit(1)
				});
			}
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
		if args.edit_inplace {
			fs::write(&path, contents).unwrap_or_else(|e| {
				eprintln!("vicut: failed to write to file '{}': {e}",path.display());
				std::process::exit(1)
			});
		} else if args.files.len() > 1 {
			writeln!(stdout, "--- {}\n{}",path.display(), contents).ok();
		} else {
			writeln!(stdout, "{contents}").ok();
		}
	}
}

fn execute_multi_thread_files_linewise(mut stdout: io::StdoutLock, args: &Argv) {

	let work: Vec<(PathBuf, usize, String)> = args.files.par_iter()
		.fold(Vec::new, |mut acc,file| {
			let contents = fs::read_to_string(file).unwrap_or_else(|e| {
				eprintln!("vicut: failed to read file '{}': {e}",file.display());
				std::process::exit(1);
			});
			if args.edit_inplace && args.backup_files {
				let extension = args.backup_extension.as_deref().unwrap_or("bak");
				let backup_path = file.with_extension(format!(
						"{}.{extension}",
						file.extension()
						.and_then(|ext| ext.to_str())
						.unwrap_or("")
				));

				fs::copy(file, &backup_path).unwrap_or_else(|e| {
					eprintln!("vicut: failed to back up file '{}': {e}", file.display());
					std::process::exit(1)
				});
			}
			for (line_no,line) in contents.lines().enumerate() {
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
		per_file.entry(path)
			.or_default()
			.push((line_no,processed));
	}
	// Write back to file
	for (path, mut lines) in per_file {
		lines.sort_by_key(|(line_no,_)| *line_no); // Sort lines
		let output_final = lines.into_iter()
			.map(|(_,line)| line)
			.collect::<Vec<_>>()
			.join("");

		if args.edit_inplace {
			fs::write(&path, output_final).unwrap_or_else(|e| {
				eprintln!("vicut: failed to write to file '{}': {e}",path.display());
				std::process::exit(1)
			});
		} else if args.files.len() > 1 {
			writeln!(stdout, "--- {}\n{}",path.display(), output_final).ok();
		} else {
			writeln!(stdout, "{output_final}").ok();
		}
	}
}

fn execute_multi_thread_stdin(stream: Box<dyn BufRead>, args: &Argv) -> String {
	let lines: Vec<_> = stream.lines().collect::<Result<_,_>>().unwrap_or_else(|e| {
		eprintln!("vicut: {e}");
		std::process::exit(1);
	});

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
	let mut output = String::new();
	for (_,line) in lines {
		writeln!(output,"{line}").ok();
	}
	output
}


#[allow(unreachable_code)]
fn main() {
	#[cfg(debug_assertions)]
	{
		// Testing
		let input = "This is some\nMultiline text\nWith trailing\nNewlines\n\n\n\n\n\n\n";
		println!("{input}");

		let args = [
			"-m", "Gvgeld",
		];
		let output = call_main(&args, input).unwrap();
		println!("{output}");
		return
	}

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
	if std::env::args().any(|arg| arg == "--version") {
		println!("vicut {}", env!("CARGO_PKG_VERSION"));
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

	if args.linewise {
		if args.single_thread {
			let mut stdout = io::stdout().lock();

			// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
			// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
			let mut output = String::new();
			if !args.files.is_empty() {
				for path in &args.files {
					let file = fs::File::open(path).unwrap_or_else(|e| {
						eprintln!("vicut: failed to read file '{}': {e}",path.display());
						std::process::exit(1)
					});
					if args.edit_inplace && args.backup_files {
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
					let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(file));
					for result in stream.lines() {
						match result {
							Ok(line) => {
								match execute(&args,line) {
									Ok(new_line) => writeln!(output,"{new_line}").ok(),
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
						};
					}
					if args.edit_inplace {
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
				let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
				for result in stream.lines() {
					match result {
						Ok(line) => {
							match execute(&args,line) {
								Ok(new_line) => writeln!(output,"{new_line}").ok(),
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
					};
				}
			}
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
						execute_multi_thread_files_linewise(stdout, &args);
						// Output has already been handled
						std::process::exit(0);
					} else {
						let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
						execute_multi_thread_stdin(stream, &args)
					};
					writeln!(stdout, "{output}").ok();
				});
		} else {
			let mut stdout = io::stdout().lock();
			let output = if !args.files.is_empty() {
				execute_multi_thread_files_linewise(stdout, &args);
				// Output has already been handled
				std::process::exit(0);
			} else {
				let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(io::stdin()));
				execute_multi_thread_stdin(stream, &args)
			};
			writeln!(stdout, "{output}").ok();
		}
	} else if !args.files.is_empty() {
		if args.single_thread {
			let mut stdout = io::stdout().lock();
			for path in &args.files {
				let content = fs::read_to_string(path).unwrap_or_else(|e| {
					eprintln!("vicut: failed to read file '{}': {e}",path.display());
					std::process::exit(1)
				});
				if args.edit_inplace && args.backup_files {
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
				match execute(&args,content) {
					Ok(mut output) => {
						if args.edit_inplace {
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
				execute_multi_thread_files(stdout, &args);
			});
		} else {
			let mut stdout = io::stdout().lock();
			execute_multi_thread_files(stdout, &args);
		}
	} else {
		let mut stdout = io::stdout().lock();
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
			Ok(output) => { writeln!(stdout,"{output}").ok(); }
			Err(e) => eprintln!("vicut: {e}"),
		};
	}
}

/*
 * Stuff down here is for testing
 */

/// Used to call the main logic internally, for testing
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
	let args = match Argv::parse_raw(args) {
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

			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input)));
			let mut output = String::new();
			for result in stream.lines() {
				match result {
					Ok(line) => {
						match execute(&args,line) {
							Ok(new_line) => writeln!(output,"{new_line}").ok(),
							Err(e) => {
								return Err(format!("vicut: {e}"));
							}
						}
					}
					Err(e) => {
						return Err(format!("vicut: {e}"));
					}
				};
			}
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
				execute_multi_thread_stdin(stream, &args)
			}))
		} else {
			let stream: Box<dyn BufRead> = Box::new(io::BufReader::new(Cursor::new(input.to_string())));
			Ok(execute_multi_thread_stdin(stream, &args))
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
			Ok(output) => Ok(output),
			Err(e) => Err(format!("vicut: {e}")),
		}
	}
}
#[cfg(any(test,debug_assertions))]
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
