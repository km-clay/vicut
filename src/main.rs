use std::io::{self, BufRead};


use exec::ViCut;
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
	NamedField(Name,String)
}

#[derive(Default,Debug)]
struct Argv {
	input: Option<String>,
	file: Option<String>,
	delimiter: Option<String>,

	json: bool,
	csv: bool,

	cmds: Vec<Cmd>
}

impl Argv {
	pub fn parse() -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = std::env::args().skip(1);
		while let Some(arg) = args.next() {
			match arg.as_str() {
				"--json" => {
					if new.csv {
						return Err("--json and --csv are mutually exclusive".into())
					}
					new.json = true;
				}
				"--csv" => {
					if new.json {
						return Err("--json and --csv are mutually exclusive".into())
					}
					new.csv = true;
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


					let repeats = new.cmds
						.iter()
						.rev()
						.take(cmd_count)
						.cloned()
						.collect::<Vec<_>>();

					for _ in 0..repeat_count {
						new.cmds.extend(repeats.clone().into_iter().rev());
					}
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

	let input: Box<dyn BufRead> = if let Some(input) = args.input {
		Box::new(io::Cursor::new(input))
	} else if let Some(file) = args.file {
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

	let delimiter = args.delimiter.unwrap_or("\t".into());
	let mut fields: Vec<(String,String)> = vec![];
	let mut fmt_lines: Vec<Vec<(String,String)>> = vec![];

	for line_result in input.lines() {
		let line = match line_result {
			Ok(l) => l,
			Err(e) => {
				eprintln!("vicut: error reading line: {e}");
				return;
			}
		};
		if line.is_empty() { continue }

		let mut vicut = match ViCut::new(&line, 0) {
			Ok(v) => v,
			Err(e) => {
				eprintln!("vicut: {e}");
				return;
			}
		};


		let mut field_num = 0;
		for cmd in &args.cmds {
			match cmd {
				Cmd::Motion(cmd) => {
					if let Err(e) = vicut.move_cursor(cmd) {
						eprintln!("vicut: {e}");
						return;
					}
				}
				Cmd::Field(motion) => {
					field_num += 1;
					match vicut.read_field(motion) {
						Ok(field) => {
							let name = format!("field_{field_num}");
							fields.push((name,field))
						}
						Err(e) => {
							eprintln!("vicut: {e}");
							return;
						}
					}
				}
				Cmd::NamedField(name, motion) => {
					field_num += 1;
					match vicut.read_field(motion) {
						Ok(field) => fields.push((name.clone(),field)),
						Err(e) => {
							eprintln!("vicut: {e}");
							return;
						}
					}
				}
			}
			vicut.set_normal_mode();
		}

		fmt_lines.push(std::mem::take(&mut fields));
	}

	if args.json {
		format_output_json(fmt_lines);
	} else {
		format_output_standard(&delimiter, fmt_lines);
	}
}
