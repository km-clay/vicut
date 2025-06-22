//! This module contains the parsing logic for the `vic` scripting language.
//! `vic` is a simple language that allows `vicut` users to more easily write complex command strings.

use std::path::PathBuf;

use pest::{iterators::Pair, Parser};
use pest_derive::Parser;
use crate::Opts;

use super::Cmd;

#[derive(Parser)]
#[grammar = "vic/vic.pest"] // relative to src
pub struct VicParser;

pub fn parse_vic(input: &str) -> Result<Opts, pest::error::Error<Rule>> {
	let pairs = VicParser::parse(Rule::vic, input)?.next().unwrap().into_inner();
	let mut opts = Opts::default();

	for pair in pairs {
		match pair.as_rule() {
			Rule::prelude => {
				let prelude_opts = pair.into_inner().next().unwrap();
				for pair in prelude_opts.into_inner() {
					let pair = pair.into_inner().next().unwrap();
					match pair.as_rule() {
						Rule::json => opts.json = true,
						Rule::linewise => opts.linewise = true,
						Rule::trim_fields => opts.trim_fields = true,
						Rule::serial => opts.single_thread = true,
						Rule::keep_mode => opts.keep_mode = true,
						Rule::backup => opts.backup_files = true,
						Rule::edit_inplace => opts.edit_inplace = true,
						Rule::trace => opts.trace = true,
						Rule::max_jobs => {
							let max_jobs = pair.into_inner().next().unwrap();
							opts.max_jobs = Some(max_jobs.as_str().parse::<u32>().unwrap());
						}
						Rule::delimiter => {
							let delimiter = pair.into_inner().next().unwrap()
								.into_inner().next().unwrap();
							opts.delimiter = Some(delimiter.as_str().to_string());
						}
						Rule::template => {
							let template = pair.into_inner().next().unwrap()
								.into_inner().next().unwrap();
							opts.template = Some(template.as_str().to_string());
						}
						Rule::file => {
							let file = pair.into_inner().next().unwrap()
								.as_str().to_string();

							for entry in glob::glob(&file).unwrap() {
								match entry {
									Ok(path) => opts.files.push(path),
									Err(e) => {
										eprintln!("vicut: error resolving file path: {e}");
										std::process::exit(1);
									}
								}
							}
						}
						Rule::files => {
							let files = pair.into_inner();
							for file in files {
								let file = file.as_str().to_string();

								for entry in glob::glob(&file).unwrap() {
									match entry {
										Ok(path) => opts.files.push(path),
										Err(e) => {
											eprintln!("vicut: error resolving file path: {e}");
											std::process::exit(1);
										}
									}
								}
							}
						}
						Rule::backup_ext => {
							let ext = pair.into_inner().next().unwrap()
								.into_inner().next().unwrap();
							opts.backup_extension = Some(ext.as_str().to_string());
						}
						Rule::pipe_in => {
							let pipe_in = pair.into_inner().next().unwrap()
								.into_inner().next().unwrap();
							opts.pipe_in = Some(pipe_in.as_str().to_string());
						}
						Rule::pipe_out => {
							let pipe_out = pair.into_inner().next().unwrap()
								.into_inner().next().unwrap();
							opts.pipe_out = Some(pipe_out.as_str().to_string());
						}
						Rule::write => {
							let write = pair.into_inner().next().unwrap();
							opts.out_file = Some(PathBuf::from(write.as_str().to_string()));
						}

						_ => unreachable!("Unexpected rule in prelude: {:?}", pair.as_rule()),
					}
				}
			}
			Rule::cmd => parse_cmd(&mut opts.cmds, pair),
			_ => unreachable!("Unexpected rule in vic: {:?}", pair.as_rule()),
		}
	}
	Ok(opts)
}

fn parse_cmd(cmds: &mut Vec<Cmd>, pair: Pair<Rule>) {
	for pair in pair.into_inner() {
		match pair.as_rule() {
			Rule::global_cmd => {
				let cmd = parse_global(pair,true);
				cmds.push(cmd);
			}
			Rule::not_global_cmd => {
				let cmd = parse_global(pair,false);
				cmds.push(cmd);
			}
			Rule::repeat_cmd => {
				let repeat_cmds = parse_repeat(pair);
				cmds.extend(repeat_cmds);
			}
			Rule::cut_cmd => {
				let cut_cmd = pair.into_inner().next().unwrap()
					.into_inner().next().unwrap().as_str().to_string();
				let cmd = Cmd::Field(cut_cmd);
				cmds.push(cmd);
			}
			Rule::move_cmd => {
				let move_cmd = pair.into_inner().next().unwrap()
					.into_inner().next().unwrap().as_str().to_string();
				let cmd = Cmd::Motion(move_cmd);
				cmds.push(cmd);
			}
			Rule::next => {
				cmds.push(Cmd::BreakGroup);
			}
			_ => unreachable!("Unexpected rule in cmd: {:?}", pair.as_rule()),
		}
	}
}

fn parse_global(pair: Pair<Rule>, polarity: bool) -> Cmd {
	let mut inner = pair.into_inner();
	let pattern = inner.next().unwrap().into_inner()
		.next().unwrap().as_str().to_string();
	let block = inner.next().unwrap().into_inner();
	let mut then_cmds = vec![];
	let mut else_cmds = None;
	for cmd in block {
		parse_cmd(&mut then_cmds, cmd);
	}

	if let Some(else_block) = inner.next() {
		let mut else_block_cmds = vec![];
		for cmd in else_block.into_inner() {
			parse_cmd(&mut else_block_cmds, cmd);
		}
		else_cmds = Some(else_block_cmds);
	}

	Cmd::Global { pattern, then_cmds, else_cmds, polarity }
}

fn parse_repeat(pair: Pair<Rule>) -> Vec<Cmd> {
	let mut cmds = vec![];
	let mut inner = pair.into_inner();
	let repeat_count = inner.next().unwrap()
		.as_str().trim().to_string()
		.parse::<usize>().unwrap();

	let block = inner.next().unwrap().into_inner();
	for cmd in block {
		parse_cmd(&mut cmds, cmd);
	}
	let cmd_count = cmds.len();

	cmds.push(Cmd::Repeat(cmd_count,repeat_count));
	cmds
}
