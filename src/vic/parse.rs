use std::{cell::RefCell, cmp::Ordering, collections::{HashMap, VecDeque}, fmt::{Debug, Display}, rc::Rc, sync::Arc};

use log::debug;
use pest::{iterators::{Pair, Pairs}, Parser};
use pest_derive::Parser;
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::{exec::ViCut, register::read_register, vic::{error::{expr_error, VicErr, VicErrResult}, libvic::Builtin}};

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

#[derive(Clone, PartialEq)]
pub struct ArcSpan {
	input: Arc<String>,
	start: usize,
	end: usize
}

impl Debug for ArcSpan {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
	  write!(f, "{{ arc span }}")
	}
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
	pub fn debug(&self) -> String {
		let mut debug_str = format!("Expr: {:?}\n", self.value);
		debug_str.push_str(&format!("Accessors: {:?}\n", self.accessors));
		debug_str
	}
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
	pub fn value_mut(&mut self) -> &mut ExprKind {
		&mut self.value
	}
	pub fn into_value(self) -> ExprKind {
		self.value
	}
	fn parse_top_level(cmd: ArcPair) -> Result<Expr,VicErr> {
		match cmd.as_rule() {
			Rule::var_declare        |
			Rule::var_add            |
			Rule::var_sub            |
			Rule::var_mut            |
			Rule::var_mult           |
			Rule::var_div            |
			Rule::var_pow            |
			Rule::var_mod            => Self::parse_var_cmd(cmd),
			Rule::block_struct       => Self::parse_block_struct(cmd),
			Rule::command            => Self::parse_expr(cmd),
			Rule::func_def           => Self::parse_func_def(cmd),
			Rule::class_def          => Self::parse_class_def(cmd),
			Rule::opts               => Self::parse_opts(cmd),
			Rule::expr_bool_priority |
			Rule::expr_not_recursive |
			Rule::expr               => Self::parse_expr(cmd),
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
	fn parse_block_struct(cmd: ArcPair) -> Result<Self,VicErr> {
		let inner = cmd.into_inner().next().unwrap();
		match inner.as_rule() {
			Rule::with_block => Self::parse_with_block(inner),
			Rule::if_block => Self::parse_if_block(inner),
			Rule::loop_block => Self::parse_loop_block(inner),
			Rule::catch_block => Self::parse_catch_block(inner),
			Rule::try_block => Self::parse_try_block(inner),
			Rule::do_while_block => Self::parse_postfix_loop_block(inner, true),
			Rule::do_until_block => Self::parse_postfix_loop_block(inner, false),
			Rule::while_block => Self::parse_prefix_loop_block(inner, true),
			Rule::until_block => Self::parse_prefix_loop_block(inner, false),
			Rule::switch_block => Self::parse_switch_block(inner),
			Rule::for_block => Self::parse_for_block(inner),
			_ => unreachable!()
		}
	}
	fn parse_try_block(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let scrutinee = Box::new(Self::parse_expr(inner.next().unwrap())?);
		let (try_bind,try_block) = {
			let next = inner.next().unwrap();
			match next.as_rule() {
				Rule::block => (None, Self::parse_block(next)?),
				Rule::var => (Some(next.as_str().to_string()), Self::parse_block(inner.next().unwrap())?),
				_ => unreachable!(),
			}
		};
		Ok(Self {
			value: ExprKind::TryBlock { scrutinee, try_bind, try_block },
			accessors: vec![],
			span: span.clone()
		})
	}
	fn parse_catch_block(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let scrutinee = Box::new(Self::parse_expr(inner.next().unwrap())?);
		let (err_bind,catch_block) = {
			let next = inner.next().unwrap();
			match next.as_rule() {
				Rule::block => (None, Self::parse_block(next)?),
				Rule::var => (Some(next.as_str().to_string()), Self::parse_block(inner.next().unwrap())?),
				_ => unreachable!(),
			}
		};
		Ok(Self {
			value: ExprKind::CatchBlock { scrutinee, err_bind, catch_block },
			accessors: vec![],
			span: span.clone()
		})
	}
	fn parse_loop_block(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let body = Self::parse_block(inner.next().unwrap())?;
		let value = ExprKind::LoopBlock { body };
		Ok(Self { value, accessors: vec![], span })
	}
	fn parse_postfix_loop_block(cmd: ArcPair, polarity: bool) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let body = Self::parse_block(inner.next().unwrap())?;
		let cond = Self::parse_expr(inner.next().unwrap())?;
		let value = if polarity {
			ExprKind::DoWhileBlock { body, cond: Box::new(cond) }
		} else {
			ExprKind::DoUntilBlock { body, cond: Box::new(cond) }
		};
		Ok(Self { value, accessors: vec![], span })
	}
	fn parse_with_block(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let buffer = Box::new(Self::parse_expr(inner.next().unwrap())?);
		let body = Self::parse_block(inner.next().unwrap())?;
		Ok(Self {
			value: ExprKind::WithBlock { buffer, body },
			accessors: vec![],
			span
		})
	}
	fn parse_switch_block(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut inner = cmd.into_inner();
		let scrutinee = Self::parse_expr(inner.next().unwrap())?;
		let case_pairs = inner.next().unwrap().into_inner();
		let mut cases = vec![];
		let mut default = None;
		for case in case_pairs {
			let mut patterns = vec![];
			match case.as_rule() {
				Rule::case => {
					let mut case_inner = case.into_inner();
					let mut value = case_inner.next().unwrap();
					while value.as_rule() == Rule::literal {
						let parsed = Val::try_from_pair(value.into_inner().next().unwrap())?;
						patterns.push(parsed);
						value = case_inner.next().unwrap();
					}
					let body = Self::parse_block(value)?;
					cases.push(Self::case_block(span.clone(), patterns, body))
				}
				Rule::default_block => {
					default = Some(Self::parse_block(case.into_inner().next().unwrap())?)
				}
				_ => unreachable!()
			}
		}
		Ok(Self {
			value: ExprKind::SwitchBlock {
				scrutinee: Box::new(scrutinee),
				case_blocks: cases,
				default_block: default
			},
			accessors: vec![],
			span
		})
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
			let name_val = Val::Var(inner.next().unwrap().as_str().to_string());
			let mut val_or_accessor = inner.next().unwrap();
			while val_or_accessor.as_rule() == Rule::accessor {
				let accessor = Self::parse_accessor(val_or_accessor.clone())?;
				accessors.push(accessor);
				val_or_accessor = inner.next().unwrap();
			}
			let name = Box::new(Expr {
				value: ExprKind::Value(name_val),
				accessors,
				span: ArcSpan::new(Arc::clone(&span.input), span.start, span.end)
			});
			let op = BinOp::from_rule(rule);
			let value = Box::new(Self::parse_expr(val_or_accessor)?);

			ExprKind::VarMut { name, op, value }
		};

		Ok(Self { value, accessors: vec![], span })
	}
	fn parse_class_def(cmd: ArcPair) -> Result<Self,VicErr> {
		let span = cmd.as_span();
		let mut data = HashMap::new();
		let mut inner = cmd.into_inner();
		let name = inner.next().unwrap().as_str().to_string();
		let mut block = inner.next().unwrap().into_inner();
		let field_pairs = block.next().unwrap().into_inner();
		for field in field_pairs {
			let field_span = field.as_span();
			let mut field_inner = field.into_inner();
			let name = field_inner.next().unwrap().as_str().to_string();

			// Fields can be written as 'field: expr' or just 'field'
			// In the latter case, we initialize it to null
			let val = if let Some(expr) = field_inner.next() {
				Self::parse_expr(expr)?
			} else {
				Expr {
					value: ExprKind::Value(Val::Null),
					accessors: vec![],
					span: field_span,
				}
			};
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
	fn case_block(span: ArcSpan, cases: Vec<Val>, body: Vec<Expr>) -> Self {
		Self {
			value: ExprKind::CaseBlock { cond: cases, body },
			accessors: vec![],
			span
		}
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
	fn parse_prefix_loop_block(cmd: ArcPair, polarity: bool) -> Result<Self,VicErr> {
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
			Rule::field => Self::parse_field(accessor_kind),
			Rule::index => Self::parse_index(accessor_kind),
			Rule::call => Self::parse_call(accessor_kind),
			Rule::err_prop => Ok(Accessor::ErrProp),
			_ => unreachable!("Unexpected rule in accessor: {:?}", accessor_kind.as_rule())
		}
	}
	fn parse_call(call: ArcPair) -> Result<Accessor,VicErr> {
		let mut arg_list = call.into_inner().next().unwrap().into_inner();
		let mut args = vec![];
		while let Some(arg) = arg_list.next() {
			args.push(Self::parse_expr(arg)?);
		}
		Ok(Accessor::Call(args.into()))
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
			Rule::block_struct => Self::parse_block_struct(expr),
			Rule::block => {
				let span = expr.as_span();
				let block = Self::parse_block(expr)?;
				Ok(Self {
					value: ExprKind::Block(block),
					accessors: vec![],
					span
				})
			}
			Rule::expr => {
				let mut inner = expr.into_inner();
				let mut eval = Self::parse_expr(inner.next().unwrap())?;
				let mut accessors = vec![];
				while let Some(accessor) = inner.next() {
					accessors.push(Self::parse_accessor(accessor)?)
				}
				if eval.accessors.is_empty() {
					eval.accessors = accessors;
				}
				Ok(eval)
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
			Rule::bool_node => {
				let span = expr.as_span();
				let mut inner = expr.into_inner();
				let left = Self::parse_expr(inner.next().unwrap())?;
				let op = LogOp::from_rule(inner.next().unwrap().into_inner().next().unwrap().as_rule()).unwrap();
				let right = Self::parse_expr(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::BoolNode { left: Box::new(left), op, right: Box::new(right) },
					accessors: vec![],
					span
				})
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
				let mut inner = expr.into_inner();
				let mut accessors = vec![];
				let next = inner.next().unwrap();
				let mut expr = Self::parse_expr(next)?;
				while let Some(pair) = inner.next() {
					if let Rule::accessor = pair.as_rule() {
						let accessor = Self::parse_accessor(pair)?;
						accessors.push(accessor);
					}
				}
				if expr.accessors.is_empty() {
					expr.accessors = accessors;
				}
				Ok(expr)
			}
			_ => unreachable!("Unexpected rule: {:?}", expr.as_rule())
		}
	}
	fn parse_bool_expr(expr: ArcPair) -> Result<Expr,VicErr> {
		let mut rpn_stack = vec![];
		let mut ops: Vec<BoolOp> = vec![];
		let span = expr.as_span();
		let mut inner = expr.into_inner();
		let first_pair = inner.next().unwrap();
		if first_pair.as_rule() == Rule::not {
			// we are in an expression that is literally just '!value'
			// so that makes things easy(?)
			let next = inner.next().unwrap();
			let expr = Expr::parse_expr(next)?;
			rpn_stack.push(RpnItem::Not(expr));
			// there won't be anything after this
			// so the while loop won't run
		} else {
			let first = RpnItem::parse_bool_value(first_pair)?;
			rpn_stack.push(first);
		}


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
		let span = cmd.as_span();
		let cmd = cmd.into_inner().next().unwrap();
		let cmd_raw = cmd.as_str().split(" ").next().unwrap();

		match cmd_raw.trim() {
			"next" => {
				Ok(Self {
					value: ExprKind::Command(Command::Next),
					accessors: vec![],
					span
				})
			}
			"continue" => {
				Ok(Self {
					value: ExprKind::Command(Command::Continue),
					accessors: vec![],
					span
				})
			}
			"break" => {
				Ok(Self {
					value: ExprKind::Command(Command::Break),
					accessors: vec![],
					span
				})
			}
			"new" => {
				let mut inner = cmd.into_inner();
				let var = Self::parse_expr(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::New(Box::new(var))),
					accessors: vec![],
					span
				})
			}
			"exit" => {
				let mut inner = cmd.into_inner();
				let code = if let Some(code_expr) = inner.next() {
					Some(Box::new(Self::parse_expr(code_expr)?))
				} else {
					None
				};
				Ok(Self {
					value: ExprKind::Command(Command::Exit { code }),
					accessors: vec![],
					span
				})
			}
			"ref" => {
				let mut inner = cmd.into_inner();
				let var = Self::parse_expr(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::Ref(Box::new(var))),
					accessors: vec![],
					span
				})
			}
			"global" | "g" => {
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
				let mut inner = cmd.into_inner();
				let pattern = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let block = Self::parse_block(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::NotGlobal { pattern, block }),
					accessors: vec![],
					span
				})
			}
			"error!" => {
				let mut inner = cmd.into_inner();
				let msg = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Error(span.clone(),msg)),
					accessors: vec![],
					span
				})
			}
			"debug!" => {
				let mut inner = cmd.into_inner();
				let msg = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Debug(msg)),
					accessors: vec![],
					span
				})
			}
			"move" | "m" => {
				let mut inner = cmd.into_inner();
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Move { motion }),
					accessors: vec![],
					span
				})
			}
			"cut" | "c" => {
				let mut inner = cmd.into_inner();
				let motion = Box::new(Self::parse_expr(inner.next().unwrap())?);
				Ok(Self {
					value: ExprKind::Command(Command::Cut { motion }),
					accessors: vec![],
					span
				})
			}
			"repeat" | "r" => {
				let mut inner = cmd.into_inner();
				let count = Box::new(Self::parse_expr(inner.next().unwrap())?);
				let block = Self::parse_block(inner.next().unwrap())?;
				Ok(Self {
					value: ExprKind::Command(Command::Repeat { count, block }),
					accessors: vec![],
					span
				})
			}
			"return" => {
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
	VarMut { name: Box<Expr>, op: Option<BinOp>, value: Box<Expr> },
	CondBlock { cond: Box<Expr>, body: Vec<Expr> },
	CaseBlock { cond: Vec<Val>, body: Vec<Expr> },
	SwitchBlock { scrutinee: Box<Expr>, case_blocks: Vec<Expr>, default_block: Option<Vec<Expr>> },
	IfBlock { cond_blocks: Vec<Expr>, else_block: Option<Vec<Expr>>, },
	LoopBlock { body: Vec<Expr> },
	ForBlock { var_name: String, list: Box<Expr>, body: Vec<Expr> },
	DoWhileBlock { cond: Box<Expr>, body: Vec<Expr> },
	DoUntilBlock { cond: Box<Expr>, body: Vec<Expr> },
	WhileBlock { cond: Box<Expr>, body: Vec<Expr> },
	UntilBlock { cond: Box<Expr>, body: Vec<Expr> },
	WithBlock { buffer: Box<Expr>, body: Vec<Expr> },
	CatchBlock { scrutinee: Box<Expr>, err_bind: Option<String>, catch_block: Vec<Expr> },
	TryBlock { scrutinee: Box<Expr>, try_bind: Option<String>, try_block: Vec<Expr> },
	Range { start: Box<Expr>, end: Box<Expr> },
	BinExpr(Vec<RpnItem>),
	BoolExpr(Vec<RpnItem>),
	BoolNode { left: Box<Expr>, op: LogOp, right: Box<Expr> },
	FuncDef { name: String, params: Vec<String>, body: Vec<Expr> },
	ClassDef { name: String, fields: HashMap<String, Expr> },
}

#[derive(Debug,Clone,PartialEq)]
pub enum Command {
	Next,
	New(Box<Expr>), // constructor for classes
	Ref(Box<Expr>), // reference to a value
	Error(ArcSpan,Box<Expr>),
	Debug(Box<Expr>),
	Continue,
	Break,
	Exit { code: Option<Box<Expr>> }, // exit the interpreter, optionally with a code
	Return { ret: Option<Box<Expr>> },
	Global { pattern: Box<Expr>, block: Vec<Expr>, },
	NotGlobal { pattern: Box<Expr>, block: Vec<Expr>, },
	Move { motion: Box<Expr> },
	Cut { motion: Box<Expr> },
	Repeat { count: Box<Expr>, block: Vec<Expr> },
	Yank { register: Box<Expr>, motion: Box<Expr> },
	ShellCmd { cmd: Box<Expr> },
	Include { path: Box<Expr> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Accessor {
	Call(Rc<[Expr]>),
	Field(String),
	Index(Index),
	ErrProp
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
pub enum LogOp {
	And,
	Or
}

impl LogOp {
	pub fn from_rule(rule: Rule) -> Option<Self> {
		match rule {
			Rule::and => Some(Self::And),
			Rule::or => Some(Self::Or),
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
}

impl BoolOp {
	pub fn precedence(&self) -> usize {
		match self {
			Self::Not => 1,
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
			_ => None
		}
	}
}


pub type RcVal = Rc<RefCell<Val>>;

/// A runtime-checked reference to a value in the interpreter
///
/// `ValRef` represents a reference to a `Val` within the interpreter, using a raw pointer.
/// To ensure the reference does not outlive the scope of the value it refers to,
/// the `min_depth` field tracks the deepest scope in which the reference is valid.
/// If the reference is accessed in a scope with depth less than `min_depth`,
/// a runtime error is thrown to prevent use-after-free behavior.
///
/// # Safety
/// The `ptr` field is a raw pointer and must only point to values that are guaranteed
/// to live until at least `min_depth`. The interpreter's scope stack is already managed
/// automatically by the use of the `ScopeGuard` struct, which pops the current scope when it is dropped.
/// This means that `ValRef` can be safely used as long as the interpreter's scope management is respected.
#[derive(Default, Debug, Clone)]
pub struct ValRef {
    ptr: *mut Val, // oooo spooooooky
    min_depth: usize,
}

impl ValRef {
	pub fn min_depth(&self) -> usize {
		self.min_depth
	}
	pub fn as_borrow(&self) -> &Val {
		assert!(!self.ptr.is_null(), "Attempted to dereference a null pointer in ValRef");
		unsafe { &*self.ptr }
	}
	pub fn as_mut_borrow(&mut self) -> &mut Val {
		assert!(!self.ptr.is_null(), "Attempted to dereference a null pointer in ValRef");
		unsafe { &mut *self.ptr }
	}
	pub fn peel_refs(&self) -> &Val {
		let mut current = self.as_borrow();
		while let Val::Ref(val_ref) = current {
			current = val_ref.as_borrow();
		}
		current
	}
	pub fn peel_refs_mut(&mut self) -> &mut Val {
		let mut current = self.as_mut_borrow();
		while let Val::Ref(val_ref) = current {
			current = val_ref.as_mut_borrow();
		}
		current
	}
}

// Evaluated expressions
#[derive(Default, Debug, Clone)]
pub enum Val {
	#[default]
	Null,
	Err(ArcSpan,Box<RcVal>),
	Ref(ValRef),
	Str(String),
	Var(String),
	Arr(Rc<RefCell<VecDeque<Val>>>),
	Num(isize),
	Register(char),
	BoundClosure(Box<Val>, Box<RcVal>), // holds the Closure and the 'self' value
	Closure(Rc<[String]>, Rc<[Expr]>),
	Dict(Rc<RefCell<HashMap<String,Val>>>),
	Constructor(String, HashMap<String, Expr>), // name and fields
	Bool(bool),
	Regex(Regex),
	Expr(Box<Expr>),

	// these two are functionally identical to "Null"
	// but used internally for control flow in loops
	// the "break"/"continue" keywords return these values
	Break,
	Continue,

	/// This one is *only* used internally, and not exposed to the user directly
	/// Used as a value that represents the currently selected buffer
	BuiltinHandle(Builtin)

}

impl From<Val> for RcVal {
	fn from(value: Val) -> Self {
		Rc::new(RefCell::new(value))
	}
}

impl Val {
	/// Unwrap implementation for `Val`
	///
	/// Panics if the value is `Null`
	pub fn unwrap(self) -> Self {
		if let Self::Null = self { panic!("Called unwrap on a Null value") }
		self
	}

	pub fn new_str(str: String) -> Self {
		Self::Str(str)
	}
	pub fn new_arr(arr: VecDeque<Val>) -> Self {
		Self::Arr(Rc::new(RefCell::new(arr)))
	}
	pub fn new_dict(dict: HashMap<String, Val>) -> Self {
		Self::Dict(Rc::new(RefCell::new(dict)))
	}
	pub fn to_int(self) -> Result<Self,VicErr> {
		self.to_string().parse::<isize>().map_err(|_| {
			VicErr::Simple(format!("Could not convert value to integer: {self}"))
		}).map(Self::Num)
	}
	/// Unwrap implementation for `Val` with a default value
	pub fn unwrap_or_else<F: FnOnce() -> Self>(self, default: F) -> Self {
		if let Self::Null = self { return default() }
		self
	}
	pub fn into_ref(&mut self, min_depth: usize) -> Self {
		let ptr = self as *mut Val;
		Self::Ref(ValRef { ptr, min_depth })
	}
	pub fn into_ref_from_ptr(ptr: *mut Val, min_depth: usize) -> Self {
		Self::Ref(ValRef { ptr, min_depth })
	}
	pub fn deep_clone(&self) -> Self {
		match self {
			Val::Dict(map) => {
				let new_map = map.borrow().iter()
					.map(|(k, v)| (k.clone(), v.deep_clone()))
					.collect();
				Val::Dict(Rc::new(RefCell::new(new_map)))
			}
			Val::Arr(arr) => {
				let new_arr = arr.borrow().iter()
					.map(|v| v.deep_clone())
					.collect();
				Val::Arr(Rc::new(RefCell::new(new_arr)))
			}
			Val::Str(str) => {
				Val::Str(str.clone())
			}
			Val::Ref(val) => {
				Val::Ref(val.clone())
			}
			_ => self.clone()
		}
	}
	pub fn cmp(&self, other: &Val, vicut: &mut ViCut) -> Option<Ordering> {
		match self {
			Val::BuiltinHandle(_) => unreachable!(),
			Val::Constructor(_, _) => todo!(),
			Val::Err(_, _) => None,
			Val::Ref(val) => {
				let val_ref = val.as_borrow();
				val_ref.cmp(other,vicut)
			}
			Val::Break |
			Val::Continue |
			Val::Null => {
				if let Val::Null = other { Some(Ordering::Equal) } else { Some(Ordering::Less) }
			}
			Val::Str(str1) => {
				if let Val::Str(str2) = other {
					debug!("Comparing strings: '{str1}' and '{str2}'");
					Some(str1.cmp(str2))
				} else if let Val::Regex(regex) = other {
					if regex.is_match(str1) {
						Some(Ordering::Equal)
					} else {
						None
					}
				} else {
					None
				}
			}
			Val::Var(var) => {
				let val = vicut.read_var(var)?;
				val.cmp(other, vicut)
			}
			Val::Arr(ref_cells) => {
				if let Val::Arr(other_cells) = other {
					let this_arr = ref_cells.borrow();
					let other_arr = other_cells.borrow();
					let mut iter1 = this_arr.iter();
					let mut iter2 = other_arr.iter();
					loop {
						match (iter1.next(), iter2.next()) {
							(Some(val1), Some(val2)) => {
								if let Some(ordering) = val1.cmp(val2, vicut) {
									if ordering != Ordering::Equal { return Some(ordering) }
								} else {
									return None;
								}
							}
							(None, None) => return Some(Ordering::Equal),
							(None, _) => return Some(Ordering::Less),
							(_, None) => return Some(Ordering::Greater),
						}
					}
				} else {
					None
				}
			}
			Val::Num(n1) => {
				if let Val::Num(n2) = other {
					Some(n1.cmp(n2))
				} else {
					None
				}
			}
			Val::Register(reg) => {
				let content = read_register(Some(*reg))?.to_string();
				if let Val::Str(other_str) = other {
					Some(content.cmp(other_str))
				} else {
					None
				}
			}
			Val::BoundClosure(_, _) |
			Val::Closure(_, _) => {
				panic!("this should have already been evaluated")
			}
			Val::Dict(hash_map) => {
				if let Val::Dict(other_map) = other {
					let this_map = hash_map.borrow();
					let other_map = other_map.borrow();
					let mut iter1 = this_map.iter();
					let mut iter2 = other_map.iter();
					loop {
						match (iter1.next(), iter2.next()) {
							(Some((key1, val1)), Some((key2, val2))) => {
								if key1 != key2 { return None }
								if let Some(ordering) = val1.cmp(val2, vicut) {
									if ordering != Ordering::Equal { return Some(ordering) }
								} else {
									return None;
								}
							}
							(None, None) => return Some(Ordering::Equal),
							(None, _) => return Some(Ordering::Less),
							(_, None) => return Some(Ordering::Greater),
						}
					}
				} else {
					None
				}
			}
			Val::Bool(bool1) => {
				if let Val::Bool(bool2) = other {
					Some(bool1.cmp(bool2))
				} else {
					None
				}
			}
			Val::Regex(regex) => {
				if let Val::Str(other_str) = other {
					if regex.is_match(other_str) {
						Some(Ordering::Equal)
					} else {
						None
					}
				} else {
					None
				}
			}
			Val::Expr(_) => {
				panic!("this should have already been evaluated")
			}
		}
	}
	pub fn is_compound(&self) -> bool {
		matches!(self, Self::Arr(_) | Self::Str(_))
	}
	pub fn try_iter(&self) -> Result<impl Iterator, VicErr> {
		match self {
			Self::Arr(arr) => Ok(arr.borrow().clone().into_iter()),
			Self::Str(s) => {
				let graphemes = s.graphemes(true).map(|g| Val::new_str(g.to_string())).collect::<VecDeque<_>>();
				Ok(graphemes.into_iter())
			}
			_ => Err(VicErr::Simple(format!("Value of type '{}' is not iterable", self.display_type())))
		}
	}
	pub fn try_into_iter(self) -> Result<impl Iterator<Item=Val>, VicErr> {
		match self {
			Self::Arr(arr) => Ok(arr.borrow().clone().into_iter()),
			Self::Str(s) => {
				let graphemes = s.graphemes(true).map(|g| Val::new_str(g.to_string())).collect::<VecDeque<_>>();
				Ok(graphemes.into_iter())
			}
			_ => Err(VicErr::Simple(format!("Value of type '{}' is not iterable", self.display_type())))
		}
	}
	pub fn try_from_pair(pair: ArcPair) -> Result<Self,VicErr> {
		match pair.as_rule() {
			Rule::array => {
				let mut elements = VecDeque::new();
				let elem_list = pair.into_inner().next().unwrap().into_inner();
				for elem in elem_list {
					elements.push_back(Self::Expr(Box::new(Expr::parse_expr(elem)?)));
				}
				Ok(Self::Arr(Rc::new(RefCell::new(elements))))
			}
			Rule::null => Ok(Self::Null),
			Rule::int => {
				let int = pair.as_str().parse::<isize>().unwrap();
				Ok(Self::Num(int))
			}
			Rule::dict => {
				let key_value_list = pair.into_inner().next().unwrap().into_inner();
				let mut map = HashMap::new();
				for key_value in key_value_list {
					let mut inner = key_value.into_inner();
					let name = inner.next().unwrap().as_str().to_string();
					let val = Val::Expr(Box::new(Expr::parse_expr(inner.next().unwrap())?));
					map.insert(name, val);
				}
				Ok(Self::Dict(Rc::new(RefCell::new(map))))
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
				Ok(Self::Closure(closure_args.into(), parsed.into()))
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
			Rule::expr => {
				let expr = Expr::parse_expr(pair)?;
				Ok(Self::Expr(Box::new(expr)))
			}
				_ => unreachable!("Unexpected rule: {:?}", pair.as_rule())
		}
	}
	pub fn add(&self, other: Val) -> Result<Self,VicErr> {
		match (self, &other) {
			(Self::Num(n1), Self::Num(n2)) => Ok(Self::Num(n1 + n2)),
			(Self::Str(s1), s2) => Ok(Self::Str(s1.to_string() + &s2.to_string())),
			(s1, Self::Str(s2)) => Ok(Self::Str(s2.to_string() + &s1.to_string())),
			(Self::Arr(arr), val) => {
				let mut arr_ref = arr.borrow_mut();
				arr_ref.push_back(val.clone());
				Ok(Self::Arr(arr.clone()))
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
			Self::Ref(val) => format!("ref<{}>", val.as_borrow().display_type()),
			Self::Err(_, val) => format!("error<{}>", val.borrow().display_type()),
			Self::BuiltinHandle(_) => "buffer_handle".to_string(),
			Self::Constructor(name, _) => format!("constructor<{name}>"),
			Self::Dict(dict) => {
				let dict = dict.borrow();
				if let Some(class_name) = dict.get("_classname") {
					class_name.to_string()
				} else {
					"dictionary".to_string()
				}
			}
			Self::Str(_) => "string".to_string(),
			Self::Num(_) => "number".to_string(),
			Self::Register(_) => "register".to_string(),
			Self::Var(_) => "variable".to_string(),
			Self::BoundClosure(_,_) |
			Self::Closure(_,_) => "closure".to_string(),
			Self::Arr(_) => "array".to_string(),
			Self::Bool(_) => "boolean".to_string(),
			Self::Regex(_) => "regex".to_string(),
			Self::Break |
			Self::Continue |
			Self::Null => "null".to_string(),
			Self::Expr(_) => "expression".to_string(),
		}
	}
	pub fn is_truthy(&self, vicut: &mut ViCut) -> bool {
		match self {
			Self::Ref(val) => {
				// If the value is a reference, we need to dereference it
				val.peel_refs().is_truthy(vicut)
			}
			Self::Constructor(_, _) => {
				// Constructors are always truthy, they represent a valid object
				true
			}
			Self::Err(_,_) => false,
			Self::BuiltinHandle(_) => {
				// This is a special case, we consider the buffer handle to be truthy
				// if it exists, which it always does.
				true
			}
			Self::Dict(dict) => !dict.borrow().is_empty(),
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
			Self::BoundClosure(_,_) => todo!(),
			Self::Closure(args, body) => todo!(),
			Self::Arr(arr) => !arr.borrow().is_empty(),
			Self::Bool(b) => *b,
			Self::Break |
			Self::Continue |
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
			Self::Ref(val) => {
				let val = val.peel_refs();
				write!(f, "{val}")
			}
			Self::Constructor(name, _) => write!(f, "constructor<{name}>"),
			Self::Err(span, msg) => {
				let msg = msg.borrow().to_string();
				let pest_err = expr_error(msg, span.clone());
				write!(f, "{pest_err}")
			}
			Self::BuiltinHandle(_) => {
				write!(f, "{{ builtin }}")
			}
			Self::Arr(arr) => {
				let inner = arr.borrow().iter()
					.map(|val| val.to_string())
					.collect::<Vec<_>>()
					.join(", ");
				write!(f, "[{inner}]")
			}
			Self::Dict(dict) => {
				let mut key_values = vec![];
				let dict_ref = dict.borrow();
				let mut class_name = None;
				for (key,value) in &*dict_ref {
					if key == "_classname" {
						class_name = Some(value.to_string());
						continue;
					}
					key_values.push(format!("{key}: {value}"))
				}
				let joined = key_values.join(", ");
				if let Some(class_name) = class_name {
					write!(f, "{class_name} {{{joined}}}")
				} else {
					write!(f, "{{{joined}}}")
				}
			}
			Self::Expr(_) => {
				write!(f, "{{ expression }}")
			}
			Self::Register(ch) => write!(f, "@{ch}"),
			Self::Var(v) => write!(f, "{v}"),
			Self::BoundClosure(_, _) |
			Self::Closure(_, _) => {
				write!(f, "{{ closure }}")
			}
			Self::Str(s) => write!(f, "{s}"),
			Self::Num(n) => write!(f, "{n}"),
			Self::Bool(b) => write!(f, "{b}"),
			Self::Regex(r) => write!(f, "{r}"),
			Self::Break |
			Self::Continue |
			Self::Null => write!(f, "null")
		}
	}
}
