use std::{boxed, cell::RefCell, collections::HashMap, fmt::Display, ops::Deref, sync::Arc};

use pest::{iterators::{Pair, Pairs}, Parser};
use pest_derive::Parser;
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::{exec::ViCut, linebuf::LineBuf, register::read_register, vic::error::{VicErr, VicErrResult}, Opts};

#[derive(Parser)]
#[grammar = "vic/vic.pest"] // relative to src
pub struct VicParser;


/*
 * Now we will be re-implementing pests' Pair/Pairs/Span structs
 * to use 'Arc<String>' instead of &str to reference the original input.
 * Using &str creates a multitude of mutability problems later on once the Expr's are passed on to the ViCut struct
 * so we will be using an owned pointer instead :)
 * also makes it nice and simple to implement our own functionality as well
 *
 * Worth noting that converting `ArcSpan` back to `pest::Span` is a trivial operation.
 */

#[derive(Debug, Clone, PartialEq)]
pub struct ArcSpan {
	input: Arc<String>,
	start: usize,
	end: usize
}

impl ArcSpan {
	pub fn new(input: Arc<String>, start: usize, end: usize) -> Self {
		Self { input, start, end }
	}
	pub fn as_str(&self) -> &str {
		&self.input[self.start..self.end]
	}
	pub fn len(&self) -> usize {
		self.end - self.start
	}
	pub fn is_empty(&self) -> bool {
		self.len() == 0
	}
	pub fn input(&self) -> Arc<String> {
		self.input.clone()
	}
	pub fn start(&self) -> usize {
		self.start
	}
	pub fn end(&self) -> usize {
		self.end
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArcPairs {
	pairs: Vec<ArcPair>,
	index: usize
}

impl Iterator for ArcPairs {
	type Item = ArcPair;
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
pub struct ArcPair {
	pub rule: Rule,
	pub span: ArcSpan,
	pub inner: Vec<ArcPair>
}

impl ArcPair {
	pub fn from_root(root_pair: Pair<Rule>) -> Self {
		let input = Arc::new(root_pair.as_span().as_str().to_string());
		Self::from_pair(root_pair, input)
	}
	pub fn from_pair(pair: Pair<Rule>, input: Arc<String>) -> Self {
		let rule = pair.as_rule();
		let pair_span = pair.as_span();
		let span = ArcSpan::new(Arc::clone(&input), pair_span.start(), pair_span.end());
		let inner = pair.into_inner();
		let rc_inner = Self::eval_inner(inner, Arc::clone(&input));
		Self { rule, span, inner: rc_inner }
	}
	fn as_str(&self) -> &str {
		self.span.as_str()
	}
	fn as_span(&self) -> ArcSpan {
		self.span.clone()
	}
	fn as_rule(&self) -> Rule {
		self.rule
	}
	fn eval_inner(inner: Pairs<Rule>, input: Arc<String>) -> Vec<Self> {
		let mut rc_inner = vec![];
		for pair in inner {
			rc_inner.push(Self::from_pair(pair, Arc::clone(&input)));
		}
		rc_inner
	}
	pub fn into_inner(self) -> ArcPairs {
		ArcPairs { pairs: self.inner, index: 0 }
	}
}

impl Display for ArcPair {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.span.as_str())
	}
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
	pub value: ExprKind,
	pub accessors: Vec<Accessor>, // postfix stuff like foo.method(), foo.field, foo[0], etc
	pub span: ArcSpan
}

impl Expr {
	pub fn parse_vic(src: Arc<String>) -> Result<Self,VicErr> {
		let mut vic = VicParser::parse(Rule::vic, &src).map_err(|e| e.to_string())?;
		let cmd_pairs = vic.next().unwrap();
		let rc_vic = ArcPair::from_root(cmd_pairs);
		let span = rc_vic.as_span();
		let cmd_pairs_inner = rc_vic.into_inner();

		let mut top_level = vec![];
		for cmd in cmd_pairs_inner {
			if cmd.as_rule() == Rule::EOI { break }

			let parsed = Self::parse_top_level(cmd)?;
			top_level.push(parsed);
		}
		Ok(Self {
			value: ExprKind::Vic(top_level),
			accessors: vec![],
			span
		})
	}
	pub fn value(&self) -> &ExprKind {
		&self.value
	}
	pub fn into_value(self) -> ExprKind {
		self.value
	}
	fn parse_top_level(cmd: ArcPair) -> Result<Expr,VicErr> {
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
			Rule::class_def => Self::parse_class_def(cmd),
			Rule::opts => Self::parse_opts(cmd),
			Rule::top_level_cmd => {
				let inner = cmd.into_inner().next().unwrap();
				Self::parse_top_level(inner)
			}
			_ => {
				// Not reachable. This is a common pattern in parsing Pest ASTs.
				// The rule that this match statement is derived from only contains
				// the above rules, so using unreachable!() is the best way to handle
				// unhandled rules, as there basically aren't any.
				unreachable!("Unhandled rule: {:?}", cmd.as_rule())
			}
		}
	}
	fn parse_var_cmd(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let rule = cmd.as_rule();
		let mut inner = cmd.into_inner().peekable();
		let mut accessors = vec![];

		let value = if rule == Rule::var_declare {
			let name = inner.next().unwrap().as_str().to_string();
			let expr = inner.next().unwrap();
			let value = Box::new(Self::parse_expr(expr)?);

			ExprKind::VarDec { name, value }
		} else {
			let name = inner.next().unwrap().as_str().to_string();
			let mut val_or_accessor = inner.next().unwrap();
			while val_or_accessor.as_rule() == Rule::accessor {
				let idx = Self::parse_accessor(val_or_accessor)?;
				accessors.push(idx);
				val_or_accessor = inner.next().unwrap();
			}
			let op = BinOp::from_rule(rule);
			let value = Box::new(Self::parse_expr(val_or_accessor)?);

			ExprKind::VarMut { name, op, value }
		};

		Ok(Self { value, accessors, span })
	}
	fn parse_class_def(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut data = HashMap::new();
		let mut inner = cmd.into_inner();
		let name = inner.next().unwrap().as_str().to_string();
		let mut block = inner.next().unwrap().into_inner();
		let field_pairs = block.next().unwrap().into_inner();
		for field in field_pairs {
			let mut field_inner = field.into_inner();
			let name = field_inner.next().unwrap().as_str().to_string();
			let val = Self::parse_expr(field_inner.next().unwrap())?;
			data.insert(name, val);
		}
		while let Some(method_def) = block.next() {
			let method = Self::parse_func_def(method_def)?;
			let name = match &method.value {
				ExprKind::FuncDef { name, .. } => name,
				_ => unreachable!("Method definition should be a FuncDef")
			};
			data.insert(name.clone(), method);
		}
		Ok(Self {
			value: ExprKind::ClassDef { name, fields: data },
			accessors: vec![],
			span
		})
	}
	fn parse_func_def(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();

		let mut name_and_args = inner.next().unwrap().into_inner();
		let name = name_and_args.next().unwrap().as_str().to_string();

		let params_pair = name_and_args.next().unwrap()
			.into_inner().next().unwrap();
		let mut params = vec![];
		for param in params_pair.into_inner() {
			params.push(param.as_str().to_string());
		}
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = ExprKind::FuncDef { name, params, body };
		Ok(Self { value, accessors: vec![], span })
	}	
	fn parse_for_block(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();

		let var_name = inner.next().unwrap().as_str().to_string();
		let list = Box::new(Self::parse_expr(inner.next().unwrap())?);
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = ExprKind::ForBlock { var_name, list, body };
		Ok(Self { value, accessors: vec![], span })
	}
	fn cond_block(span: ArcSpan, cond: Expr, body: Vec<Expr>) -> Self {
		Self {
			value: ExprKind::CondBlock { cond: Box::new(cond), body },
			accessors: vec![],
			span
		}
	}
	fn parse_if_block(cmd: ArcPair) -> Result<Self,VicErr> {
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
			accessors: vec![],
			span
		})
	}
	fn parse_loop_block(cmd: ArcPair, polarity: bool) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let cond = Self::parse_expr(inner.next().unwrap())?;
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = if polarity {
			ExprKind::WhileBlock { cond: Box::new(cond), body }
		} else {
			ExprKind::UntilBlock { cond: Box::new(cond), body }
		};
		Ok(Self { value, accessors: vec![], span })
	}
	fn parse_block(block: ArcPair) -> Result<Vec<Self>,VicErr> {
		let mut cmds = vec![];
		let inner = block.into_inner();
		for cmd in inner {
			cmds.push(Self::parse_top_level(cmd)?)
		}
		Ok(cmds)
	}
	fn parse_accessor(accessor: ArcPair) -> Result<Accessor,VicErr> {
		let accessor_kind = accessor.into_inner().next().unwrap();
		match accessor_kind.as_rule() {
			Rule::method => Self::parse_method(accessor_kind),
			Rule::field => Self::parse_field(accessor_kind),
			Rule::index => Self::parse_index(accessor_kind),
			_ => unreachable!()
		}
	}
	fn parse_method(method: ArcPair) -> Result<Accessor,VicErr> {
		let func_call = method.into_inner().next().unwrap();
		let span = func_call.as_span();
		let method_name = func_call.clone().into_inner().next().unwrap().as_str().to_string();
		let ExprKind::FuncCall { name: _, args } = Self::parse_expr(func_call)?.value else {
			return Err(VicErr::Full(span, "Expected a function call in method accessor".to_string()));
		};
		Ok(Accessor::Method(method_name, args))
	}
	fn parse_field(field: ArcPair) -> Result<Accessor,VicErr> {
		let var_name = field.into_inner().next().unwrap();
		let field_name = var_name.as_str().to_string();
		Ok(Accessor::Field(field_name))
	}
	fn parse_index(idx: ArcPair) -> Result<Accessor,VicErr> {
		let idx_pair = idx.into_inner().next().unwrap();
		let index = match idx_pair.as_rule() {
			Rule::index_sng => {
				let inner = idx_pair.into_inner().next().unwrap();
				let index = Box::new(Self::parse_expr(inner)?);
				Index::Single(index)
			}
			Rule::index_to => {
				let inner = idx_pair.into_inner().next().unwrap();
				let index = Box::new(Self::parse_expr(inner)?);
				Index::To(index)
			}
			Rule::index_from => {
				let inner = idx_pair.into_inner().next().unwrap();
				let index = Box::new(Self::parse_expr(inner)?);
				Index::From(index)
			}
			Rule::slice => {
				let mut inner = idx_pair.into_inner();
				let start = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let end = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Index::Slice(start, end)
			}
			_ => unreachable!("Unexpected rule in index: {:?}", idx_pair.as_rule())
		};
		Ok(Accessor::Index(index))
	}
	fn parse_expr(expr: ArcPair) -> Result<Expr,VicErr> {
		match expr.as_rule() {
			Rule::value => Self::parse_value(expr),
			Rule::command => Self::parse_command(expr),
			Rule::bin_expr => Self::parse_bin_expr(expr),
			Rule::bool_expr => Self::parse_bool_expr(expr),
			Rule::expr => {
				let mut inner = expr.into_inner();
				let mut eval = Self::parse_expr(inner.next().unwrap())?;
				let mut accessors = vec![];
				while let Some(accessor) = inner.next() {
					accessors.push(Self::parse_accessor(accessor)?)
				}
				eval.accessors = accessors;
				Ok(eval)
			}
			Rule::func_call => {
				let span = expr.as_span();
				let mut inner = expr.into_inner();
				let mut name_maybe_accessor = inner.next().unwrap().into_inner();
				let mut name = Box::new(Self::parse_expr(name_maybe_accessor.next().unwrap())?);
				let mut accessors = vec![];
				while let Some(accessor) = name_maybe_accessor.next() {
					let accessor = Self::parse_accessor(accessor)?;
					accessors.push(accessor);
				}
				let mut args = vec![];
				let arg_pairs = inner.next().unwrap().into_inner()
					.next().unwrap().into_inner();
				for arg in arg_pairs {
					args.push(Self::parse_expr(arg)?);
				}
				let accessor = inner.next().map(Self::parse_accessor).transpose()?;
				let value = ExprKind::FuncCall { name, args };
				Ok(Self {
					value,
					accessors,
					span
				})
			}
			Rule::bool => {
				let span = expr.as_span();
				let bool_inner = expr.into_inner().next().unwrap();
				match bool_inner.as_rule() {
					Rule::r#true => {
						Ok(Self {
							value: ExprKind::Value(Val::Bool(true)),
							accessors: vec![],
							span
						})
					}
					Rule::r#false => {
						Ok(Self {
							value: ExprKind::Value(Val::Bool(false)),
							accessors: vec![],
							span
						})
					}
					_ => unreachable!()
				}
			}
			Rule::var => {
				let var_name = expr.as_str().to_string();
				let span = expr.as_span();
				Ok(Self {
					value: ExprKind::Value(Val::Var(var_name)),
					accessors: vec![],
					span
				})
			}
			// All of these are rules that we have to unwrap further
			// before we can continue processing. So we just unwrap
			// and then descend again.
			Rule::bin_atom |
			Rule::bin_lit |
			Rule::bool_lit |
			Rule::bool_atom |
			Rule::expr_not_recursive |
			Rule::expr_bool_priority => {
				let inner = expr.into_inner().next().unwrap();
				Self::parse_expr(inner)
			}
			_ => unreachable!("Unexpected rule: {:?}", expr.as_rule())
		}
	}
	fn parse_bool_expr(expr: ArcPair) -> Result<Expr,VicErr> {
		let mut rpn_stack = vec![];
		let mut ops: Vec<BoolOp> = vec![];
		let span = expr.as_span();
		let mut inner = expr.into_inner();

		let first = RpnItem::parse_bool_value(inner.next().unwrap())?;
		rpn_stack.push(first);

		while let Some(op) = inner.next() {
			let op = op.into_inner().next().unwrap();

			let val = inner.next().unwrap();
			let val = RpnItem::parse_bool_value(val)?;
			rpn_stack.push(val);

			let op = BoolOp::from_rule(op.as_rule()).unwrap();
			while let Some(top_op) = ops.last() {
				if top_op.precedence() >= op.precedence() {
					let rpn_item = RpnItem::BoolOp(ops.pop().unwrap());
					rpn_stack.push(rpn_item);
				} else {
					break;
				}
			}

			ops.push(op);
		}	
		rpn_stack.extend(ops.into_iter().map(RpnItem::BoolOp));
		let expr = Expr {
			value: ExprKind::BoolExpr(rpn_stack),
			accessors: vec![],
			span
		};
		Ok(expr)
	}
	/// Parse a binary expression, like `1 + 1` for instance
	///
	/// We use Shunting Yard and Reverse Polish Notation here
	/// since we already have things nice and tokenized thanks to Pest.
	fn parse_bin_expr(expr: ArcPair) -> Result<Expr,VicErr> {
		let mut rpn_stack = vec![];
		let mut ops: Vec<BinOp> = vec![];
		let span = expr.as_span();
		let mut inner = expr.into_inner();

		let first = inner.next().unwrap();
		rpn_stack.push(RpnItem::Val(Self::parse_expr(first)?));
		
		while let Some(op) = inner.next() {
			let op = op.into_inner().next().unwrap();
			let next = inner.next().unwrap();
			rpn_stack.push(RpnItem::Val(Self::parse_expr(next)?));
			let op = BinOp::from_rule(op.as_rule()).unwrap();
			while let Some(top_op) = ops.last() {
				if top_op.precedence() >= op.precedence() {
					let rpn_item = RpnItem::BinOp(ops.pop().unwrap());
					rpn_stack.push(rpn_item);
				} else {
					break;
				}
			}

			ops.push(op);
		}

		rpn_stack.extend(ops.into_iter().map(RpnItem::BinOp));
		
		let expr = Expr {
			value: ExprKind::BinExpr(rpn_stack),
			accessors: vec![],
			span
		};
		Ok(expr)
	}
	fn parse_command(cmd: ArcPair) -> Result<Expr,VicErr> {
		let cmd = cmd.into_inner().next().unwrap();
		let cmd_raw = cmd.as_str().split(" ").next().unwrap();

		match cmd_raw {
			"next" => {
				let span = cmd.as_span();
				Ok(Self {
					value: ExprKind::Command(Command::Next),
					accessors: vec![],
					span
				})
			}
			"shell" | "sh" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let cmd_expr = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::ShellCmd { cmd: cmd_expr }),
					accessors: vec![],
					span
				})
			}
			"continue" => {
				let span = cmd.as_span();
				Ok(Self {
					value: ExprKind::Command(Command::Continue),
					accessors: vec![],
					span
				})
			}
			"break" => {
				let span = cmd.as_span();
				Ok(Self {
					value: ExprKind::Command(Command::Break),
					accessors: vec![],
					span
				})
			}
			"new" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let var = Val::Str(inner.next().unwrap().as_str().to_string());
				Ok(Self {
					value: ExprKind::Command(Command::New(var.to_string())),
					accessors: vec![],
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
					accessors: vec![],
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
					accessors: vec![],
					span
				})
			}
			"move" | "m" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Move { motion }),
					accessors: vec![],
					span
				})
			}
			"cut" | "c" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Cut { motion }),
					accessors: vec![],
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
					accessors: vec![],
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
					accessors: vec![],
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
					accessors: vec![],
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
					accessors: vec![],
					span
				})
			}
			"pop" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let stack = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Pop { stack }),
					accessors: vec![],
					span
				})
			}
			"buf" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner().next().unwrap().into_inner();
				let rule = inner.next().unwrap().as_rule();
				match rule {
					Rule::buf_switch => {
						let id = Box::new(Self::parse_expr(inner.next().unwrap())?);
						Ok(Self {
							value: ExprKind::Command(Command::BufSwitch { id }),
							accessors: vec![],
							span
						})
					}
					Rule::buf_id => {
						Ok(Self {
							value: ExprKind::Command(Command::BufId),
							accessors: vec![],
							span
						})
					}
					_ => unreachable!("Unexpected rule in buf command: {rule:?}")
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
					accessors: vec![],
					span
				})
			}
			"include" => {
				let span = cmd.as_span();
				let mut inner = cmd.into_inner();
				let path = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Include { path }),
					accessors: vec![],
					span
				})
			}
			_ => Err(VicErr::Full(cmd.as_span(), format!("Unknown command: {cmd_raw}")))
		}
	}
	fn parse_opts(opts: ArcPair) -> Result<Expr,VicErr> {
		let span = opts.as_span();
		let mut parsed_opts = vec![];
		for opt in opts.into_inner() {
			let span = opt.as_span();
			let mut arg = None;
			let mut opt = opt.into_inner().next().unwrap();
			let mut set = true;
			while opt.as_rule() == Rule::not {
				set = !set;
				opt = opt.into_inner().next().unwrap();
			}
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
				value: ExprKind::Opt { set, name: name.to_string(), arg },
				accessors: vec![],
				span
			});
		}
		Ok(Self{
			value: ExprKind::Opts(parsed_opts),
			accessors: vec![],
			span
		})
	}
	fn parse_value(value: ArcPair) -> Result<Expr,VicErr> {
		let span = value.as_span();
		let value_kind = value.into_inner().next().unwrap();
		let value = Val::try_from_pair(value_kind).try_blame(span.clone())?;
		Ok(Self {
			value: ExprKind::Value(value),
			accessors: vec![],
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
	pub fn span(&self) -> ArcSpan {
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
	/// `set` is whether or not the opt is passed with a leading '!'
	/// if set is true, the option will be set back to it's default value
	/// e.g. Some(false), this is to allow shadowing existing options
	Opt { set: bool, name: String, arg: Option<Box<Expr>> },
	VarDec { name: String, value: Box<Expr> },
	VarMut { name: String, op: Option<BinOp>, value: Box<Expr> },
	CondBlock { cond: Box<Expr>, body: Vec<Expr> },
	ForBlock { var_name: String, list: Box<Expr>, body: Vec<Expr> },
	IfBlock { cond_blocks: Vec<Expr>, else_block: Option<Vec<Expr>>, },
	WhileBlock { cond: Box<Expr>, body: Vec<Expr> },
	UntilBlock { cond: Box<Expr>, body: Vec<Expr> },
	Range { start: Box<Expr>, end: Box<Expr> },
	BinExpr(Vec<RpnItem>),
	BoolExpr(Vec<RpnItem>),
	FuncCall { name: Box<Expr>, args: Vec<Expr> },
	FuncDef { name: String, params: Vec<String>, body: Vec<Expr> },
	ClassDef { name: String, fields: HashMap<String, Expr> },
}

impl ExprKind {
	pub fn string(str: impl ToString) -> Self {
		Self::Value(Val::Str(str.to_string()))
	}
}

#[derive(Debug,Clone,PartialEq)]
pub enum Command {
	Next,
	New(String), // constructor for classes
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
pub enum Accessor {
	Method(String,Vec<Expr>),
	Field(String),
	Index(Index)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Index {
	Single(Box<Expr>),
	To(Box<Expr>),
	ToInc(Box<Expr>),
	From(Box<Expr>),
	Slice(Box<Expr>,Box<Expr>),
	SliceInc(Box<Expr>,Box<Expr>),
}

/// Reverse polish notation stuff
#[derive(Debug, Clone, PartialEq)]
pub enum RpnItem {
	BinOp(BinOp),
	BoolOp(BoolOp),
	Val(Expr),
	Not(Expr),
}

impl RpnItem {
	pub fn parse_bool_value(value: ArcPair) -> Result<Self, VicErr> {
		let mut inner = value.into_inner();
		let mut next = inner.next().unwrap();
		let mut polarity = true;
		while next.as_rule() == Rule::not {
			polarity = !polarity;
			next = inner.next().unwrap();
		}
		let expr = Expr::parse_expr(next)?;
		if polarity {
			Ok(Self::Val(expr))
		} else {
			Ok(Self::Not(expr))
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
	pub fn precedence(&self) -> usize {
		match self {
			Self::Equals => 1,
			Self::Add | Self::Sub => 2,
			Self::Mult | Self::Div | Self::Mod => 3,
			Self::Pow => 4,
		}
	}
	pub fn from_rule(rule: Rule) -> Option<Self> {
		match rule {
			Rule::add => Some(Self::Add),
			Rule::sub => Some(Self::Sub),
			Rule::mult => Some(Self::Mult),
			Rule::div => Some(Self::Div),
			Rule::modulo => Some(Self::Mod),
			Rule::pow => Some(Self::Pow),
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

#[derive(Debug, Clone, PartialEq)]
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
	pub fn precedence(&self) -> usize {
		match self {
			Self::Not => 1,
			Self::And => 3,
			Self::Or => 4,
			_ => 2
		}
	}
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
	Register(char),
	Closure(Vec<String>, Vec<Expr>),
	Dict(HashMap<String,Val>),
	Class(String,HashMap<String,Val>),
	Bool(bool),
	Ref(Arc<RefCell<Box<Val>>>),
	Regex(Regex),
	Expr(Box<Expr>),

	/// This one is *only* used internally, and not exposed to the user directly
	/// The `_buffers` built-in variable contains only these.
	/// We also have to box it, because LineBuf as a struct requires at least 384 bytes, and that's when it's empty.
	/// This requirement means that *all* Val instances would be at least 384 bytes in size
	/// And that sounds like hell, so we will just store a pointer.
	Buffer(Box<LineBuf>) 
}

impl Val {
	/// Unwrap implementation for `Val`
	///
	/// Panics if the value is `Null`
	pub fn unwrap(self) -> Self {
		if let Self::Null = self { panic!("Called unwrap on a Null value") }
		self
	}
	pub fn try_deref(&self) -> Self {
		match self {
			Self::Ref(refer) => {
				let val = refer.borrow();
				(**val).clone()
			}
			_ => self.clone()
		}
	}
	/// Unwrap implementation for `Val` with a default value
	pub fn unwrap_or_else<F: FnOnce() -> Self>(self, default: F) -> Self { 
		if let Self::Null = self { return default() }
		self
	}
	pub fn is_compound(&self) -> bool {
		matches!(self, Self::Arr(_) | Self::Str(_))
	}
	pub fn try_iter(&self) -> Result<impl Iterator, VicErr> {
		match self {
			Self::Arr(arr) => Ok(arr.clone().into_iter()),
			Self::Str(s) => {
				let graphemes = s.graphemes(true).map(|g| Val::Str(g.to_string())).collect::<Vec<_>>();
				Ok(graphemes.into_iter())
			}
			_ => Err(VicErr::Simple(format!("Value of type '{}' is not iterable", self.display_type())))
		}
	}
	pub fn try_into_iter(self) -> Result<impl Iterator<Item=Val>, VicErr> {
		match self {
			Self::Arr(arr) => Ok(arr.into_iter()),
			Self::Str(s) => {
				let graphemes = s.graphemes(true).map(|g| Val::Str(g.to_string())).collect::<Vec<_>>();
				Ok(graphemes.into_iter())
			}
			_ => Err(VicErr::Simple(format!("Value of type '{}' is not iterable", self.display_type())))
		}
	}
	pub fn try_from_pair(pair: ArcPair) -> Result<Self,VicErr> {
		match pair.as_rule() {
			Rule::array => {
				let mut elements = vec![];
				let elem_list = pair.into_inner().next().unwrap().into_inner();
				for elem in elem_list {
					let elem_inner = elem.into_inner().next().unwrap();
					match elem_inner.as_rule() {
						Rule::value => elements.push(Self::try_from_pair(elem_inner.into_inner().next().unwrap())?),
						Rule::expr => elements.push(Self::Expr(Box::new(Expr::parse_expr(elem_inner)?))),
						_ => return Err(VicErr::Simple(format!("Unexpected rule in array: {:?}", elem_inner.as_rule()))),
					}
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
				_ => unreachable!("Unexpected rule: {:?}", pair.as_rule())
		}
	}
	pub fn add(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => Ok(Self::Num(n1 + n2)),
			(Self::Str(s1), s2) => Ok(Self::Str(s1.to_string() + &s2.to_string())),
			(s1, Self::Str(s2)) => Ok(Self::Str(s1.to_string() + &s2.to_string())),
			(Self::Arr(arr), val) => {
				let mut arr = arr.clone();
				arr.push(val.clone());
				Ok(Self::Arr(arr))
			}
			_ => Err(VicErr::Simple(format!("Cannot add values of type '{}' and '{}'", self.display_type(), other.display_type())))
		}
	}
	pub fn sub(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => Ok(Self::Num(n1 - n2)),
			_ => Err(VicErr::Simple(format!("Cannot subtract values of type '{}' and '{}'", self.display_type(), other.display_type())))
		}
	}
	pub fn mult(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => Ok(Self::Num(n1 * n2)),
			(Self::Str(s1), Self::Num(n2)) => {
				let repeated = s1.repeat(*n2 as usize);
				Ok(Self::Str(repeated))
			}
			_ => Err(VicErr::Simple(format!("Cannot multiply values of type '{}' and '{}'", self.display_type(), other.display_type())))
		}
	}
	pub fn div(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => {
				if *n2 == 0 {
					return Err(VicErr::Simple("Division by zero".to_string()));
				}
				Ok(Self::Num(n1 / n2))
			}
			_ => Err(VicErr::Simple(format!("Cannot divide values of type '{}' and '{}'", self.display_type(), other.display_type())))
		}
	}
	pub fn modulo(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => {
				if *n2 == 0 {
					return Err(VicErr::Simple("Division by zero".to_string()));
				}
				Ok(Self::Num(n1 % n2))
			}
			_ => Err(VicErr::Simple(format!("Cannot modulo values of type '{}' and '{}'", self.display_type(), other.display_type())))
		}
	}
	pub fn pow(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => Ok(Self::Num(n1.pow(*n2 as u32))),
			_ => Err(VicErr::Simple(format!("Cannot power values of type '{}' and '{}'", self.display_type(), other.display_type())))
		}
	}
	pub fn display_type(&self) -> String {
		match self {
			Self::Buffer(_) => "buffer".to_string(),
			Self::Class(_,_) => "class".to_string(),
			Self::Dict(_) => "dictionary".to_string(),
			Self::Ref(refer) => {
				let inner = refer.borrow();
				inner.display_type()
			}
			Self::Str(_) => "string".to_string(),
			Self::Num(_) => "number".to_string(),
			Self::Register(_) => "register".to_string(),
			Self::Var(_) => "variable".to_string(),
			Self::Closure(_,_) => "closure".to_string(),
			Self::Arr(_) => "array".to_string(),
			Self::Bool(_) => "boolean".to_string(),
			Self::Regex(_) => "regex".to_string(),
			Self::Null => "null".to_string(),
			Self::Expr(_) => "expression".to_string(),
		}
	}
	pub fn is_truthy(&self, vicut: &mut ViCut) -> bool {
		match self {
			Self::Buffer(buf) => !buf.buffer.is_empty(),
			Self::Dict(dict) => !dict.is_empty(),
			Self::Ref(refer) => {
				let inner = refer.borrow();
				inner.is_truthy(vicut)
			},
			Self::Class(name, data) => {
				data.is_empty() && name.is_empty()
			},
			Self::Str(s) => !s.is_empty(),
			Self::Num(n) => *n != 0,
			Self::Expr(e) => {
				vicut.eval_expr(false, e).is_ok_and(|eval| eval.is_truthy(vicut))
			}
			Self::Register(ch) => {
				read_register(Some(*ch)).is_some_and(|content| !content.is_empty())
			}
			Self::Var(v) => {
				let Some(var) = vicut.read_var(v).clone() else { return false };
				var.is_truthy(vicut)
			}
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
			Self::Ref(refer) => {
				let inner = refer.borrow();
				write!(f, "{inner}")
			}
			Self::Dict(dict) => {
				let mut key_values = vec![];
				for (key,value) in dict {
					key_values.push(format!("{key}: {value}"))
				}
				let joined = key_values.join(", ");
				write!(f, "{{{joined}}}")
			}
			Self::Class(name, dict) => write!(f, "{{ class }}"),
			Self::Expr(_) => {
				write!(f, "{{ expression }}")
			}
			Self::Register(ch) => write!(f, "@{ch}"),
			Self::Buffer(buf) => write!(f, "{}", &buf.buffer),
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
