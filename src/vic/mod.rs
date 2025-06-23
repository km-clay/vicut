//! This module contains the parsing logic for the `vic` scripting language.
//! `vic` is a simple language that allows `vicut` users to more easily write complex command strings.

use std::{fmt::Display, path::PathBuf};

use pest::{iterators::Pair, Parser};
use pest_derive::Parser;
use crate::{exec::{Val, ViCut}, CondBlock, Opts};

use super::Cmd;

#[derive(Debug, Clone)]
pub enum CmdArg {
	Literal(Val),
	Var(String),
	Count(usize),
	Expr(Expr),
}

impl CmdArg {
	pub fn is_truthy(&self, vicut: &mut ViCut) -> bool {
		match self {
			CmdArg::Literal(lit) => {
				lit.is_truthy()
			}
			CmdArg::Var(var) => {
				let Some(val) = vicut.get_var(var) else {
					eprintln!("vicut: variable '{var}' not found");
					std::process::exit(1)
				};
				val.is_truthy()
			}
			CmdArg::Expr(expr) => {
				match expr {
					Expr::Var(var) => {
						let Some(val) = vicut.get_var(var) else {
							eprintln!("vicut: variable '{var}' not found");
							std::process::exit(1)
						};
						val.is_truthy()
					}
					Expr::Return(cmd) => !cmd.is_empty(),
					Expr::Literal(lit) => !lit.is_empty(),
					Expr::Int(int) => {
						Val::Num(*int as isize).is_truthy()
					}
					Expr::Bool(bool) => {
						Val::Bool(*bool).is_truthy() // this is a bit redundant
					}
					Expr::BoolExp { op, left, right } => {
						vicut.eval_bool_expr(op, left, right).unwrap_or_else(|e| {
							eprintln!("vicut: {e}");
							std::process::exit(1)
						}).is_truthy()
					}
					Expr::BinExp { op, left, right } => {
						vicut.eval_bin_expr(op, left, right).unwrap_or_else(|e| {
							eprintln!("vicut: {e}");
							std::process::exit(1)
						}).is_truthy()
					}
				}
			}
			_ => unreachable!()
		}
	}
}

#[derive(Debug, Clone)]
pub enum BinOp {
	Add,
	Sub,
	Mult,
	Div,
	Mod,
	Pow,
}

impl BinOp {
	pub fn bin_op_from_rule(pair: Pair<Rule>) -> Self {
		match pair.as_rule() {
			Rule::add => BinOp::Add,
			Rule::sub => BinOp::Sub,
			Rule::mult => BinOp::Mult,
			Rule::div => BinOp::Div,
			Rule::modulo => BinOp::Mod,
			Rule::pow => BinOp::Pow,
			_ => unreachable!("Unexpected rule in bin_expr: {:?}", pair.as_rule()),
		}
	}
}

#[derive(Debug, Clone)]
pub enum UnOp {
	Neg,
	Not,
}
#[derive(Debug, Clone)]
pub enum BoolOp {
	And,
	Or,
	Xor,
	Eq,
	Ne,
	Lt,
	Gt,
	LtEq,
	GtEq,
}
impl BoolOp {
	pub fn bool_op_from_rule(mut pair: Pair<Rule>) -> Self {
		if pair.as_rule() == Rule::bool_conjunction {
			pair = pair.into_inner().next().unwrap();
		}
		match pair.as_rule() {
			Rule::and => BoolOp::And,
			Rule::or => BoolOp::Or,
			Rule::eq => BoolOp::Eq,
			Rule::ne => BoolOp::Ne,
			Rule::lt => BoolOp::Lt,
			Rule::gt => BoolOp::Gt,
			Rule::le => BoolOp::LtEq,
			Rule::ge => BoolOp::GtEq,
			_ => unreachable!("Unexpected rule in bool_expr: {:?}", pair.as_rule()),
		}
	}
}

impl Display for BoolOp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			BoolOp::And => write!(f, "&&"),
			BoolOp::Or => write!(f, "||"),
			BoolOp::Xor => write!(f, "^"),
			BoolOp::Eq => write!(f, "=="),
			BoolOp::Ne => write!(f, "!="),
			BoolOp::Lt => write!(f, "<"),
			BoolOp::Gt => write!(f, ">"),
			BoolOp::LtEq => write!(f, "<="),
			BoolOp::GtEq => write!(f, ">="),
		}
	}
}

#[derive(Debug, Clone)]
pub enum Expr {
	Var(String),
	Int(i64),
	Bool(bool),
	Literal(String),
	Return(String), // extract via vim command
	BoolExp {
		op: BoolOp,
		left: (bool,Box<Expr>), // (is negated, expr)
		right: (bool,Box<Expr>), // (is negated, expr)
	},
	BinExp {
		op: BinOp,
		left: Box<Expr>,
		right: Box<Expr>,
	}
}

impl Expr {
	pub fn eval_atom(pair: Pair<Rule>) -> Self {
		match pair.as_rule() {
			Rule::bin_lit => {
				let mut literal = pair.into_inner();
				let mut int = literal.next().unwrap();
				let mut is_negated = false;
				if let Rule::unary_minus = int.as_rule() {
					int = int.into_inner().next().unwrap();
					is_negated = true;
				}
				let int = int.as_str().parse::<i64>().unwrap();
				if is_negated {
					Self::Int(-int)
				} else {
					Self::Int(int)
				}
			}
			Rule::int => {
				let int = pair.as_str().parse::<i64>().unwrap();
				Self::Int(int)
			}
			Rule::var => {
				let var = pair.into_inner().next().unwrap()
					.as_str().to_string();
				Self::Var(var)
			}
			_ => unreachable!("Unexpected rule in bin_atom: {:?}", pair.as_rule()),
		}
	}
	pub fn from_rule(pair: Pair<Rule>) -> Self {
		// we do a little hacking
		let inner = if matches!(pair.as_rule(), Rule::bool_expr_single | Rule::int) {
			pair
		} else {
			pair.into_inner().next().unwrap()
		};
		match inner.as_rule() {
			Rule::return_cmd => {
				let return_cmd = inner.into_inner().next().unwrap()
					.into_inner().next().unwrap()
					.into_inner().next().unwrap();
				let return_cmd = return_cmd.as_str().to_string();
				Self::Return(return_cmd)
			}
			Rule::value => {
				Self::from_rule(inner)
			}
			Rule::bool => {
				let bool = inner.into_inner().next().unwrap();
				match bool.as_rule() {
					Rule::true_lit => Self::Bool(true),
					Rule::false_lit => Self::Bool(false),
					_ => unreachable!("Unexpected rule in bool: {:?}", bool.as_rule()),
				}
			}
			Rule::literal => {
				let literal = inner.into_inner().next().unwrap()
					.as_str().to_string();
				Self::Literal(literal)
			}
			Rule::var => {
				let var = inner.into_inner().next().unwrap()
					.as_str().to_string();
				Self::Var(var)
			}
			Rule::var_name => {
				let var = inner.as_str().to_string();
				Self::Var(var)
			}
			Rule::int => {
				let int = inner.as_str().parse::<i64>().unwrap();
				Self::Int(int)
			}
			Rule::bool_lit => {
				// This can be a nested boolean expression
				let lit = inner.into_inner().next().unwrap();
				Self::from_rule(lit)
			}
			Rule::bin_lit => {
				let int = inner.as_str().parse::<i64>().unwrap();
				Self::Int(int)
			}
			Rule::bin_expr => {
				let mut expr = inner.into_inner();
				let mut left_pair = expr.next().unwrap().into_inner().next().unwrap();
				let mut left_negated = false;
				if let Rule::unary_minus = left_pair.as_rule() {
					left_pair = left_pair.into_inner().next().unwrap();
					left_negated = true;
				}

				let mut left = Self::from_rule(left_pair);

				if let Expr::Int(int) = &mut left {
					if left_negated {
						*int = -(*int);
					}
				};

				while let Some(op_pair) = expr.next() {
					let op = BinOp::bin_op_from_rule(op_pair);
					let mut right_pair = expr.next().unwrap().into_inner().next().unwrap();
					let mut right_negated = false;
					if let Rule::unary_minus = right_pair.as_rule() {
						right_pair = right_pair.into_inner().next().unwrap();
						right_negated = true;
					}
					let mut right = Self::from_rule(right_pair);
					if let Expr::Int(int) = &mut right {
						if right_negated {
							*int = -(*int);
						}
					};

					left = Self::BinExp { op, left: Box::new(left), right: Box::new(right) };
				}

				left
			}

			Rule::bool_expr_single => {
				let mut expr = inner.into_inner();
				let mut left_pair = expr.next().unwrap().into_inner().next().unwrap();
				let mut left_negated = false;
				if let Rule::not = left_pair.as_rule() {
					left_pair = left_pair.into_inner().next().unwrap();
					left_negated = true;
				}
				let mut left = Self::from_rule(left_pair);

				if let Expr::Bool(bool) = &mut left {
					if left_negated {
						*bool = !(*bool);
					}
				};

				if let Some(op_pair) = expr.next() {
					let op = BoolOp::bool_op_from_rule(op_pair);
					let mut right_pair = expr.next().unwrap().into_inner().next().unwrap();
					let mut right_negated = false;
					if let Rule::not = right_pair.as_rule() {
						right_pair = right_pair.into_inner().next().unwrap();
						right_negated = true;
					}
					let mut right = Self::from_rule(right_pair);

					if let Expr::Bool(bool) = &mut right {
						if right_negated {
							*bool = !(*bool);
						}
					};

					return Self::BoolExp { op, left: (left_negated,Box::new(left)), right: (right_negated,Box::new(right)) };
				}

				left
			}
			Rule::bool_expr => {
				let mut expr = inner.into_inner();
				let mut left_pair = expr.next().unwrap().into_inner().next().unwrap();
				let mut left_negated = false;
				if let Rule::not = left_pair.as_rule() {
					left_pair = left_pair.into_inner().next().unwrap();
					left_negated = true;
				}
				let mut left = Self::from_rule(left_pair);

				while let Some(op_pair) = expr.next() {
					let op = BoolOp::bool_op_from_rule(op_pair);
					let mut right_pair = expr.next().unwrap().into_inner().next().unwrap();
					let mut right_negated = false;
					if let Rule::not = right_pair.as_rule() {
						right_pair = right_pair.into_inner().next().unwrap();
						right_negated = true;
					}
					let right = Self::from_rule(right_pair);

					left = Self::BoolExp { op, left: (left_negated,Box::new(left)), right: (right_negated,Box::new(right)) };
				}

				left
			}
			_ => unreachable!("Unexpected rule in expr: {:?}", inner.as_rule()),
		}
	}
}

impl Display for BinOp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			BinOp::Add => write!(f, "+"),
			BinOp::Sub => write!(f, "-"),
			BinOp::Mult => write!(f, "*"),
			BinOp::Div => write!(f, "/"),
			BinOp::Mod => write!(f, "%"),
			BinOp::Pow => write!(f, "^"),
		}
	}
}

impl Display for Expr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Expr::Var(var) => write!(f, "${var}"),
			Expr::Int(int) => write!(f, "{int}"),
			Expr::Literal(lit) => write!(f, "{lit}"),
			Expr::Return(lit) => write!(f, "{lit}"),
			Expr::Bool(bool) => write!(f, "{bool}"),
			Expr::BoolExp { op, left, right } => {
				let (left_negated, left) = left;
				let (right_negated, right) = right;
				let right_display = if *right_negated {
					format!("!{right}")
				} else {
					right.to_string()
				};
				let left_display = if *left_negated {
					format!("!{left}")
				} else {
					left.to_string()
				};
				write!(f, "({left_display} {op} {right_display})")
			}
			Expr::BinExp { op, left, right } => {
				write!(f, "({left} {op} {right})")
			}
		}
	}
}

impl Display for CmdArg {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			CmdArg::Literal(lit) => write!(f, "{lit}"),
			CmdArg::Var(var) => write!(f, "${var}"),
			CmdArg::Count(count) => write!(f, "{count}"),
			CmdArg::Expr(expr) => expr.fmt(f),
		}
	}
}

#[derive(Parser)]
#[grammar = "vic/vic.pest"] // relative to src
pub struct VicParser;

pub fn parse_vic(input: &str) -> Result<Opts, String> {
	let pairs = VicParser::parse(Rule::vic, input)
		.map_err(|e| format!("vicut: error parsing vic script: {e}"))?.next().unwrap().into_inner();
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
						Rule::silent => opts.silent = true,
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
				let repeat_cmd = parse_repeat(pair);
				cmds.push(repeat_cmd);
			}
			Rule::cut_cmd => {
				let cut_cmd = parse_argument(pair.into_inner().next().unwrap());
				let cmd = Cmd::Field(cut_cmd);
				cmds.push(cmd);
			}
			Rule::move_cmd => {
				let move_cmd = parse_argument(pair.into_inner().next().unwrap());
				let cmd = Cmd::Motion(move_cmd);
				cmds.push(cmd);
			}
			Rule::next => {
				cmds.push(Cmd::BreakGroup);
			}
			Rule::echo_cmd => {
				let inner = pair.into_inner();
				let mut echo_args = vec![];
				for arg in inner {
					let arg = CmdArg::Expr(Expr::from_rule(arg));
					echo_args.push(arg);
				}
				let cmd = Cmd::Echo(echo_args);
				cmds.push(cmd);
			}
			Rule::until_block |
			Rule::while_block => {
				let is_while = pair.as_rule() == Rule::while_block;
				let mut inner = pair.into_inner();
				let expr_pair = inner.next().unwrap();
				let cond = CmdArg::Expr(Expr::from_rule(expr_pair));
				let block = inner.next().unwrap();
				let body = parse_block(block);
				let cmd = if is_while {
					Cmd::WhileBlock(CondBlock { cond, cmds: body })
				} else {
					Cmd::UntilBlock(CondBlock { cond, cmds: body })
				};
				cmds.push(cmd);
			}
			Rule::if_block => {
				let mut inner = pair.into_inner();
				let expr_pair = inner.next().unwrap();
				let cond = CmdArg::Expr(Expr::from_rule(expr_pair));
				let block = inner.next().unwrap();
				let mut cond_blocks = vec![CondBlock { cond, cmds: parse_block(block) }];
				let mut else_block = None;
				while let Some(block) = inner.next() {
					match block.as_rule() {
						Rule::elif_block => {
							let mut inner = block.into_inner();
							let expr_pair = inner.next().unwrap();
							let cond = CmdArg::Expr(Expr::from_rule(expr_pair));
							let block = inner.next().unwrap();
							cond_blocks.push(CondBlock { cond, cmds: parse_block(block) });
						}
						Rule::else_block => {
							let block = block.into_inner().next().unwrap();
							let else_cmds = parse_block(block);
							else_block = Some(else_cmds);
						}
						_ => unreachable!("Unexpected rule in if_block: {:?}", block.as_rule()),
					}
				}

				let cmd = Cmd::IfBlock {
					cond_blocks,
					else_block,
				};
				cmds.push(cmd);
			}
			Rule::var_add |
			Rule::var_sub |
			Rule::var_mult|
			Rule::var_div |
			Rule::var_mod |
			Rule::var_pow |
			Rule::var_declare => {
				let cmd = parse_var_cmd(pair);
				cmds.push(cmd);
			}
			_ => unreachable!("Unexpected rule in cmd: {:?}", pair.as_rule()),
		}
	}
}

fn parse_block(pair: Pair<Rule>) -> Vec<Cmd> {
	let mut cmds = vec![];
	for cmd in pair.into_inner() {
		parse_cmd(&mut cmds, cmd);
	}
	cmds
}

fn parse_var_cmd(pair: Pair<Rule>) -> Cmd {
	match pair.as_rule() {
		Rule::var_add |
		Rule::var_sub |
		Rule::var_mult|
		Rule::var_div |
		Rule::var_mod |
		Rule::var_pow => {
			let op = match pair.as_rule() {
				Rule::var_add => BinOp::Add,
				Rule::var_sub => BinOp::Sub,
				Rule::var_mult => BinOp::Mult,
				Rule::var_div => BinOp::Div,
				Rule::var_mod => BinOp::Mod,
				Rule::var_pow => BinOp::Pow,
				_ => unreachable!("Unexpected rule in var_cmd: {:?}", pair.as_rule()),
			};
			let mut inner = pair.into_inner();
			let name = inner.next().unwrap().as_str().to_string();
			let expr_pair = inner.next().unwrap();
			let exp = Expr::from_rule(expr_pair);
			Cmd::MutateVar { name, op, value: CmdArg::Expr(exp) }
		}
		Rule::var_declare => {
			let mut inner = pair.into_inner();
			let name = inner.next().unwrap().as_str().to_string();
			let expr_pair = inner.next().unwrap();
			let exp = Expr::from_rule(expr_pair);
			Cmd::VarDec { name, value: CmdArg::Expr(exp) }
		}
		_ => unreachable!("Unexpected rule in var_cmd: {:?}", pair.as_rule()),
	}
}

fn parse_global(pair: Pair<Rule>, polarity: bool) -> Cmd {
	let mut inner = pair.into_inner();
	let pattern = parse_argument(inner.next().unwrap());
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

fn parse_repeat(pair: Pair<Rule>) -> Cmd {
	let mut body = vec![];
	let mut inner = pair.into_inner();
	let repeat_count = parse_count(inner.next().unwrap());

	let block = inner.next().unwrap().into_inner();
	for cmd in block {
		parse_cmd(&mut body, cmd);
	}

	Cmd::Repeat{ body, count: repeat_count }
}

fn parse_count(pair: Pair<Rule>) -> CmdArg {
	match pair.as_rule() {
		Rule::int => {
			let count = pair.into_inner().next().unwrap();
			CmdArg::Count(count.as_str().parse::<usize>().unwrap())
		}
		Rule::var => {
			let var = pair.into_inner().next().unwrap();
			CmdArg::Var(var.as_str().to_string())
		}
		_ => unreachable!("Unexpected rule in count: {:?}", pair.as_rule()),
	}
}

fn parse_argument(pair: Pair<Rule>) -> CmdArg {
	let pair = pair.into_inner().next().unwrap();
	match pair.as_rule() {
		Rule::var => {
			let var = pair.into_inner().next().unwrap();
			CmdArg::Var(var.as_str().to_string())
		}
		Rule::literal => {
			let literal = pair.into_inner().next().unwrap();
			CmdArg::Literal(Val::Str(literal.as_str().to_string()))
		}
		_ => unreachable!("Unexpected rule in argument: {:?}", pair.as_rule()),
	}
}
