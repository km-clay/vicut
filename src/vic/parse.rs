use std::{boxed, fmt::Display, rc::Rc};

use pest::{iterators::{Pair, Pairs}, Parser};
use pest_derive::Parser;
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::{exec::ViCut, Opts};

#[derive(Parser)]
#[grammar = "vic/vic.pest"] // relative to src
pub struct VicParser;

// Have to do this weird hack or else we have to attach '::<Rule>' to every error construction callsite
/// Leverage `pest`'s pretty error reporting
pub fn expr_error(message: String, span: RcSpan) -> String {
	let span = pest::Span::new(span.as_str(), span.start(), span.end()).unwrap();
	expr_error2::<Rule>(message, span)
}
fn expr_error2<'s,R: pest::RuleType>(message: String, span: pest::Span) -> String {
	pest::error::Error::new_from_span(pest::error::ErrorVariant::<R>::CustomError { message }, span).to_string()
}

/*
 * Now we will be re-implementing pests' Pair/Pairs/Span structs
 * to use 'Rc<String>' instead of &str to reference the original input.
 * Using &str creates a multitude of problems later on once the Expr's are passed on to the ViCut struct
 * so we will be using an owned pointer instead :)
 */

#[derive(Debug, Clone, PartialEq)]
pub struct RcSpan {
	input: Rc<String>,
	start: usize,
	end: usize
}

impl RcSpan {
	pub fn new(input: Rc<String>, start: usize, end: usize) -> Self {
		Self { input, start, end }
	}
	pub fn as_str(&self) -> &str {
		&self.input[self.start..self.end]
	}
	pub fn len(&self) -> usize {
		self.end - self.start
	}
	pub fn input(&self) -> Rc<String> {
		self.input.clone()
	}
	pub fn start(&self) -> usize {
		self.start
	}
	pub fn end(&self) -> usize {
		self.end
	}
}

pub struct RcPairs {
	pairs: Vec<RcPair>,
	index: usize
}

impl Iterator for RcPairs {
	type Item = RcPair;
	fn next(&mut self) -> Option<Self::Item> {
		if self.index >= self.pairs.len() {
			return None
		}
		let item = self.pairs[self.index].clone();
		self.index += 1;
		Some(item)
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct RcPair {
	pub rule: Rule,
	pub span: RcSpan,
	pub inner: Vec<RcPair>
}

impl RcPair {
	pub fn from_root(root_pair: Pair<Rule>) -> Self {
		let input = Rc::new(root_pair.as_span().as_str().to_string());
		Self::from_pair(root_pair, input)
	}
	pub fn from_pair(pair: Pair<Rule>, input: Rc<String>) -> Self {
		let rule = pair.as_rule();
		let pair_span = pair.as_span();
		let span = RcSpan::new(Rc::clone(&input), pair_span.start(), pair_span.end());
		let inner = pair.into_inner();
		let rc_inner = Self::eval_inner(inner, Rc::clone(&input));
		Self { rule, span, inner: rc_inner }
	}
	fn as_str(&self) -> &str {
		self.span.as_str()
	}
	fn to_string(&self) -> String {
		self.span.as_str().to_string()
	}
	fn as_span(&self) -> RcSpan {
		self.span.clone()
	}
	fn as_rule(&self) -> Rule {
		self.rule.clone()
	}
	fn eval_inner(inner: Pairs<Rule>, input: Rc<String>) -> Vec<Self> {
		let mut rc_inner = vec![];
		for pair in inner {
			rc_inner.push(Self::from_pair(pair, Rc::clone(&input)));
		}
		rc_inner
	}
	pub fn into_inner(self) -> RcPairs {
		RcPairs { pairs: self.inner, index: 0 }
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
	pub value: ExprKind,
	pub index: Option<Index>,
	pub span: RcSpan
}

impl Expr {
	pub fn parse_vic(src: Rc<String>) -> Result<Self,String> {
		let mut vic = VicParser::parse(Rule::vic, &src).map_err(|e| e.to_string())?;
		let cmd_pairs = vic.next().unwrap();
		let rc_vic = RcPair::from_root(cmd_pairs);
		let span = rc_vic.as_span();
		let cmd_pairs_inner = rc_vic.into_inner();

		let mut top_level = vec![];
		for cmd in cmd_pairs_inner {
			let parsed = Self::parse_top_level(cmd)?;
			top_level.push(parsed);
		}
		Ok(Self {
			value: ExprKind::Vic(top_level),
			index: None,
			span
		})
	}
	fn parse_top_level(cmd: RcPair) -> Result<Expr,String> {
		match cmd.as_rule() {
			Rule::var_declare |
			Rule::var_add |
			Rule::var_sub |
			Rule::var_mut |
			Rule::var_mult |
			Rule::var_div |
			Rule::var_pow |
			Rule::var_mod => Self::parse_var_cmd(cmd),
			Rule::for_block => Self::parse_for_block(cmd),
			Rule::if_block => Self::parse_if_block(cmd),
			Rule::while_block => Self::parse_loop_block(cmd,true),
			Rule::until_block => Self::parse_loop_block(cmd,false),
			Rule::func_call |
			Rule::command => Self::parse_expr(cmd),
			Rule::func_def => Self::parse_func_def(cmd),
			Rule::opts => Self::parse_opts(cmd),
			_ => {
				// Not reachable. This is a common pattern in parsing Pest ASTs.
				// The rule that this match statement is derived from only contains
				// the above rules, so using unreachable!() is the best way to handle
				// unhandled rules, as there basically aren't any.
				unreachable!("Unhandled rule: {:?}", cmd.as_rule())
			}
		}
	}
	fn parse_var_cmd(cmd: RcPair) -> Result<Self,String> {
		let span = cmd.as_span();
		let rule = cmd.as_rule();
		let mut inner = cmd.into_inner();

		let value = if rule == Rule::var_declare {
			let name = inner.next().unwrap().as_str().to_string();
			let expr = inner.next().unwrap();
			let value = Box::new(Self::parse_expr(expr)?);

			ExprKind::VarDec { name, value }
		} else {
			let name = inner.next().unwrap().as_str().to_string();
			let op = BinOp::from_rule(rule);
			let value = Box::new(Self::parse_expr(inner.next().unwrap())?);

			ExprKind::VarMut { name, op, value }
		};

		Ok(Self { value, index: None, span })
	}
	fn parse_func_def(cmd: RcPair) -> Result<Self,String> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();

		let name = inner.next().unwrap().as_str().to_string();
		let params_pair = inner.next().unwrap();
		let mut params = vec![];
		for param in params_pair.into_inner() {
			params.push(param.as_str().to_string());
		}
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = ExprKind::FuncDef { name, params, body };
		Ok(Self { value, index: None, span })
	}	
	fn parse_for_block(cmd: RcPair) -> Result<Self,String> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();

		let var_name = inner.next().unwrap().as_str().to_string();
		let list = Box::new(Self::parse_expr(inner.next().unwrap())?);
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = ExprKind::ForBlock { var_name, list, body };
		Ok(Self { value, index: None, span })
	}
	fn cond_block(span: RcSpan, cond: Expr, body: Vec<Expr>) -> Self {
		Self {
			value: ExprKind::CondBlock { cond: Box::new(cond), body },
			index: None,
			span
		}
	}
	fn parse_if_block(cmd: RcPair) -> Result<Self,String> {
		let mut cond_blocks = vec![];
		let mut else_block = None;
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let cond = Self::parse_expr(inner.next().unwrap().into_inner().next().unwrap())?;
		let block = Self::parse_block(inner.next().unwrap());
		cond_blocks.push(Self::cond_block(cond.span.clone(), cond, block?));

		while let Some(pair) = inner.next() {
			match pair.as_rule() {
				Rule::elif_block => {
					let mut elif_inner = pair.into_inner();
					let cond = Self::parse_expr(elif_inner.next().unwrap().into_inner().next().unwrap())?;
					let block = Self::parse_block(elif_inner.next().unwrap());
					cond_blocks.push(Self::cond_block(cond.span.clone(), cond, block?));
				}
				Rule::else_block => {
					let else_inner = pair.into_inner().next().unwrap();
					else_block = Some(Self::parse_block(else_inner)?);
				}
				_ => unreachable!()
			}
		}
		Ok(Self {
			value: ExprKind::IfBlock { cond_blocks, else_block },
			index: None,
			span
		})
	}
	fn parse_loop_block(cmd: RcPair, polarity: bool) -> Result<Self,String> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let cond = Self::parse_expr(inner.next().unwrap())?;
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = if polarity {
			ExprKind::WhileBlock { cond: Box::new(cond), body }
		} else {
			ExprKind::UntilBlock { cond: Box::new(cond), body }
		};
		Ok(Self { value, index: None, span })
	}
	fn parse_block(block: RcPair) -> Result<Vec<Self>,String> {
		let mut cmds = vec![];
		let inner = block.into_inner();
		for cmd in inner {
			cmds.push(Self::parse_top_level(cmd)?)
		}
		Ok(cmds)
	}
	fn parse_expr(expr: RcPair) -> Result<Expr,String> {
		let inner_expr = expr.into_inner().next().unwrap();

		match inner_expr.as_rule() {
			Rule::value => Self::parse_value(inner_expr),
			Rule::command => Self::parse_command(inner_expr),
			_ => unreachable!()
		}
	}
	fn parse_command(cmd: RcPair) -> Result<Expr,String> {
		let cmd_raw = cmd.as_str().split(" ").next().unwrap();

		match cmd_raw {
			"next" => {
				let span = cmd.as_span();
				Ok(Self {
					value: ExprKind::Command(Command::Next),
					index: None,
					span
				})
			}
			"shell" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let cmd_expr = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::ShellCmd { cmd: cmd_expr }),
					index: None,
					span
				})
			}
			"continue" => {
				let span = cmd.as_span();
				Ok(Self {
					value: ExprKind::Command(Command::Continue),
					index: None,
					span
				})
			}
			"break" => {
				let span = cmd.as_span();
				Ok(Self {
					value: ExprKind::Command(Command::Break),
					index: None,
					span
				})
			}
			"global" | "g" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let pattern = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let block = Self::parse_block(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::Global { pattern, block }),
					index: None,
					span
				})
			}
			"not_global" | "!global" | "v" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let pattern = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let block = Self::parse_block(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::NotGlobal { pattern, block }),
					index: None,
					span
				})
			}
			"move" | "m" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Move { motion }),
					index: None,
					span
				})
			}
			"cut" | "c" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Cut { motion }),
					index: None,
					span
				})
			}
			"echo" => {
				let span = cmd.as_span();
				let inner = cmd.into_inner();
				let mut args = vec![];
				for arg in inner {
					args.push(Self::parse_expr(arg)?);
				}
				Ok(Self {
					value: ExprKind::Command(Command::Echo { args }),
					index: None,
					span
				})
			}
			"repeat" | "r" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let count = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let block = Self::parse_block(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::Repeat { count, block }),
					index: None,
					span
				})
			}
			"yank" | "y" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let register_pair = inner.next().unwrap();
				let register = Box::new(Self::parse_expr(register_pair)?);
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Yank { register, motion }),
					index: None,
					span
				})
			}
			"push" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let stack = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let value = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Push { stack, value }),
					index: None,
					span
				})
			}
			"pop" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let stack = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Pop { stack }),
					index: None,
					span
				})
			}
			"buf" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				match inner.next().unwrap().as_rule() {
					Rule::buf_switch => {
						let id = Box::new(Self::parse_expr(inner.next().unwrap())?);
						Ok(Self {
							value: ExprKind::Command(Command::BufSwitch { id }),
							index: None,
							span
						})
					}
					Rule::buf_id => {
						Ok(Self {
							value: ExprKind::Command(Command::BufId),
							index: None,
							span
						})
					}
					_ => unreachable!()
				}
			}
			"return" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let ret = if let Some(ret_expr) = inner.next() {
					Some(Box::new(Self::parse_expr(ret_expr)?))
				} else {
					None
				};
				Ok(Self {
					value: ExprKind::Command(Command::Return { ret }),
					index: None,
					span
				})
			}
			"include" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let path = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Include { path }),
					index: None,
					span
				})
			}
			_ => Err(expr_error(format!("Unknown command: {cmd_raw}"), cmd.as_span()))
		}
	}
	fn parse_opts(opts: RcPair) -> Result<Expr,String> {
		let span = opts.as_span();
		let mut parsed_opts = vec![];
		for opt in opts.into_inner() {
			let span = opt.as_span();
			let mut arg = None;
			let opt = opt.into_inner().next().unwrap();
			let name = match opt.as_rule() {
				Rule::backup_ext => "backup_ext",
				Rule::template => "template",
				Rule::delimiter => "delimiter",
				Rule::file => "file",
				Rule::files => "files",
				Rule::pipe_in => "pipe_in",
				Rule::pipe_out => "pipe_out",
				Rule::write => "write",
				Rule::max_jobs => "max_jobs",
				Rule::trace => "trace",
				Rule::json => "json",
				Rule::linewise => "linewise",
				Rule::serial => "serial",
				Rule::trim_fields => "trim_fields",
				Rule::keep_mode => "keep_mode",
				Rule::backup => "backup",
				Rule::edit_inplace => "edit_inplace",
				Rule::silent => "silent",
				Rule::no_input => "no_input",
				Rule::global_uses_line_numbers => "global_uses_line_numbers",
				_ => unreachable!()
			};
			if let Some(pair) = opt.into_inner().next() {
				arg = Some(Box::new(Self::parse_value(pair)?));
			}
			parsed_opts.push(Self {
				value: ExprKind::Opt { name: name.to_string(), arg },
				index: None,
				span
			});
		}
		Ok(Self{
			value: ExprKind::Opts(parsed_opts),
			index: None,
			span
		})
	}
	fn parse_value(value: RcPair) -> Result<Expr,String> {
		let span = value.as_span();
		let value_kind = value.into_inner().next().unwrap();
		let value = Val::try_from_pair(value_kind).map_err(|e| expr_error(e,span.clone()))?;
		Ok(Self {
			value: ExprKind::Value(value),
			index: None,
			span
		})
	}
	pub fn is_break(&self) -> bool {
		self.value == ExprKind::Command(Command::Break)
	}
	pub fn is_continue(&self) -> bool {
		self.value == ExprKind::Command(Command::Continue)
	}
	pub fn is_return(&self) -> bool {
		matches!(self.value, ExprKind::Command(Command::Return { .. }))
	}
	pub fn span(&self) -> RcSpan {
		self.span.clone()
	}
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
	Vic(Vec<Expr>), // Root node of AST
	TopLevel(Box<Expr>),
	Block(Vec<Expr>),
	Value(Val),
	Command(Command),
	Opts(Vec<Expr>), // Always contains 'ExprKind::Opt'
	Opt { name: String, arg: Option<Box<Expr>> },
	VarDec { name: String, value: Box<Expr> },
	VarMut { name: String, op: Option<BinOp>, value: Box<Expr> },
	CondBlock { cond: Box<Expr>, body: Vec<Expr> },
	ForBlock { var_name: String, list: Box<Expr>, body: Vec<Expr> },
	IfBlock { cond_blocks: Vec<Expr>, else_block: Option<Vec<Expr>>, },
	WhileBlock { cond: Box<Expr>, body: Vec<Expr> },
	UntilBlock { cond: Box<Expr>, body: Vec<Expr> },
	Range { start: Box<Expr>, end: Box<Expr> },
	BinaryExpr { left: Box<Expr>, op: BinOp, right: Box<Expr> },
	BoolExpr { left: Box<Expr>, op: BinOp, right: Box<Expr> },
	FuncCall { name: Box<Expr>, args: Vec<Expr> },
	FuncDef { name: String, params: Vec<String>, body: Vec<Expr> },
}

impl ExprKind {
	pub fn string(str: impl ToString) -> Self {
		Self::Value(Val::Str(str.to_string()))
	}
}

#[derive(Debug,Clone,PartialEq)]
pub enum Command {
	Next,
	Continue,
	Break,
	BufId,
	Return { ret: Option<Box<Expr>> },
	Global { pattern: Box<Expr>, block: Vec<Expr>, },
	NotGlobal { pattern: Box<Expr>, block: Vec<Expr>, },
	Move { motion: Box<Expr> },
	Cut { motion: Box<Expr> },
	Echo { args: Vec<Expr> },
	Repeat { count: Box<Expr>, block: Vec<Expr> },
	Yank { register: Box<Expr>, motion: Box<Expr> },
	ShellCmd { cmd: Box<Expr> },
	Push { stack: Box<Expr>, value: Box<Expr> },
	Pop { stack: Box<Expr> },
	BufSwitch { id: Box<Expr> },
	Include { path: Box<Expr> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Index {
	Single(usize),
	To(usize),
	ToInc(usize),
	From(usize),
	Slice(usize,usize),
	SliceInc(usize,usize),
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
	pub fn from_rule(rule: Rule) -> Option<Self> {
		match rule {
			Rule::var_add => Some(Self::Add),
			Rule::var_sub => Some(Self::Sub),
			Rule::var_mult => Some(Self::Mult),
			Rule::var_div => Some(Self::Div),
			Rule::var_mod => Some(Self::Mod),
			Rule::var_pow => Some(Self::Pow),
			_ => None
		}
	}
}

pub enum BoolOp {
	Ne,
	Eq,
	Lt,
	Gt,
	Lte,
	Gte,
	Not,
	And,
	Or,
}

impl BoolOp {
	pub fn from_rule(rule: Rule) -> Option<Self> {
		match rule {
			Rule::ne => Some(Self::Ne),
			Rule::eq => Some(Self::Eq),
			Rule::lt => Some(Self::Lt),
			Rule::gt => Some(Self::Gt),
			Rule::le => Some(Self::Lte),
			Rule::ge => Some(Self::Gte),
			Rule::not => Some(Self::Not),
			Rule::and => Some(Self::And),
			Rule::or => Some(Self::Or),
			_ => None
		}
	}
}

// Evaluated expressions
#[derive(Default, Debug, Clone)]
pub enum Val {
	#[default]
	Null,
	Str(String),
	Var(String),
	Arr(Vec<Val>),
	Num(isize),
	Closure(Vec<String>, Vec<Expr>),
	Bool(bool),
	Regex(Regex),
}

impl Val {
	pub fn is_compound(&self) -> bool {
		matches!(self, Self::Arr(_) | Self::Str(_))
	}
	pub fn try_iter(&self) -> Result<impl Iterator, String> {
		match self {
			Self::Arr(arr) => Ok(arr.clone().into_iter()),
			Self::Str(s) => {
				let graphemes = s.graphemes(true).map(|g| Val::Str(g.to_string())).collect::<Vec<_>>();
				Ok(graphemes.into_iter())
			}
			_ => Err(format!("Value of type '{}' is not iterable", self.display_type()))
		}
	}
	pub fn try_into_iter(self) -> Result<impl Iterator<Item=Val>, String> {
		match self {
			Self::Arr(arr) => Ok(arr.into_iter()),
			Self::Str(s) => {
				let graphemes = s.graphemes(true).map(|g| Val::Str(g.to_string())).collect::<Vec<_>>();
				Ok(graphemes.into_iter())
			}
			_ => Err(format!("Value of type '{}' is not iterable", self.display_type()))
		}
	}
	pub fn try_from_pair(pair: RcPair) -> Result<Self,String> {
		match pair.as_rule() {
			Rule::array => {
				let mut elements = vec![];
				let elem_list = pair.into_inner().next().unwrap().into_inner();
				for elem in elem_list {
					elements.push(Self::try_from_pair(elem)?);
				}
				Ok(Self::Arr(elements))
			}
			Rule::null => Ok(Self::Null),
			Rule::int => {
				let int = pair.as_str().parse::<isize>().unwrap();
				Ok(Self::Num(int))
			}
			Rule::str_literal => {
				let text = pair.into_inner().next().unwrap().as_str().to_string();
				Ok(Self::Str(text))
			}
			Rule::var => {
				let var_name = pair.as_str().to_string();
				Ok(Self::Var(var_name))
			}
			Rule::closure => {
				let mut inner = pair.into_inner();
				let mut closure_args = vec![];
				let closure_arg_pairs = inner.next().unwrap().into_inner()
					.next().unwrap().into_inner();

				for arg in closure_arg_pairs {
					closure_args.push(arg.as_str().to_string());
				}

				let block = inner.next().unwrap();
				let parsed = Expr::parse_block(block)?;
				Ok(Self::Closure(closure_args, parsed))
			}
			Rule::bool => {
				let boolean = pair.as_str().parse::<bool>().unwrap();
				Ok(Self::Bool(boolean))
			}
			Rule::register => {
				let reg_name = pair.into_inner().next().unwrap().as_str().to_string();
				Ok(Self::Var(reg_name))
			}
			Rule::regex => {
				let regex_raw = pair.into_inner().next().unwrap().to_string();
				let regex = Regex::new(&regex_raw).map_err(|e| format!("Invalid regex: {e}"))?;
				Ok(Self::Regex(regex))
			}
			_ => unreachable!()
		}
	}
	pub fn display_type(&self) -> String {
		match self {
			Self::Str(_) => "string".to_string(),
			Self::Num(_) => "number".to_string(),
			Self::Var(_) => "variable".to_string(),
			Self::Closure(_,_) => "closure".to_string(),
			Self::Arr(_) => "array".to_string(),
			Self::Bool(_) => "boolean".to_string(),
			Self::Regex(_) => "regex".to_string(),
			Self::Null => "null".to_string()
		}
	}
	pub fn is_truthy(&self, vicut: &ViCut) -> bool {
		match self {
			Self::Str(s) => !s.is_empty(),
			Self::Num(n) => *n != 0,
			Self::Var(v) => todo!(),
			Self::Closure(args, body) => todo!(),
			Self::Arr(arr) => !arr.is_empty(),
			Self::Bool(b) => *b,
			Self::Null => false,

			// Weird case. The regex compiled successfully, so we consider it truthy
			Self::Regex(_) => true,
		}
	}
}

impl PartialEq for Val {
	fn eq(&self, other: &Self) -> bool {
		match (self, other) {
			(Val::Str(s1), Val::Str(s2)) => s1 == s2,
			(Val::Arr(a1), Val::Arr(a2)) => a1 == a2,
			(Val::Num(n1), Val::Num(n2)) => n1 == n2,
			(Val::Bool(b1), Val::Bool(b2)) => b1 == b2,
			(Val::Null, Val::Null) => true,
			(Val::Regex(r1), Val::Regex(r2)) => r1.as_str() == r2.as_str(),
			(Val::Regex(r1), val) => {
				let val_str = val.to_string();
				r1.is_match(&val_str)
			}
			(val, Val::Regex(r2)) => {
				let val_str = val.to_string();
				r2.is_match(&val_str)
			}
			_ => false
		}
	}
}

impl Display for Val {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Arr(arr) => {
				let inner = arr.iter()
					.map(|val| val.to_string())
					.collect::<Vec<_>>()
					.join(", ");
				write!(f, "[{inner}]")
			}
			Self::Var(v) => write!(f, "{v}"),
			Self::Closure(_, _) => {
				write!(f, "{{ closure }}")
			}
			Self::Str(s) => write!(f, "{s}"),
			Self::Num(n) => write!(f, "{n}"),
			Self::Bool(b) => write!(f, "{b}"),
			Self::Regex(r) => write!(f, "{r}"),
			Self::Null => write!(f, "null")
		}
	}
}
