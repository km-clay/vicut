use std::iter::{Peekable, Skip};
use std::fmt::Write;

use crate::{linebuf::LineBuf, modes::{normal::ViNormal, ViMode}, Opts, Cmd};
use pretty_assertions::assert_eq;

pub const LOREM_IPSUM: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum. Curabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub const LOREM_IPSUM_MULTILINE: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.\nUt enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat.\nDuis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur.\nExcepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\nCurabitur pretium tincidunt lacus. Nulla gravida orci a odio. Nullam varius, turpis et commodo pharetra.";

pub mod modes;
pub mod linebuf;
pub mod editor;
pub mod files;
pub mod pattern_match;
pub mod wiki_examples;

fn vicut_integration(input: &str, args: &[&str], expected: &str) {
	let output = call_main(args, input).unwrap();
	let output = output.strip_suffix("\n").unwrap_or(&output);
	/*
	println!("got: {output:?}");
	println!("expected: {expected:?}");
	*/
	assert_eq!(output,expected)
}


fn normal_cmd(cmd: &str, buf: &str, cursor: usize) -> (String,usize) {
	let cmd = ViNormal::new()
		.cmds_from_raw(cmd)
		.pop()
		.unwrap();
	let mut buf = LineBuf::new().with_initial(buf.to_string(), cursor);
	buf.exec_cmd(cmd).unwrap();
	(buf.as_str().to_string(),buf.cursor.get())
}

/*
 * Stuff down here is for testing
 */

/// Testing fixture
/// Used to call the main logic internally
#[cfg(any(test,debug_assertions))]
pub fn call_main(args: &[&str], input: &str) -> Result<String,String> {
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

	let mut args_iter = args.iter();
	args_iter.find(|arg| **arg == "--script"); // let's find the --script flag
	let script = args_iter.next(); // If we found it, the next arg is the script name

	let args = if let Some(script) = script {
		let script = PathBuf::from(script);
		Opts::from_script(script).unwrap_or_else(|e| {
			eprintln!("{e}");
			std::process::exit(1)
		})
	} else {
		// Let's see if we got a literal in-line script instead then
		let mut flags = args.iter().take_while(|arg| **arg != "--");
		let use_inline = flags.all(|arg| !arg.starts_with('-'));

		if use_inline {
			// We know that there's at least one argument, so we can safely unwrap
			let mut args = args.iter();
			let maybe_script = args.next().unwrap();
			let mut opts = if Opts::validate_filename(maybe_script).is_err() {
				// It's not a file...
				// Let's see if it's a valid in-line script
				Opts::from_raw(maybe_script).unwrap_or_else(|e| {
					eprintln!("{e}");
					std::process::exit(1)
				})
			} else {
				// It's a file, let's see if it's a valid script
				let script_path = PathBuf::from(maybe_script);
				Opts::from_script(script_path).unwrap_or_else(|e| {
					eprintln!("{e}");
					std::process::exit(1)
				})
			};
			// Now let's grab the file names
			for arg in args {
				if let Err(e) = Opts::validate_filename(arg) {
					eprintln!("vicut: {e}");
					std::process::exit(1);
				}
				opts.files.push(PathBuf::from(arg));
			}
			opts
		} else {
			eprintln!("args: {args:?}");
			// We're using command line arguments
			// boo
			Opts::parse_raw(args).unwrap_or_else(|e| {
				eprintln!("vicut: {e}");
				std::process::exit(1)
			})
		}
	};

	use std::{io::{self, BufRead, Cursor}, path::PathBuf};

use crate::{execute, execute_linewise, format_output, get_help, get_lines, Opts};
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
impl Opts {
	pub fn parse_raw(args: &[&str]) -> Result<Self,String> {
		let mut new = Self::default();
		let mut full_args = vec!["vicut"];
		full_args.extend(args.iter());
		let mut args = full_args.iter().skip(1).peekable();
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
				"--global-uses-line-numbers" => {
					new.global_uses_line_numbers = true;
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
				"-v" | "--not-global" |
				"-g" | "--global" => {
					let global = Self::handle_global_arg_raw(arg, &mut args);
					new.cmds.push(global);
				}
				_ => new.handle_filename(arg.to_string())
			}
		}
		Ok(new)
	}
	fn handle_global_arg_raw(arg: &str, args: &mut Peekable<Skip<std::slice::Iter<'_, &str>>>) -> Cmd {
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
			match *global_arg {
				"-n" | "--next" => then_cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or(&"1")
						.parse::<usize>()
						.unwrap_or_else(|_| {
							eprintln!("Expected a number after '{global_arg}'");
							std::process::exit(1)
						});
					let repeat_count = args
						.next()
						.unwrap_or(&"1")
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
						else_cmds.push(Cmd::Motion(arg.to_string()))
					} else {
						then_cmds.push(Cmd::Motion(arg.to_string()))
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
							cmds.push(Cmd::NamedField(name,arg.to_string()));
						} else {
							then_cmds.push(Cmd::NamedField(name,arg.to_string()));
						}
					} else {
						if arg.starts_with('-') {
							eprintln!("Expected a selection command after '-c', found {arg}");
							std::process::exit(1);
						}
						if let Some(cmds) = else_cmds.as_mut() {
							cmds.push(Cmd::Field(arg.to_string()));
						} else {
							then_cmds.push(Cmd::Field(arg.to_string()));
						}
					}
				}
				"-g" | "--global" |
				"-v" | "--not-global" => {
					let nested = Self::handle_global_arg_raw(global_arg, args);
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
						pattern: arg.to_string(),
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
			pattern: arg.to_string(),
			then_cmds,
			else_cmds,
			polarity
		}
	}
}
