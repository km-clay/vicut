use std::io::{self, Write, BufRead};


use exec::ViCut;
use log::{debug, error, info, trace, warn};
use serde_json::{Map, Value};

pub mod vicmd;
pub mod vimode;
pub mod exec;
pub mod linebuf;
pub mod keys;
pub mod register;
pub mod reader;

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
	input: Option<String>,
	file: Option<String>,
	delimiter: Option<String>,

	json: bool,
	trace: bool,
	linewise: bool,
	trim_fields: bool,

	cmds: Vec<Cmd>
}

impl Argv {
	pub fn parse() -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = std::env::args().skip(1);
		while let Some(arg) = args.next() {
			match arg.as_str() {
				"--json" => {
					new.json = true;
				}
				"--trace" => {
					new.trace = true;
				}
				"--linewise" => {
					new.linewise = true;
				}
				"--trim-fields" => {
					new.trim_fields = true;
				}
				"--input" => {
					let Some(arg) = args.next() else {
						return Err("Expected a string after '--input'".into())
					};
					if arg.starts_with('-') {
						return Err(format!("Expected a motion command after '-m', found {arg}"))
					}
					if new.file.is_some() {
						return Err("--input and --file are mutually exclusive".into())
					}
					new.input = Some(arg);
				}
				"--file" => {
					let Some(arg) = args.next() else {
						return Err("Expected a path after '--file'".into())
					};
					if arg.starts_with('-') {
						return Err(format!("Expected a path after '--file', found {arg}"))
					}
					if new.input.is_some() {
						return Err("--input and --file are mutually exclusive".into())
					}
					new.file = Some(arg);
				}
				"--delimiter" | "-d" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a delimiter after '-m', found {arg}"))
					}
					new.delimiter = Some(arg)
				}
				"-n" => new.cmds.push(Cmd::BreakGroup),
				"-r" => {
					let cmd_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or(1);
					let repeat_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or(1);

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

fn format_output_json(lines: Vec<Vec<(String,String)>>) {
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
	let output = serde_json::to_string_pretty(&json).unwrap();
	println!("{output}")
}

fn format_output_standard(delimiter: &str, lines: Vec<Vec<(String,String)>>) {
	let lines = lines.into_iter()
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
		});

	print!("{lines}");
}

fn execute(args: Argv, input: String) {

	let delimiter = args.delimiter.unwrap_or("\t".into());
	let mut fields: Vec<(String,String)> = vec![];
	let mut fmt_lines: Vec<Vec<(String,String)>> = vec![];

	let mut vicut = match ViCut::new(&input, 0) {
		Ok(v) => v,
		Err(e) => {
			eprintln!("vicut: {e}");
			return;
		}
	};

	let mut spent_cmds: Vec<Cmd> = vec![];

	let mut field_num = 0;
	for cmd in args.cmds {
		exec_cmd(
			&cmd,
			&mut vicut,
			&mut field_num,
			&mut spent_cmds,
			&mut fields,
			&mut fmt_lines
		);
		spent_cmds.push(cmd);
		vicut.set_normal_mode();
	}

	if !fields.is_empty() {
		fmt_lines.push(std::mem::take(&mut fields));
	}

	if args.trim_fields {
		trim_fields(&mut fmt_lines);
	}

	if args.json {
		format_output_json(fmt_lines);
	} else {
		format_output_standard(&delimiter, fmt_lines);
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
	spent_cmds: &mut Vec<Cmd>,
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
					exec_cmd(&r_cmd, vicut, field_num, spent_cmds, fields, fmt_lines);
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
					let name = format!("field_{field_num}");
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

fn main() {
	if std::env::args().skip(1).count() == 0 {
		eprintln!("print usage here lol"); // TODO: actually print the usage here
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

	let mut stream: Box<dyn BufRead> = if let Some(input) = args.input.clone() {
		Box::new(io::Cursor::new(input))
	} else if let Some(file) = args.file.clone() {
		match std::fs::File::open(file) {
			Ok(file) => Box::new(io::BufReader::new(file)),
			Err(e) => {
				eprintln!("vicut: {e}");
				return;
			}
		}
	} else {
		Box::new(io::BufReader::new(io::stdin()))
	};

	if args.linewise {
		for line in stream.lines() {
			match line {
				Ok(line) => {
					execute(args.clone(),line)
				}
				Err(e) => {
					eprintln!("vicut: {e}");
					return;
				}
			}
		}
	} else {
		let mut input = String::new();
		match stream.read_to_string(&mut input) {
			Ok(_) => {}
			Err(e) => {
				eprintln!("vicut: {e}");
				return;
			}
		}
		execute(args,input);
	}
}
