use std::io::{self, BufRead};


use exec::ViCut;

pub mod vicmd;
pub mod vimode;
pub mod exec;
pub mod linebuf;
pub mod keys;
pub mod register;
pub mod reader;

#[derive(Debug)]
enum Cmd {
	Motion(String),
	Field(String)
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
				"-m" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a motion command after '-m', found {arg}"))
					}
					new.cmds.push(Cmd::Motion(arg))
				}
				"-f" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a selection command after '-f', found {arg}"))
					}
					new.cmds.push(Cmd::Field(arg));
				}
				arg => { return Err(format!("Unrecognized argument '{arg}'")) }
			}
		}
		Ok(new)
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

	for line_result in input.lines() {
		let line = match line_result {
			Ok(l) => l,
			Err(e) => {
				eprintln!("vicut: error reading line: {e}");
				continue;
			}
		};

		let mut vicut = match ViCut::new(&line, 0) {
			Ok(v) => v,
			Err(e) => {
				eprintln!("vicut: {e}");
				continue;
			}
		};

		let mut fields = vec![];

		for cmd in &args.cmds {
			match cmd {
				Cmd::Motion(cmd) => {
					if let Err(e) = vicut.move_cursor(cmd) {
						eprintln!("vicut: {e}");
						continue;
					}
				}
				Cmd::Field(motion) => {
					match vicut.read_field(motion) {
						Ok(field) => fields.push(field),
						Err(e) => {
							eprintln!("vicut: {e}");
							continue;
						}
					}
				}
			}
		}

		let output = fields.join(&delimiter);
		println!("{output}");
	}
}
