//! This module contains the parsing logic for the `vic` scripting language.
//! `vic` is a simple language that allows `vicut` users to more easily write complex command strings.

use std::{fmt::Display, path::PathBuf};

use pest::{iterators::Pair, Parser};
use pest_derive::Parser;
use crate::{complain_and_exit, exec::{Val, ViCut}, register::{read_register, RegisterContent}, CondBlock, ExecCtx, Opts};

use super::Cmd;

#[derive(Debug, PartialEq, Clone)]
pub enum CmdArg {
	Null,
	Literal(Val),
	Var(String),
	Count(usize),
	Expr(Expr),
}

impl CmdArg {
	pub fn is_truthy(&self, vicut: &mut ViCut, ctx: &mut ExecCtx) -> bool {
		match self {
			CmdArg::Null => false,
			CmdArg::Var(var) => {
				let Some(val) = vicut.get_var(var) else {
					return false
				};
				val.is_truthy()
			}
			CmdArg::Literal(lit) => lit.is_truthy(),
			CmdArg::Expr(expr) => expr.is_truthy(vicut,ctx),
			CmdArg::Count(count) => *count > 0
		}
	}
	pub fn display_type(&self) -> String {
		match self {
			CmdArg::Null => String::from("null"),
			CmdArg::Literal(lit) => lit.display_type(),
			CmdArg::Var(var) => format!("${var}"),
			CmdArg::Count(count) => format!("{count}"),
			CmdArg::Expr(expr) => expr.display_type(),
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub enum BinOp {
	Equals,
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
#[derive(Debug, Clone,PartialEq)]
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
	Not,
	Null
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
			BoolOp::Not => write!(f, "!"),
			BoolOp::Null => write!(f, ""),
		}
	}
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
	Var(String),
	VarIndex(String,Box<Expr>), // for array indexing
	Register(char),
	Int(i64),
	Bool(bool),
	Literal(String),
	Regex(String),
	Pop(String), // pop a value from a stack
	Range(Box<Expr>,Box<Expr>), // 1..10
	RangeInclusive(Box<Expr>,Box<Expr>), // 1..=10
	Null,
	Array(Vec<Expr>), // for array literals
	Return(String), // extract via vim command
	FuncCall(String,Vec<Expr>), // function call with arguments
	TernaryExp {
		cond: (bool,Box<Expr>),
		true_case: Box<Expr>,
		false_case: Box<Expr>,
	},
	BoolExp {
		op: BoolOp,
		left: (bool,Box<Expr>), // (is negated, expr)
		right: Option<(bool,Box<Expr>)>, // (is negated, expr)
	},
	BinExp {
		op: BinOp,
		left: Box<Expr>,
		right: Box<Expr>,
	}
}

impl Expr {
	pub fn is_truthy(&self, vicut: &mut ViCut, ctx: &mut ExecCtx) -> bool {
		match self {
			Expr::Regex(_) => true,
			Expr::Pop(stack_name) => {
				let Some(val) = vicut.get_var(stack_name) else {
					eprintln!("vicut: stack '{stack_name}' not found for pop command");
					std::process::exit(1);
				};
				val.is_truthy()
			}
			Expr::RangeInclusive(start, end) |
			Expr::Range(start, end) => {
				let start = start.is_truthy(vicut,ctx);
				let end = end.is_truthy(vicut,ctx);
				start && end
			}
			Expr::Register(reg) => {
				read_register(Some(*reg)).is_some_and(|val| !matches!(val, RegisterContent::Empty))
			}
			Expr::Null => {
				false
			}
			Expr::VarIndex(var, index) => {
				let val = vicut.eval_expr(index, ctx);
				let Val::Num(index) = val.unwrap_or_else(complain_and_exit) else {
					eprintln!("vicut: index must be an integer");
					std::process::exit(1);
				};
				let index = index as usize;
				let val = vicut.read_index_var(var.to_string(), index).unwrap_or_else(complain_and_exit);
				val.is_truthy()
			}
			Expr::Var(var) => {
				let Some(val) = vicut.get_var(var) else {
					eprintln!("vicut: variable '{var}' not found");
					std::process::exit(1)
				};
				val.is_truthy()
			}
			Expr::Array(arr) => {
				!arr.is_empty() // an empty array is falsy
			}
			Expr::Int(int) => {
				Val::Num(*int as isize).is_truthy()
			}
			Expr::Bool(bool) => {
				Val::Bool(*bool).is_truthy()
			}
			Expr::Literal(lit) => {
				!lit.is_empty()
			}
			Expr::FuncCall(name, args) => {
				let args = args.iter()
					.map(|arg| vicut.eval_expr(arg, ctx))
					.collect::<Result<Vec<Val>, String>>();
				let args = args.unwrap_or_else(complain_and_exit);
				let ret = vicut.eval_function(name.to_string(), args, ctx);
				let Ok(ret) = ret else {
					// The function call failed
					// so we return false.
					return false;
				};
				if let Val::Null = ret {
					// The function succeeded, but returned nothing
					// We treat it as truthy based on the call succeeding
					return true;
				}
				// If the function returns a value, we check if it is truthy
				ret.is_truthy()
			}
			Expr::Return(cmd) => {
				!cmd.is_empty()
			}
			Expr::TernaryExp { cond, true_case, false_case } => {
				let (is_negated, cond) = cond;
				let cond = if *is_negated {
					!cond.is_truthy(vicut,ctx)
				} else {
					cond.is_truthy(vicut,ctx)
				};
				if cond {
					true_case.is_truthy(vicut,ctx)
				} else {
					false_case.is_truthy(vicut,ctx)
				}
			}
			Expr::BoolExp { op, left, right } => {
				vicut.eval_bool_expr(op, left, right.as_ref(),ctx).unwrap_or_else(complain_and_exit).is_truthy()
			}
			Expr::BinExp { op, left, right } => {
				vicut.eval_bin_expr(op, left, right).unwrap_or_else(complain_and_exit).is_truthy()
			}
		}
	}
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
		let inner = if matches!(pair.as_rule(), Rule::func_call | Rule::var_ident | Rule::range | Rule::range_inclusive | Rule::bin_expr | Rule::bool_expr_single | Rule::bool_expr | Rule::int | Rule::null) {
			pair
		} else {
			let rule = format!("{:?}",pair.as_rule());
			let inner_check = pair.clone();
			if inner_check.into_inner().next().is_none() {
				pair
			} else {
				pair.into_inner().next().unwrap_or_else(|| panic!("Unwrapped nothing on rule: {rule}"))
			}
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
			Rule::null => {
				Self::Null
			}
			Rule::var => {
				let var = inner.into_inner().next().unwrap();
				Self::from_rule(var)
			}
			Rule::var_name => {
				let var = inner.into_inner().next().unwrap();
				Self::from_rule(var)
			}
			Rule::var_index => {
				let mut inner = inner.into_inner();
				let var_name = inner.next().unwrap()
					.as_str().to_string();
				let index = inner.next().unwrap();
				let index = Self::from_rule(index);
				Self::VarIndex(var_name,Box::new(index))
			}
			Rule::var_ident => {
				let var = inner.as_str().to_string();
				Self::Var(var)
			}
			Rule::range_inclusive |
			Rule::range => {
				let is_inclusive = matches!(inner.as_rule(), Rule::range_inclusive);
				let mut inner = inner.into_inner();
				let start = inner.next().unwrap();
				let start = Self::from_rule(start);
				let end = inner.next().unwrap();
				let end = Self::from_rule(end);
				if is_inclusive {
					Self::RangeInclusive(Box::new(start),Box::new(end))
				} else {
					Self::Range(Box::new(start),Box::new(end))
				}
			}
			Rule::register => {
				let reg = inner.into_inner().next().unwrap()
					.as_str().chars().next().unwrap();
				Self::Register(reg)
			}
			Rule::array => {
				let inner = inner.into_inner();
				let mut elements = vec![];
				for elem in inner {
					elements.push(Self::from_rule(elem));
				}
				Self::Array(elements)
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
			Rule::true_lit => {
				Self::Bool(true)
			}
			Rule::false_lit => {
				Self::Bool(false)
			}
			Rule::pop_cmd => {
				let stack_name = inner.into_inner().next().unwrap()
					.into_inner().next().unwrap()
					.as_str().to_string();
				Self::Pop(stack_name)
			}
			Rule::bin_atom => {
				let inner = inner.into_inner().next().unwrap();
				Self::from_rule(inner)
			}
			Rule::bin_lit => {
				let int = inner.as_str().parse::<i64>().unwrap();
				Self::Int(int)
			}
			Rule::regex_lit => {
				let regex = inner.into_inner().next().unwrap()
					.as_str().to_string();
				Self::Regex(regex)
			}
			Rule::func_call => {
				let mut inner = inner.into_inner();
				let func_name = inner.next().unwrap().as_str().to_string();
				let mut args = vec![];
				for arg in inner {
					args.push(Self::from_rule(arg));
				}
				Self::FuncCall(func_name,args) // TODO: handle function calls properly
			}
			Rule::ternary => {
				let mut inner = inner.into_inner();
				let cond_pair = inner.next().unwrap()
					.into_inner().next().unwrap();
				let true_case_pair = inner.next().unwrap()
					.into_inner().next().unwrap();
				let false_case_pair = inner.next().unwrap()
					.into_inner().next().unwrap();

				let cond = Self::from_rule(cond_pair);
				let true_case = Self::from_rule(true_case_pair);
				let false_case = Self::from_rule(false_case_pair);

				Self::TernaryExp {
					cond: (false,Box::new(cond)),
					true_case: Box::new(true_case),
					false_case: Box::new(false_case),
				}
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
				let mut left_pair = expr.next().unwrap();
				let mut left_negated = false;
				if let Rule::not = left_pair.as_rule() {
					left_pair = expr.next().unwrap();
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

					return Self::BoolExp { op, left: (left_negated,Box::new(left)), right: Some((right_negated,Box::new(right))) };
				}
				Self::BoolExp { op: BoolOp::Null, left: (left_negated,Box::new(left)), right: None }
			}
			Rule::bool_expr => {
				let mut expr = inner.into_inner();
				let mut left_expr = expr.next().unwrap();
				let mut left_negated = false;
				if let Rule::not = left_expr.as_rule() {
					left_expr = expr.next().unwrap();
					left_negated = true;
				}
				let mut left = Self::from_rule(left_expr);
				let mut right_expressions = vec![];
				while let Some(op_pair) = expr.next() {
					let op = BoolOp::bool_op_from_rule(op_pair);
					let mut right_pair = expr.next().unwrap().into_inner();
					let mut right_expr = right_pair.next().unwrap();
					let mut right_negated = false;
					if let Rule::not = right_expr.as_rule() {
						right_expr = right_pair.next().unwrap();
						right_negated = true;
					}
					let right = Self::from_rule(right_expr);
					right_expressions.push((op,right_negated,Box::new(right)));
				}

				for expr in right_expressions {
					let (op, right_negated, right) = expr;
					left = Self::BoolExp {
						op,
						left: (left_negated, Box::new(left)),
						right: Some((right_negated, right)),
					};
				}
				left
			}
			Rule::expr => {
				// We didn't unwrap this before getting here
				// So let's descend further
				let inner = inner.into_inner().next().unwrap();
				Self::from_rule(inner)
			}
			_ => unreachable!("Unexpected rule in expr: {:?}", inner.as_rule()),
		}
	}
	pub fn display_type(&self) -> String {
		match self {
			Expr::VarIndex(_,_) |
			Expr::Var(_) => String::from("var"),
			Expr::Pop(_) => String::from("pop"),
			Expr::Regex(_) => String::from("regex"),
			Expr::Range(_,_) => String::from("range"),
			Expr::RangeInclusive(_,_) => String::from("range_inclusive"),
			Expr::Null => String::from("null"),
			Expr::Register(_) => String::from("register"),
			Expr::Int(_) => String::from("int"),
			Expr::Bool(_) => String::from("bool"),
			Expr::Array(_) => String::from("array"),
			Expr::Literal(_) => String::from("string"),
			Expr::FuncCall(_, _) => String::from("function_call"),
			Expr::Return(_) => String::from("return_cmd"),
			Expr::TernaryExp {..} => String::from("ternary"),
			Expr::BoolExp {..} => String::from("bool_expr"),
			Expr::BinExp {..} => String::from("binary_expr"),
		}
	}
}

impl Display for BinOp {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			BinOp::Equals => write!(f, "="),
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
			Expr::Pop(stack_name) => {
				write!(f, "pop {stack_name}")
			}
			Expr::RangeInclusive(start, end) => {
				write!(f, "{}..={}", start, end)
			}
			Expr::Regex(regex) => {
				write!(f, "{regex}")
			}
			Expr::Range(start, end) => {
				write!(f, "{}..{}", start, end)
			}
			Expr::Null => write!(f, "NULL"),
			Expr::Var(var) => write!(f, "${var}"),
			Expr::VarIndex(var, index) => {
				write!(f, "${var}[{}]", index)
			}
			Expr::Register(reg) => write!(f, "@{reg}"),
			Expr::Int(int) => write!(f, "{int}"),
			Expr::Literal(lit) => write!(f, "{lit}"),
			Expr::FuncCall(name, args) => {
				let args_str: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
				write!(f, "{name}({})", args_str.join(", "))
			}
			Expr::Return(lit) => write!(f, "{lit}"),
			Expr::Bool(bool) => write!(f, "{bool}"),
			Expr::Array(arr) => {
				let elements: Vec<String> = arr.iter().map(|e| e.to_string()).collect();
				write!(f, "[{}]", elements.join(", "))
			}
			Expr::TernaryExp { cond, true_case, false_case } => {
				let (is_negated, cond) = cond;
				let cond_display = if *is_negated {
					format!("!{cond}")
				} else {
					cond.to_string()
				};
				write!(f, "({cond_display} ? {true_case} : {false_case})")
			}
			Expr::BoolExp { op, left, right } => {
				let (left_negated, left) = left;
				if let Some((right_negated, right)) = right {
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
				} else {
					let left_display = if *left_negated {
						format!("!{left}")
					} else {
						left.to_string()
					};
					write!(f, "({left_display} {op})")
				}
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
			CmdArg::Null => write!(f, "NULL"),
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
			Rule::EOI => {
				// End of input
			}
			_ => unreachable!("Unexpected rule in vic: {:?}", pair.as_rule()),
		}
	}
	Ok(opts)
}

fn parse_cmd(cmds: &mut Vec<Cmd>, pair: Pair<Rule>) {
	for pair in pair.into_inner() {
		match pair.as_rule() {
			Rule::include => {
				let include_cmds = include_cmd(pair);
				cmds.extend(include_cmds);
			}
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
			Rule::push_cmd => {
				let mut inner = pair.into_inner();
				let stack_name = inner.next().unwrap()
					.into_inner().next().unwrap()
					.into_inner().next().unwrap()
					.as_str().to_string();
				let expr = inner.next().unwrap();
				let expr = Expr::from_rule(expr);
				let cmd = Cmd::Push(CmdArg::Var(stack_name), CmdArg::Expr(expr));
				cmds.push(cmd);
			}
			Rule::pop_cmd => {
				let mut inner = pair.into_inner();
				let stack_name = inner.next().unwrap()
					.into_inner().next().unwrap()
					.into_inner().next().unwrap()
					.as_str().to_string();
				let cmd = Cmd::Pop(CmdArg::Var(stack_name));
				cmds.push(cmd);
			}
			Rule::return_cmd => {
				if let Some(cmd) = pair.into_inner().next() {
					let return_cmd = Expr::from_rule(cmd);
					let cmd = Cmd::Return(CmdArg::Expr(return_cmd));
					cmds.push(cmd);
				} else {
					// If there is no return command, we just push an empty return
					cmds.push(Cmd::Return(CmdArg::Null));
				}
			}
			Rule::yank_cmd => {
				let mut inner = pair.into_inner();
				let register = inner.next().unwrap()
					.into_inner().next().unwrap()
					.as_str().chars().next().unwrap();
				let expr = inner.next().unwrap();
				let expr = Expr::from_rule(expr);
				let cmd = Cmd::Yank(CmdArg::Expr(expr), register);
				cmds.push(cmd);
			}
			Rule::next => {
				cmds.push(Cmd::BreakGroup);
			}
			Rule::break_loop => {
				cmds.push(Cmd::LoopBreak);
			}
			Rule::continue_loop => {
				cmds.push(Cmd::LoopContinue);
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
			Rule::func_def => {
				let mut inner = pair.into_inner();
				let mut args = vec![];
				let mut name_and_args = inner.next().unwrap().into_inner();
				let name = name_and_args.next().unwrap().as_str().to_string();
				let arg_list = name_and_args.next().unwrap().into_inner();
				for arg in arg_list {
					let arg = arg.as_str().to_string();
					args.push(arg);
				}

				let block = inner.next().unwrap();
				let body = parse_block(block);
				let cmd = Cmd::FuncDef { name, args, body };
				cmds.push(cmd);
			}
			Rule::for_block => {
				let mut inner = pair.into_inner();
				let var_pair = inner.next().unwrap();
				let var_name = var_pair.as_str().to_string();
				let range_pair = inner.next().unwrap();
				let iterable = Expr::from_rule(range_pair);
				let iterable = CmdArg::Expr(iterable);
				let block = inner.next().unwrap();
				let body = parse_block(block);
				let cmd = Cmd::ForBlock { var_name, iterable, body };
				cmds.push(cmd);
			}
			Rule::func_call => {
				let mut inner = pair.into_inner();
				let func_name = inner.next().unwrap().as_str().to_string();
				let pair_args = inner.next().unwrap().into_inner();
				let mut args = vec![];
				for arg in pair_args {
					args.push(CmdArg::Expr(Expr::from_rule(arg)));
				}
				cmds.push(Cmd::FuncCall { name: func_name, args });
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
			Rule::var_mut |
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
		Rule::var_mut |
		Rule::var_pow => {
			let op = match pair.as_rule() {
				Rule::var_mut => BinOp::Equals,
				Rule::var_add => BinOp::Add,
				Rule::var_sub => BinOp::Sub,
				Rule::var_mult => BinOp::Mult,
				Rule::var_div => BinOp::Div,
				Rule::var_mod => BinOp::Mod,
				Rule::var_pow => BinOp::Pow,
				_ => unreachable!("Unexpected rule in var_cmd: {:?}", pair.as_rule()),
			};
			let mut inner = pair.into_inner();
			let mut name_parts = inner.next().unwrap().into_inner();
			let first_part = name_parts.next().unwrap();
			let (name, index) = match first_part.as_rule() {
				Rule::var_ident => {
					let name = first_part.as_str().to_string();
					(name, None)
				}
				Rule::var_index => {
					let mut inner = first_part.into_inner();
					let name = inner.next().unwrap().as_str().to_string();
					let index = inner.next().unwrap();
					let index = Expr::from_rule(index);
					(name, Some(CmdArg::Expr(index)))
				}
				_ => unreachable!("Unexpected rule in var_cmd: {:?}", first_part.as_rule()),

			};
			let expr_pair = inner.next().unwrap();
			let exp = Expr::from_rule(expr_pair);
			Cmd::MutateVar { name, index, op, value: CmdArg::Expr(exp) }
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
		Rule::expr => {
			let expr = Expr::from_rule(pair);
			CmdArg::Expr(expr)
		}
		_ => unreachable!("Unexpected rule in argument: {:?}", pair.as_rule()),
	}
}

fn include_cmd(pair: Pair<Rule>) -> Vec<Cmd> {
	let mut cmds = vec![];
	let mut inner = pair.into_inner();
	let file_pair = inner.next().unwrap();
	let file = file_pair.into_inner().next().unwrap()
		.as_str().to_string();

	let file_content = std::fs::read_to_string(&file)
		.unwrap_or_else(|_| panic!("vicut: error reading included file '{file}'"));

	let included_cmds = parse_vic(&file_content).unwrap_or_else(complain_and_exit);

	cmds.extend(included_cmds.cmds);
	cmds
}
