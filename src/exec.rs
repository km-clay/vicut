//! This module contains the `ViCut` struct, which is the central container for state in the program.
//!
//! Everything that moves through this program passes through the `ViCut` struct at some point.
use std::cell::{Ref, RefCell, RefMut};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Display;
use std::fs;
use std::io::{self, BufRead, Write as IoWrite};
use std::fmt::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use log::{debug, trace};
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::keys::{KeyCode, KeyEvent, ModKeys};
use crate::linebuf::{ordered, ordered_signed, ClampedUsize, MotionKind};
use crate::modes::ex::ViEx;
use crate::modes::search::ViSearch;
use crate::reader::{KeyReader, RawReader};
use crate::register::{append_register, read_register, write_register, RegisterContent};
use crate::vic::error::{VicErr, VicErrResult};
use crate::vic::parse::{Accessor, BinOp, BoolOp, Command, Expr, ExprKind, Index, RcVal, RpnItem, Val};
use crate::vicmd::{Bound, LineAddr, Word};
use crate::{complain_and_exit, ExecCtx, Opts};

use super::linebuf::{LineBuf, SelectAnchor, SelectMode};
use super::vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};
use super::modes::{CmdReplay, ModeReport, insert::ViInsert, ViMode, normal::ViNormal, replace::ViReplace, visual::ViVisual};

/// shorthand for getting a mutable reference to ViCut's currently selected buffer
/// Takes, the self parameter, and the name you want to assign the reference to

#[derive(Debug, Clone)]
pub struct VicFunc {
	pub args: Vec<String>,
	pub body: Vec<Expr>,
}

pub struct ViCut {
	pub reader: RawReader,
	pub mode: Box<dyn ViMode>,
	pub repeat_action: Option<CmdReplay>,
	pub repeat_motion: Option<MotionCmd>,
	pub editor: ClampedUsize, // This is the index of the current buffer in the `buffers` vector
	pub buffers: Vec<LineBuf>,

	/// We use a vector as a stack here. Each entry is a scope.
	/// It starts with two hashmaps: one contains built-in variables
	/// and the other is the global scope accessible to the user
	/// This stack should never dip below length 2.
	pub variables: Vec<HashMap<String, RcVal>>,

	/// We also scope the runtime options
	/// This allows for scoped 'opts' blocks, e.g.
	/// ```vic
	/// if foo {
	///   opts { json }
	///   cut "5w"
	/// }
	/// ```
	pub opts: Vec<Opts>,
	pub cmds: Vec<Expr>,
	pub exec_ctx: ExecCtx,
}


impl ViCut {
	pub const BUILTINS: [&str;14] = [
		"_col",
		"_line",
		"_lines",
		"_pos",
		"_byte",
		"_buf_len",
		"_selection",
		"_buffer",
		"_word",
		"_WORD",
		"_is_eof",
		"_is_eol",
		"_is_bof",
		"_char"
	];
	pub const BUILTIN_FUNCS: [&str;4] = [
		"print",
		"env",
		"format",
		"type_of",
	];
	pub fn new(opts: Opts, input: String, cursor: usize) -> Result<Self,VicErr> {
		let vic_src = if let Some(ref raw) = opts.vic_raw {
			raw.to_string()
		} else if let Some(ref file) = opts.vic_file {
			fs::read_to_string(file).unwrap_or_else(complain_and_exit)
		} else {
			return Err("No vic source provided".into())
		};
		let ExprKind::Vic(cmds) = Expr::parse_vic(Arc::new(vic_src))?.into_value() else { unreachable!() };
		// i am hacker man
		let mut builtins = HashMap::new();
		builtins.insert("_buffer".to_string(), Val::BufferHandle.into());

		let mut new = Self {
			reader: RawReader::new(),
			mode: Box::new(ViNormal::new()),
			repeat_action: None,
			repeat_motion: None,
			editor: ClampedUsize::new(0, 1, true), // Index of the currently active buffer
																						 // ClampedUsize is used to ensure that the index is always within bounds
			buffers: vec![LineBuf::new().with_initial(input, cursor)],

																						 // Initialize the stack frames
																						 // The first is the "built-in" scope, which includes built-in variables
																						 // and standard library functions from "prelude.vic"
																						 // The second is the "global" scope, which is where user-defined variables and functions go
																						 // User definitions can shadow built-ins this way.
																						 // Never allow these vectors to dip below length 2.
			variables: vec![builtins,HashMap::new()],
			opts: vec![opts],
			exec_ctx: ExecCtx::default(),
			cmds,
		};
		new.eval_prelude()?; // Evaluate prelude options/imports
		Ok(new)
	}
	pub fn empty() -> Self {
		Self::new(Opts::default(),String::new(),0).unwrap()
	}
	pub fn opts(&self) -> &Opts {
		self.opts.last().expect("There is always at least one opts frame")
	}
	pub fn opts_mut(&mut self) -> &mut Opts {
		self.opts.last_mut().expect("There is always at least one opts frame")
	}
	pub fn exec_loop(&mut self) -> Result<(),VicErr> {
		loop {
			let Some(key) = self.reader.read_key() else {
				break
			};

			let Some(mut cmd) = self.mode.handle_key_fallible(key)? else {
				continue
			};
			cmd.alter_line_motion_if_no_verb();
			let return_to_normal = cmd.flags.contains(CmdFlags::EXIT_CUR_MODE);


			self.exec_cmd(cmd)?;
			if return_to_normal {
				self.set_normal_mode();
			}
		}
		if matches!(self.mode.report_mode(), ModeReport::Search | ModeReport::Ex)
			&& !self.mode.pending_seq().unwrap().is_empty() {
				// We have run out of keys with a pending sequence.
				// The user may have done something like "-c :%s/foo/bar/" and did not type the explicit "<CR>" to submit
				// Let's see if we get a command if we send the enter key for them :)
				if let Some(mut cmd) = self.mode.handle_key_fallible(KeyEvent(KeyCode::Char('\r'), ModKeys::NONE))? {
					cmd.alter_line_motion_if_no_verb();
					let return_to_normal = cmd.flags.contains(CmdFlags::EXIT_CUR_MODE);


					self.exec_cmd(cmd)?;
					if return_to_normal {
						self.set_normal_mode();
					}
				}
		}
		Ok(())
	}

	pub fn buffers_mut(&mut self) -> &mut Vec<LineBuf> {
		&mut self.buffers
	}
	
	pub fn buffers(&self) -> &[LineBuf] {
		&self.buffers
	}

	pub fn current_buffer(&mut self) -> &LineBuf {
		self.buffers.last().unwrap()
	}
	pub fn current_buffer_mut(&mut self) -> &mut LineBuf {
		self.buffers.last_mut().unwrap()
	}

	pub fn num_buffers(&self) -> usize {
		self.buffers.len()
	}

	pub fn push_buffer(&mut self, buffer: impl ToString) {
		let buf = buffer.to_string();
		let new_buffer = LineBuf::new().with_initial(buf, 0);

		self.buffers.push(new_buffer);

		self.editor.set_max(self.buffers.len());
	}

	pub fn current_buffer_index(&self) -> usize {
		self.editor.get()
	}

	pub fn pop_buffer(&mut self) -> String {
		let mut popped = self.buffers.pop().unwrap_or_default(); // Should never be empty, but just in case
		if self.buffers.is_empty() {
			self.buffers.push(LineBuf::new()); // Always keep at least one buffer
		}
		let len = self.buffers.len();
		self.editor.set_max(len);
		popped.take_buf()
	}

	pub fn read_field(&mut self, cmd: &str) -> Result<String,VicErr> {
		self.load_input(cmd);
		let mut start = self.current_buffer().cursor.get();
		let mut end;

		self.exec_loop()?;

		let new_pos_clamped = self.current_buffer().cursor;
		let new_pos = new_pos_clamped.get();
		end = new_pos;
		(start,end) = ordered(start, end);
		end += 1;



		if self.current_buffer().select_range().is_some() {
			// We are in visual mode if we've made it here
			// So we are going to use the editor's selected content
			Ok(self.current_buffer_mut().selected_content().unwrap())
		} else {
			if self.current_buffer().buffer.is_empty() {
				return Ok(String::new())
			}
			let start = ClampedUsize::new(start, self.current_buffer().cursor.cap(), true);
			let end = ClampedUsize::new(end, self.current_buffer().cursor.cap(), false);
			let start_pos = start.get();
			let end_pos = end.get();
			let slice = self.current_buffer_mut()
				.slice_inclusive(start_pos..=end_pos)
				.map(|slice| slice.to_string())
				.ok_or("Failed to slice buffer".into());
			if let Ok(slice) = slice.as_ref() {
				trace!("Cutting from start position to cursor: '{slice}'");
			} else {
				trace!("Failed to slice buffer from cursor motion");
			}
			slice
		}
	}

	pub fn move_cursor(&mut self, cmd: &str) -> Result<(),VicErr> {
		self.read_field(cmd).map(|_| ()) // Same logic, just ignore the returned range
	}

	pub fn load_input(&mut self, input: &str) {
		let bytes = input.as_bytes();
		self.reader.load_bytes(bytes);
	}

	pub fn set_normal_mode(&mut self) {
		let should_go_back_one = self.mode.report_mode() == ModeReport::Insert;
		self.mode = Box::new(ViNormal::new());
		let cur_buf = self.current_buffer_mut();
		cur_buf.stop_selecting();
		if should_go_back_one {
			let new_pos = cur_buf.cursor.ret_sub(1);
			// Leaving insert mode moves back one, but never crosses line boundaries
			if cur_buf.grapheme_at(new_pos).is_some_and(|gr| gr != "\n") {
				cur_buf.cursor.sub(1);
			}
			if cur_buf.should_handle_block_insert() {
				cur_buf.handle_block_insert();
			}
		}
	}

	fn handle_mode_transition(&mut self, cmd: ViCmd) -> Result<(),VicErr> {

		let mut select_mode = None;
		let mut is_insert_mode = false;
		let count = cmd.verb_count();
		if self.mode.report_mode() == ModeReport::Insert && self.current_buffer_mut().should_handle_block_insert() {
			self.current_buffer_mut().handle_block_insert();
		}
		let mut inserting_from_visual = false;
		let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
			Verb::Change |
			Verb::InsertModeLineBreak(_) |
			Verb::InsertMode => {
				is_insert_mode = true;
				inserting_from_visual = self.mode.report_mode() == ModeReport::Visual;

				Box::new(ViInsert::new().with_count(count as u16))
			}

			Verb::NormalMode => {
				Box::new(ViNormal::new())
			}

			Verb::ReplaceMode => {
				Box::new(ViReplace::new())
			}

			Verb::VisualModeSelectLast => {
				if self.mode.report_mode() != ModeReport::Visual {
					self.current_buffer_mut().start_selecting(SelectMode::Char(SelectAnchor::Start));
				}
				self.current_buffer_mut().inserting_from_visual = false;
				let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
				std::mem::swap(&mut mode, &mut self.mode);
				let should_clamp = self.mode.clamp_cursor();
				self.current_buffer_mut().set_cursor_clamp(should_clamp);

				return self.current_buffer_mut().exec_cmd(cmd)
			}
			Verb::VisualMode => {
				select_mode = Some(SelectMode::Char(SelectAnchor::Start));
				Box::new(ViVisual::new())
			}
			Verb::VisualModeLine => {
				select_mode = Some(SelectMode::Line(SelectAnchor::Start));
				Box::new(ViVisual::new())
			}
			Verb::VisualModeBlock => {
				select_mode = Some(self.current_buffer_mut().get_block_select());
				Box::new(ViVisual::new())
			}

			// For these two we will return early instead of doing all the other stuff.
			// This is to preserve the line buffer's state while we are entering a pattern in search mode
			// If we continue from here, visual mode selections will be lost for instance.
			Verb::ExMode => {
				let mut mode: Box<dyn ViMode> = Box::new(ViEx::new(self.current_buffer_mut().selected_lines()));
				self.current_buffer_mut().inserting_from_visual = false;
				std::mem::swap(&mut mode, &mut self.mode);

				return Ok(())
			}
			Verb::SearchMode(count,dir) => {
				let mut mode: Box<dyn ViMode> = Box::new(ViSearch::new(count,dir));
				self.current_buffer_mut().inserting_from_visual = false;
				std::mem::swap(&mut mode, &mut self.mode);

				return Ok(())
			}

			_ => unreachable!()
		};

		self.current_buffer_mut().inserting_from_visual = inserting_from_visual;

		std::mem::swap(&mut mode, &mut self.mode);

		if mode.is_repeatable() {
			self.repeat_action = mode.as_replay();
		}

		let should_clamp = self.mode.clamp_cursor();
		self.current_buffer_mut().set_cursor_clamp(should_clamp);
		self.current_buffer_mut().exec_cmd(cmd)?;

		if let Some(select_mode) = select_mode {
			self.current_buffer_mut().start_selecting(select_mode);
		} else {
			self.current_buffer_mut().stop_selecting();
		}
		if is_insert_mode {
			self.current_buffer_mut().mark_insert_mode_start_pos();
		} else {
			self.current_buffer_mut().clear_insert_mode_start_pos();
		}
		Ok(())
	}

	fn handle_cmd_repeat(&mut self, cmd: ViCmd) -> Result<(),VicErr> {

		let Some(replay) = self.repeat_action.clone() else {
			return Ok(())
		};
		let ViCmd { verb, .. } = cmd;
		let VerbCmd(count,_) = verb.unwrap();
		match replay {
			CmdReplay::ModeReplay { cmds, mut repeat } => {
				if count > 1 {
					repeat = count as u16;
				}
				for _ in 0..repeat {
					let cmds = cmds.clone();
					for cmd in cmds {
						self.current_buffer_mut().exec_cmd(cmd)?
					}
				}
			}
			CmdReplay::Single(mut cmd) => {
				if count > 1 {
					// Override the counts with the one passed to the '.' command
					if cmd.verb.is_some() {
						if let Some(v_mut) = cmd.verb.as_mut() {
							v_mut.0 = count
						}
						if let Some(m_mut) = cmd.motion.as_mut() {
							m_mut.0 = 1
						}
					} else {
						return Ok(()) // it has to have a verb to be repeatable, something weird happened
					}
				}
				self.current_buffer_mut().exec_cmd(cmd)?;
			}
			_ => unreachable!("motions should be handled in the other branch")
		}
		Ok(())
	}

	fn handle_motion_repeat(&mut self, cmd: ViCmd) -> Result<(),VicErr> {

		match cmd.motion.as_ref().unwrap() {
			MotionCmd(count,Motion::RepeatMotion) => {
				let Some(motion) = self.repeat_motion.clone() else {
					return Ok(())
				};
				let repeat_cmd = ViCmd {
					register: RegisterName::default(),
					verb: cmd.verb().cloned(),
					motion: Some(motion),
					raw_seq: format!("{count};"),
					flags: CmdFlags::empty()
				};
				self.current_buffer_mut().exec_cmd(repeat_cmd)
			}
			MotionCmd(count,Motion::RepeatMotionRev) => {
				let Some(motion) = self.repeat_motion.clone() else {
					return Ok(())
				};
				let mut new_motion = motion.invert_char_motion();
				new_motion.0 = *count;
				let repeat_cmd = ViCmd {
					register: RegisterName::default(),
					verb: cmd.verb().cloned(),
					motion: Some(new_motion),
					raw_seq: format!("{count},"),
					flags: CmdFlags::empty()
				};
				self.current_buffer_mut().exec_cmd(repeat_cmd)
			}
			_ => unreachable!()
		}
	}

	pub fn exec_cmd(&mut self, mut cmd: ViCmd) -> Result<(),VicErr> {

		if cmd.is_mode_transition() {
			return self.handle_mode_transition(cmd)

		} else if cmd.is_cmd_repeat() {
			return self.handle_cmd_repeat(cmd)

		} else if cmd.is_motion_repeat() {
			return self.handle_motion_repeat(cmd)

		} else if cmd.is_ex_global() {
			return self.exec_ex_global(cmd)

		} else if cmd.is_ex_normal() {
			return self.exec_ex_normal(cmd)

		}
		if cmd.is_repeatable() {
			if self.mode.report_mode() == ModeReport::Visual {
				// The motion is assigned in the line buffer execution, so we also have to assign it here
				// in order to be able to repeat it
				let range = self.current_buffer_mut().select_range().unwrap().clone();
				let motion = match self.current_buffer_mut().select_mode.as_ref().unwrap() {
					SelectMode::Char(_) => Motion::RangeInclusive(range),
					SelectMode::Line(_) |
					SelectMode::Block {..} => Motion::Range(range)
				};
				cmd.motion = Some(MotionCmd(1,motion))
			}
			self.repeat_action = Some(CmdReplay::Single(cmd.clone()));
		}

		if cmd.is_char_search() {
			self.repeat_motion = cmd.motion.clone()
		}

		let should_clamp = self.mode.clamp_cursor();
		self.current_buffer_mut().set_cursor_clamp(should_clamp);
		self.current_buffer_mut().exec_cmd(cmd.clone())?;

		if self.mode.report_mode() == ModeReport::Visual && cmd.verb().is_some_and(|v| v.1.is_edit()) {
			self.current_buffer_mut().stop_selecting();
			let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
			std::mem::swap(&mut mode, &mut self.mode);
		}
		Ok(())
	}

	// Easier to handle these out here
	fn exec_ex_global(&mut self, cmd: ViCmd) -> Result<(),VicErr> {

		let ViCmd { register, verb, motion, raw_seq, flags } = cmd;
		let MotionKind::Lines(lines) = self.current_buffer_mut().eval_motion(verb.as_ref().map(|vcmd| &vcmd.1), motion.unwrap()) else { unreachable!() };
		for line in lines {
			let Some((start,_)) = self.current_buffer_mut().line_bounds(line) else { break };
			self.current_buffer_mut().cursor.set(start);
			let new_cmd = ViCmd {
				register,
				verb: verb.clone(),
				motion: Some(MotionCmd(1, Motion::Line(LineAddr::Number(line + 1)))),
				raw_seq: raw_seq.clone(),
				flags,
			};
			self.exec_cmd(new_cmd)?;
		}

		Ok(())
	}
	fn exec_ex_normal(&mut self, cmd: ViCmd) -> Result<(),VicErr> {

		let ViCmd { register: _, verb, motion, raw_seq: _, flags: _ } = cmd;
		let VerbCmd(_,Verb::Normal(seq)) = verb.unwrap() else { unreachable!() };
		let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
		std::mem::swap(&mut self.mode, &mut mode);
		match motion.unwrap().1 {
			Motion::Line(addr) => {
				let line_no = self.current_buffer_mut().eval_line_addr(addr)
					.ok_or("Failed to evaluate line address".to_string())?;
				let (start,_) = self.current_buffer_mut().line_bounds(line_no)
					.ok_or(format!("Failed to get line bounds for line {line_no}"))?;
				self.current_buffer_mut().cursor.set(start);
				self.reader.push_bytes_front(seq.as_bytes());

				self.exec_loop()?;
			}
			Motion::LineRange(start, end) => {
				let start_ln = self.current_buffer_mut().eval_line_addr(start)
					.ok_or("Failed to evaluate line address".to_string())?;
				let end_ln = self.current_buffer_mut().eval_line_addr(end)
					.ok_or("Failed to evaluate line address".to_string())?;
				let (start_ln,end_ln) = ordered(start_ln, end_ln);

				for line in start_ln..=end_ln {
					let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
					std::mem::swap(&mut self.mode, &mut mode);

					let (start,_) = self.current_buffer_mut().line_bounds(line)
						.ok_or("Failed to evaluate line address".to_string())?;
					self.current_buffer_mut().cursor.set(start);
					self.reader.push_bytes_front(seq.as_bytes());

					self.exec_loop()?;
				}
			}
			_ => unreachable!()
		}
		std::mem::swap(&mut self.mode, &mut mode);
		Ok(())
	}
	pub fn descend(&mut self) {
		self.variables.push(HashMap::new());
		self.opts.push(Opts::default());
	}
	pub fn ascend(&mut self) {
		// Never pop the built-in/global scopes
		if self.variables.len() > 2 {
			self.variables.pop();
		}
		if self.opts.len() > 1 {
			self.opts.pop();
		}
	}
	pub fn expr_targets_buffers(&self, expr: &Expr) -> bool {
		match expr.value() {
			ExprKind::Value(var) => {
				let Val::Var(ref var) = *var.borrow() else { return false };
				var == "_buffers"
			}
			_ => false
		}
	}
	pub fn var_from_expr(&mut self, expr: &Expr) -> Result<RcVal, VicErr> {
		let ExprKind::Value(var) = expr.value() else {
			return Err(VicErr::Full(expr.span(), format!("Expected a variable, got {}", expr.span().as_str())))
		};
		let Val::Var(ref var) = *var.borrow() else {
			return Err(VicErr::Full(expr.span(), format!("Expected a variable, got {}", expr.span().as_str())))
		};
		let Some(var_val) = self.get_var(var) else {
			return Err(VicErr::Full(expr.span(), format!("Variable '{var}' not found")))
		};
		Ok(var_val)
	}
	pub fn read_var(&self, name: &str) -> Option<RcVal> {
		// Search the stack frames for the variable
		// We do this in reverse order, so that we get the most local variable
		for frame in self.variables.iter().rev() {
			if frame.contains_key(name) {
				return frame.get(name).cloned()
			}
		}
		None
	}
	pub fn get_var(&mut self, name: &str) -> Option<RcVal> {
		// Search the stack frames for the variable
		// We do this in reverse order, so that we get the most local variable
		for frame in self.variables.iter().rev() {
			if frame.contains_key(name) {
				return frame.get(name).cloned()
			}
		}
		if Self::BUILTINS.contains(&name) {
			// If the variable is a built-in, we return it as a Val::Str
			// This is to allow for built-in variables like 'col', 'line', etc.
			return self.get_builtin_var(name).clone()
		}
		None
	}
	pub fn set_var(&mut self, name: String, value: RcVal) -> Result<(),VicErr> {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.insert(name, value);
		Ok(())
	}
	pub fn mutate_var(&mut self, var: RcVal, op: Option<BinOp>, value: RcVal) -> Result<(),VicErr> {
		if let Some(op) = op {
			let value = {
				let borrowed_var = var.borrow();
				let borrowed_val = value.borrow();
				match op {
					BinOp::Add => borrowed_var.add(borrowed_val.clone()),
					BinOp::Sub => borrowed_var.sub(borrowed_val.clone()),
					BinOp::Mult => borrowed_var.mult(borrowed_val.clone()),
					BinOp::Div => borrowed_var.div(borrowed_val.clone()),
					BinOp::Mod => borrowed_var.modulo(borrowed_val.clone()),
					BinOp::Pow => borrowed_var.pow(borrowed_val.clone()),
					BinOp::Equals => Ok(borrowed_val.clone())
				}?
			};
			*var.borrow_mut() = value;
		} else {
			*var.borrow_mut() = value.borrow().clone();
		}
		Ok(())
	}
	pub fn clear_var(&mut self, name: &str) {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.remove(name);
	}
	pub fn eval_count(&mut self, count: &Expr) -> Result<usize,VicErr> {
		let val = self.eval_expr(false,count).try_blame(count.span())?;
		let Val::Num(ref n) = *val.borrow() else {
			return Err(VicErr::Full(count.span(), format!("Expected a number, got {}", val.borrow().display_type())))
		};
		Ok(*n as usize)
	}
	pub fn try_builtin_function(&mut self, name: &str, args: Vec<RcVal>) -> Result<RcVal,VicErr> {
		match name {
			"type_of" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("type_of expects exactly one argument".to_string()))
				}
				let arg = &args[0];
				Ok(Val::Str(arg.borrow().display_type()).into())
			}
			"env" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("env expects exactly one argument".to_string()))
				}
				let arg = &args[0];
				let Val::Str(ref var_name) = *arg.borrow() else {
					return Err(VicErr::Simple(format!("Expected string in env(), got {}", arg.borrow().display_type())))
				};
				let var_name = var_name.trim();
				let env_value = std::env::var(var_name).unwrap_or_default();
				Ok(Val::Str(env_value).into())
			}
			"print" => {
				let mut output = String::new();
				for arg in args {
					let arg_str = arg.borrow().to_string();
					output.push_str(&arg_str);
				}
				println!("{output}");
				Ok(Val::Null.into())
			}
			"format" => {
				let mut format = String::new();
				for arg in args {
					let arg_str = arg.borrow().to_string();
					format.push_str(&arg_str);
				}
				Ok(Val::Str(format).into())
			}
			_ => Err(VicErr::Simple(format!("Unknown built-in function: {name}")))
		}
	}
	pub fn run_shell_cmd(&mut self, cmd: &str) -> Result<RcVal,VicErr> {
		let mut outputs = vec![];
		let output = std::process::Command::new("sh")
			.arg("-c")
			.arg(cmd)
			.output()
			.map_err(|e| format!("Failed to run shell command: {e}"))?;
		if !output.status.success() {
			return Err(VicErr::Simple(format!("Shell command failed with status: {}", output.status)))
		}
		// Shell commands return an array containing stdout as index 0 and stderr as index 1
		let stdout = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
		let stderr = String::from_utf8_lossy(&output.stderr).trim_end().to_string();
		outputs.push(stdout);
		outputs.push(stderr);
		Ok(Val::Arr(outputs.into_iter().map(|output| Val::Str(output).into()).collect()).into())
	}
	pub fn expand_literal(&mut self, literal: &str) -> Result<String,VicErr> {
		let mut expanded = String::new();
		let mut var_name = String::new();
		let mut chars = literal.chars().peekable();
		while let Some(c) = chars.next() {
			match c {
				'\\' => {
					// Skip the next character
					if let Some(next) = chars.next() {
						match next {
							'$' |
							'"' => {
								// Dollar sign and double quotes are special cases
								// These are control characters in 'vic' strings, so we remove a layer of escaping
							}
							// Now we handle escape sequences
							'n' => {
								expanded.push('\n');
								continue
							}
							't' => {
								expanded.push('\t'); 
								continue
							}
							'r' => {
								expanded.push('\r');
								continue
							}
							_ => {
								expanded.push('\\');
							}
						}
						expanded.push(next);
					}
					continue
				}
				'$' => {
					match (chars.next(), chars.next()) {
						(Some('{'), Some('{')) => {
							// This is a variable
							let mut closed = false;
							while let Some(ch) = chars.next() {
								match ch {
									'}' if chars.peek() == Some(&'}') => {
										// End of variable
										closed = true;
										chars.next();
										break
									}
									_ => {
										var_name.push(ch);
									}
								}
							}
							if !closed {
								return Err(VicErr::Simple("Unmatched ${{".to_string()))
							}
							if let Some(var) = self.get_var(&std::mem::take(&mut var_name)) {
								expanded.push_str(&var.borrow().to_string());
							}
						}
						(ch1,ch2) => {
							// Not a variable, just push what we got
							expanded.push('$');
							if let Some(ch1) = ch1 { expanded.push(ch1); }
							if let Some(ch2) = ch2 { expanded.push(ch2); }
						}
					}
				}
				_ => {
					// Just a normal character
					expanded.push(c);
				}
			}
		}
		Ok(expanded)
	}
	pub fn push_file(&mut self, file: PathBuf) {
		if let Some(files) = self.find_opt_mut(|o| o.files.as_mut()) {
			files.push(file);
		} else {
			self.opts_mut().files = Some(vec![file])
		}
	}
	pub fn flatten_opts(&self) -> Opts {
		let mut flat = Opts::default();

		// We go in reverse, so that the most recent scoped options are filled first
		for frame in self.opts.iter().rev() {
			// We're going to be fancy and write a macro here
			// This will let us check and fill each field in a concise way
			macro_rules! fill_fields {
				($($field:ident),*) => {
					$(if flat.$field.is_none() {
						flat.$field = frame.$field.clone();
					})*
				};
			}
			fill_fields!(
				delimiter,
				json,
				trace,
				linewise,
				trim_fields,
				keep_mode,
				backup_files,
				single_thread,
				global_uses_line_numbers,
				no_input,
				silent,
				template,
				max_jobs,
				backup_extension,
				pipe_in,
				pipe_out,
				out_file,
				files
			);
		}
		flat
	}
	/// Find an option in the Opts scope stack
	///
	/// Since we use a stack to allow for scoped opts setting, it's not as simple as just asking if an option is set
	/// We have to check each 'Opts' in the stack to see if the option is set
	/// We do this in reverse so that we start with the most recent scope.
	pub fn find_opt<T: Clone>(&self, selector: impl Fn(&Opts) -> Option<T>) -> Option<T> {
		for frame in self.opts.iter().rev() {
			if let Some(val) = selector(frame) {
				return Some(val);
			}
		}
		None
	}
	pub fn find_opt_or_default<T: Clone + Default>(&self, selector: impl Fn(&Opts) -> Option<T>) -> T {
		self.find_opt(selector).unwrap_or_default()
	}
	pub fn find_opt_mut<T>(&mut self, selector: impl Fn(&mut Opts) -> Option<&mut T>) -> Option<&mut T> {
		for frame in self.opts.iter_mut().rev() {
			if let Some(val) = selector(frame) {
				return Some(val);
			}
		}
		None
	}
	pub fn load_commands(&mut self, cmds: Vec<Expr>) {
		self.cmds = cmds
	}
	pub fn format_output(&self) -> String {
		if self.find_opt_or_default(|o| o.json) {
			Ok(self.format_output_json())
		} else if self.find_opt(|o| o.template.clone()).is_some() {
			self.format_output_template()
		} else {
			Ok(self.format_output_standard())
		}.unwrap_or_else(complain_and_exit)
	}
	fn no_fields_extracted(&self) -> bool {
		let lines = &self.exec_ctx.fmt_lines;
		lines.len() == 1 && lines.first().is_some_and(|record| record.len() == 1 && record.first().is_some_and(|field| field.0 == "0"))
	}
	pub fn format_output_standard(&self) -> String {
		let mut lines = self.exec_ctx.fmt_lines.clone();
		let delimiter = self.find_opt(|o| o.delimiter.clone()).unwrap_or("\t".into());
		// Let's check to see if we are outputting the whole buffer
		if self.no_fields_extracted()  {
			// We performed len checks in no_fields_extracted(), so unwrap is safe
			// So let's double pop the 2d vector and grab the value of our only field
			lines.pop()
				.unwrap()
				.pop()
				.unwrap()
				.1
		} else {
			let mut fields = vec![];
			let mut records = vec![];
			let mut output = String::new();
			for line in lines {
				for field in line {
					fields.push(field.1);
				}
				// Join the fields by the delimiter
				// Also clear fields for the next line
				let record = std::mem::take(&mut fields).join(&delimiter);
				// Push the new string
				records.push(record);
			}
			for record in records {
				if record.ends_with('\n') {
					write!(output, "{record}").ok();
				} else {
					writeln!(output,"{record}").ok();
				}
			}
			output
		}
	}
	/// Format the output as JSON
	pub fn format_output_json(&self) -> String {
		use serde_json::{Map, Value};
		let lines = self.exec_ctx.fmt_lines.clone();
		if lines.is_empty() || lines.iter().all(|line| line.is_empty()) {
			return String::new();
		}
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
		serde_json::to_string_pretty(&json).unwrap()
	}
	/// Format the output according to the given format string
	///
	/// We use a state machine here to interpolate the fields
	/// The loop looks for patterns like {{1}} or {{foo}} to interpolate on
	pub fn format_output_template(&self) -> Result<String,VicErr> {
		let lines = self.exec_ctx.fmt_lines.clone();
		let template = self.find_opt(|o| o.template.clone()).expect("We already checked for this, right?");
		let mut field_name = String::new();
		let mut output = String::new();
		let mut cur_line = String::new();
		for line in lines {
			let mut chars = template.chars().peekable();
			while let Some(ch) = chars.next() {
				match ch {
					'\\' => {
						if let Some(esc_ch) = chars.next() {
							cur_line.push(esc_ch)
						}
					}
					'{' if chars.peek() == Some(&'{') => {
						chars.next();
						let mut closed = false;
						while let Some(ch) = chars.next() {
							match ch {
								'\\' => {
									if let Some(esc_ch) = chars.next() {
										field_name.push(esc_ch)
									}
								}
								'}' if chars.peek() == Some(&'}') => {
									chars.next();
									closed = true;
									break
								}
								_ => field_name.push(ch)
							}
						}
						if closed {
							let result = line
								.iter()
								.find(|(name,_)| name == &field_name)
								.map(|(_,field)| field);

							if let Some(field) = result {
								cur_line.push_str(field);
							} else {
								let mut e = String::new();
								writeln!(e,"Did not find a field called '{field_name}' for output template").ok();
								writeln!(e,"Captured field names were:").ok();
								for (name,_) in line {
									writeln!(e,"\t{name}").ok();
								}
								return Err(VicErr::Simple(e))
							}
						} else {
							cur_line.extend(field_name.drain(..));
						}
						field_name.clear();
					}
					_ => cur_line.push(ch)
				}
			}
			if !cur_line.is_empty() {
				writeln!(output,"{}",std::mem::take(&mut cur_line)).ok();
			}
		}
		Ok(output)
	}
	pub fn eval_closure(&mut self,
		self_val: Option<RcVal>,
		mut given_args: Vec<RcVal>,
		arg_names: Vec<String>,
		body: Vec<Expr>
	) -> Result<RcVal,VicErr> {
		let self_pos = arg_names.iter().position(|name| name == "self");
		if let Some(pos) = self_pos {
			if let Some(val) = self_val {
				// If 'self' is provided, we insert it at the position of 'self'
				given_args.insert(pos, val);
			} else {
				return Err(VicErr::Simple("'self' is a reserved parameter name".to_string()))
			}
		}
		if given_args.len() != arg_names.len() {
			let given_len = given_args.len();
			let expected_len = arg_names.len();
			return Err(VicErr::Simple(format!("Closure expects {expected_len} arguments, got {given_len}")))
		}
		let arg_pairs = arg_names.into_iter().zip(given_args);
		self.descend();
		for (name,value) in arg_pairs {
			self.set_var(name.clone(), value).map_err(|e| format!("In closure: {e}"))?;
		}
		let mut ret = Val::Null.into();
		for cmd in body {
			ret = self.eval_expr(true,&cmd).try_blame(cmd.span())?;
			if cmd.is_return() {
				self.ascend();
				return Ok(ret)
			}
		}
		self.ascend();
		Ok(ret)
	}
	pub fn eval_prelude(&mut self) -> Result<(),VicErr> {
		// We want to evaluate options and imports as basically 
		// the first thing we do after constructing a ViCut instance
		// So let's go ahead and do that

		// We'll take the cmds vector and turn it into a peekable iterator
		let mut cmds = std::mem::take(&mut self.cmds).into_iter().peekable();

		// Next, we'll evaluate stuff while the next command is either an opts block or an include command
		while let Some(ExprKind::Opts(_)) | Some(ExprKind::Command(Command::Include {..})) = cmds.peek().map(|cmd| cmd.value()) {
			let cmd = cmds.next().unwrap();
			self.eval_expr(false,&cmd)?;
		}

		// Collect the iterator now, evaluated expressions are consumed
		self.cmds = cmds.collect();

		Ok(())
	}
	pub fn eval_expr(&mut self, is_top_level: bool, cmd_expr: &Expr) -> Result<RcVal,VicErr> {
		let Expr { value: cmd, accessors, ..} = cmd_expr;
		let mut eval = match cmd {
			ExprKind::Command(Command::New(var_name)) => {
				// 'new' creates and returns a copy of another variable
				// it's main use is to create new instances of classes
				// but it can create a copy of any existing variable
				let Some(var) = self.read_var(var_name.trim()) else {
					return Err(VicErr::Full(cmd_expr.span(), format!("Variable '{var_name}' not found")))
				};
				if !accessors.is_empty() {
					return Err(VicErr::Full(cmd_expr.span(), "Cannot index into type 'variable'".into()))
				}
				var.borrow().deep_clone().into()
			}
			ExprKind::Command(Command::ShellCmd { cmd }) => {
				// Evaluate the shell command and execute it
				let command = self.eval_expr(false, cmd)?.borrow().to_string();
				let expanded = self.expand_literal(&command).try_blame(cmd_expr.span())?;
				self.run_shell_cmd(&expanded).try_blame(cmd_expr.span())?
			}
			ExprKind::Command(Command::BufSwitch { id }) => {
				let Val::Num(ref id) = self.eval_expr(false,id)?.borrow().clone() else {
					return Err(VicErr::Simple("vicut: expected a number for buffer ID".into()))
				}; 
				if *id >= self.num_buffers() as isize || *id < 0 {
					Val::Bool(false).into()
				} else {
					self.editor.set(*id as usize);
					Val::Bool(true).into()
				}
			}
			ExprKind::Command(Command::Include { path }) => {
				todo!()
			}
			ExprKind::Command(Command::BufId) => {
				// Get the current buffer's ID
				let buf_id = self.editor.get();
				if !accessors.is_empty() {
					return Err(VicErr::Full(cmd_expr.span(), "Cannot index into type 'integer'".into()))
				}
				Val::Num(buf_id as isize).into()
			}
			ExprKind::Command(Command::Push { stack, value }) => {
				let is_buffers = self.expr_targets_buffers(stack);
				let value = self.eval_expr(false,value).try_blame(value.span())?.clone();
				let stack_var = self.var_from_expr(stack).try_blame(stack.span())?;

				match *stack_var.borrow_mut() {
					_ if is_buffers => {
						self.push_buffer(value.borrow());
					}
					Val::Str(ref mut str) => {
						str.push_str(&value.borrow().to_string());
					}
					Val::Arr(ref mut arr) => {
						arr.push(value);
					}
					_ => return Err(VicErr::Full(
						stack.span(),
						format!("vicut: expected a list or string for variable '{stack:?}', found {}",stack_var.borrow())
					))
				}
				Val::Null.into()
			}
			ExprKind::Command(Command::Pop { stack }) => {
				let is_buffers = self.expr_targets_buffers(stack);
				let stack_var = self.var_from_expr(stack).try_blame(stack.span())?;
				let popped_value = match *stack_var.borrow_mut() {
					_ if is_buffers => Some(Val::Str(self.pop_buffer()).into()),
					Val::Str(ref mut str) => {
						let mut graphemes = str.graphemes(true);
						let popped = graphemes.next_back().map(|gr| Val::Str(gr.into()));
						let remainder = graphemes.collect::<String>();
						*str = remainder;
						popped.map(|val| val.into())
					}
					Val::Arr(ref mut arr) => {
						// We have to take() instead of popping in this case
						arr.pop()
					}
					_ => return Err(VicErr::Full(
						stack.span(),
						format!("vicut: expected a list or string for variable '{stack:?}', found {}",stack_var.borrow())
					))
				};

				if is_buffers && self.num_buffers() == 0 {
					self.push_buffer("");
				}
				popped_value.unwrap_or_default()
			}
			ExprKind::Command(Command::Break) => return Err(VicErr::Break(cmd_expr.span())),
			ExprKind::Command(Command::Continue) => return Err(VicErr::Continue(cmd_expr.span())),
			ExprKind::Command(Command::Yank { register, motion }) => {
				// Evaluate the arg and yank it into the given register
				let reg = self.eval_expr(false,register).try_blame(register.span())?.borrow().to_string()
					.chars().next().ok_or(VicErr::Full(register.span(), "vicut: expected a register name, found empty string".into()))?;
				let motion_eval = self.eval_expr(false,motion).try_blame(motion.span())?.borrow().to_string();

				let value = self.read_field(&motion_eval).try_blame(motion.span())?;

				// Uppercase register name means "append to the register"
				if reg.is_ascii_uppercase() {
					append_register(Some(reg), RegisterContent::Span(value.to_string()));
				} else {
					write_register(Some(reg), RegisterContent::Span(value.to_string()));
				}
				Val::Null.into()
			}
			ExprKind::Command(Command::Return { ret }) => {
				let Some(ret) = ret else {
					return Err(VicErr::Return(cmd_expr.span(),Val::Null.into()))
				};
				return Err(VicErr::Return(cmd_expr.span(),self.eval_expr(false,ret).try_blame(ret.span())?))
			}
			ExprKind::Command(Command::Echo { args }) => {
				if args.is_empty() {
					println!();
					return Ok(Val::Null.into());
				}
				let mut display_args = vec![];
				for arg in args {
					let value = self.eval_expr(false,arg).try_blame(arg.span())?;

					display_args.push(value.borrow().to_string());
				}
				let output = display_args.join(" ");
				if is_top_level {
					println!("{output}");
					Val::Null.into()
				} else {
					Val::Str(output).into()
				}
			}
			ExprKind::Command(Command::Repeat { count, block }) => {
				let n_repeats = self.eval_count(count).unwrap_or_else(complain_and_exit);
				self.descend(); // new scope
				for _ in 0..n_repeats {

					for r_cmd in block {
						// We use recursion so that we can nest repeats easily
						self.eval_expr(true,r_cmd);
					}
					if !self.find_opt(|o| o.keep_mode).unwrap_or(false) {
						self.set_normal_mode();
					}
				}
				self.ascend(); // leave scope
				Val::Null.into()
			}
			ExprKind::Command(Command::NotGlobal { pattern, block }) |
			ExprKind::Command(Command::Global { pattern, block }) => {
				/*
				let polarity = matches!(cmd, ExprKind::Command(Command::Global { .. }));
				let pattern = self.eval_expr(false,pattern).try_blame(pattern.span())?;
				let motion = match polarity {
					false  => Motion::NotGlobal(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern),
					true => Motion::Global(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern)
				};

				// Here we ask ViCut's editor directly to evaluate the Global motion for us.
				// LineBuf::eval_motion() *always* returns MotionKind::Lines() for Motion::Global/NotGlobal.
				let MotionKind::Lines(lines) = cur_buf.eval_motion(None, MotionCmd(1,motion)) else { unreachable!() };
				if !lines.is_empty() {
					// Positive branch
					for line in lines {
						let line_no = line;
						let field_num = if self.find_opt_or_default(|o| o.global_uses_line_numbers) {
							// If we are using line numbers, we need to set the field number to the line number
							line_no
						} else {
							self.exec_ctx.field_num
						};
						let Some((start,_)) = cur_buf.line_bounds(line) else { continue };
						// Set the cursor on the start of the line
						cur_buf.cursor.set(start);
						// Execute our commands
						self.exec_ctx.field_num = field_num;
						self.descend(); // new scope
						for cmd in block {
							self.eval_expr(true,cmd);
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
					}
				} 	
				Val::Null.into()
				*/ todo!()
			}
			ExprKind::Command(Command::Move { motion }) => {
				let motion_eval = self.eval_expr(false,motion).try_blame(motion.span())?.borrow().to_string();
				let cursor_pos = self.current_buffer_mut().cursor.get();
				if let Err(e) = self.move_cursor(&motion_eval) {
					return Err(e).try_blame(motion.span());
				}
				let new_pos = self.current_buffer_mut().cursor.get();
				Val::Bool(cursor_pos != new_pos).into()
			}
			ExprKind::Command(Command::Cut { motion }) => {
				let motion_eval = self.eval_expr(false,motion).try_blame(motion.span())?.borrow().to_string();
				self.exec_ctx.field_num += 1;
				match self.read_field(&motion_eval) {
					Ok(field) => {
						let name = format!("{}",self.exec_ctx.field_num);
						if is_top_level {
							self.exec_ctx.fields.push((name,field.clone()));
							Val::Null.into()
						} else {
							Val::Str(field).into()
						}
					}
					Err(e) => {
						eprintln!("vicut: {e}");
						Val::Bool(false).into()
					}
				}
			}
			ExprKind::Command(Command::Next) => {
				if self.find_opt_or_default(|o| o.trace) {
					trace!("Breaking field group with fields: ");
					for field in &mut self.exec_ctx.fields {
						let name = &field.0;
						let content = &field.1;
						trace!("\t{name}: {content}");
					}
				}
				self.exec_ctx.field_num = 0;
				if !self.exec_ctx.fields.is_empty() {
					self.exec_ctx.fmt_lines.push(std::mem::take(&mut self.exec_ctx.fields));
				}
				Val::Null.into()
			}
			ExprKind::ClassDef { name, fields } => {
				let mut fields = fields
					.clone()
					.into_iter()
					.map(|(name, expr)| {
						self.eval_expr(false, &expr).map(|val| (name, val))
					})
				.collect::<Result<HashMap<String, RcVal>, VicErr>>()?;
				fields.insert("_name".to_string(), Val::Str(name.to_string()).into());
				fields.insert("_type".to_string(), Val::Str("class".to_string()).into());
				let val = Val::Dict(fields).into();
				self.set_var(name.to_string(), val)?;
				Val::Null.into()
			}
			ExprKind::FuncDef { name, params, body } => {
				// Define a function
				if is_top_level {
					self.set_var(name.clone(), Val::Closure(params.clone(),body.clone()).into())?;
					Val::Null.into()
				} else {
					Val::Closure(params.clone(), body.clone()).into()
				}
			}

			ExprKind::FuncCall { name, args } => {
				// First we check if this is a method call using this weird hack
				let (self_val,method_name) = if matches!(name.accessors.last(), Some(Accessor::Field(_))) {
					let Some(Accessor::Field(field_name)) = name.accessors.last() else { unreachable!() };
					let method_name = Some(field_name.clone());
					let mut outer_val = name.clone(); // clone the name value
					outer_val.accessors.pop(); // throw out the last accessor
					(self.eval_expr(false, &outer_val).ok(),method_name) // evaluate the new last accessor
				} else {
					(None,None)
				};
				// Func calls use evaluated names, so that stuff like func_ptr_array[0](arg1,arg2) is valid
				let result = self.eval_expr(false, name).try_blame(name.span());
				let Ok(val) = result else {
					if Self::BUILTIN_FUNCS.contains(&name.span().as_str().trim()) {
						let func_args: Result<Vec<RcVal>, VicErr> = args
							.iter()
							.map(|arg| self.eval_expr(false,arg).try_blame(arg.span()))
							.collect();
						return self.try_builtin_function(name.span().as_str().trim(), func_args?);
					} else if let Some(val) = self_val {
						let func_args: Result<Vec<RcVal>, VicErr> = args
							.iter()
							.map(|arg| self.eval_expr(false,arg).try_blame(arg.span()))
							.collect();
						return self.dispatch_builtin_method(val, &method_name.unwrap(), func_args?)
					}
					return Err(VicErr::Full(
						name.span(),
						format!("Function '{}' not found", name.span().as_str())
					))
				};
				if let Val::Closure(ref arg_names, ref body) = *val.borrow() {
					// If we are here, we are working with an anonymous closure
					let func_args: Result<Vec<RcVal>, VicErr> = args
						.iter()
						.map(|arg| self.eval_expr(false,arg).try_blame(arg.span()))
						.collect();
					let res = self.eval_closure(self_val, func_args?, arg_names.to_vec(), body.to_vec());
					match res {
						Ok(val) => return Ok(val),
						Err(VicErr::Return(_,val)) => return Ok(val),
						Err(e) => return Err(e)
					}

				} else if let Val::Var(ref var) = *val.borrow() {
					// If we are here, we are working with a named, defined function
					if let Some(val) = self.read_var(var) {
						if let Val::Closure(ref arg_names, ref body) = *val.borrow() {
							// If we are here, we are working with a named closure
							let func_args: Result<Vec<RcVal>, VicErr> = args
								.iter()
								.map(|arg| self.eval_expr(false,arg).try_blame(arg.span()))
								.collect();
							let res = self.eval_closure(self_val, func_args?, arg_names.to_vec(), body.to_vec());
							match res {
								Ok(val) => return Ok(val),
								Err(VicErr::Return(_,val)) => return Ok(val),
								Err(e) => return Err(e)
							}
						} else {
							return Err(VicErr::Full(
								name.span(),
								format!("'{}' is not callable", name.span().as_str())
							))
						}
					} else {
						return Err(VicErr::Full(
								name.span(),
								format!("'{}' is not callable", name.span().as_str())
						))
					}
				} else {
					return Err(VicErr::Full(
						name.span(),
						format!("Function '{}' not found", name.span().as_str())
					))
				}
			}
			ExprKind::VarDec { name, value } => {
				let value = self.eval_expr(false,value).try_blame(value.span())?;
				self.set_var(name.clone(), value.clone()).try_blame(cmd_expr.span())?;
				Val::Null.into()
			}
			ExprKind::VarMut { name, op, value } => {
				let span = name.span();
				let name_raw = span
					.as_str()
					.split(" ")
					.next()
					.unwrap();
				let value = self.eval_expr(false,value).try_blame(value.span())?;
				if !accessors.is_empty() && accessors.len() > 1 {
					return Err(VicErr::Full(
							cmd_expr.span(),
							"Assigning to a variable with multiple indexes is not yet supported".to_string()
					));
				} else if Self::BUILTINS.contains(&name_raw) {
					self.set_builtin_var(name_raw, value)?;
					Val::Null.into()
				} else {
					let var = self.eval_expr(false, name).try_blame(name.span())?;
					self.mutate_var(var, op.clone(), value.clone()).try_blame(cmd_expr.span())?;
					Val::Null.into()
				}
			}
			ExprKind::SwitchBlock { scrutinee, case_blocks, default_block } => {
				let mut executed = false;
				let mut ret = Val::Null.into();
				let scrutinee_value = self.eval_expr(false,scrutinee).try_blame(scrutinee.span())?;
				for block in case_blocks {
					let ExprKind::CaseBlock { cond, body } = &block.value else {
						return Err(VicErr::Full(block.span(), "Expected a case block".to_string()));
					};
					let mut matches = false;
					for pattern in cond {
						matches = *pattern == *scrutinee_value.borrow();
						if matches { break }
					}
					if matches {
						executed = true;
						self.descend(); // new scope
						for cmd in body {
							ret = self.eval_expr(true,cmd).try_blame(cmd.span())?;
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
						break;
					}
				}
				if let Some(default) = default_block && !executed {
					self.descend(); // new scope
					for cmd in default {
						ret = self.eval_expr(false,cmd).try_blame(cmd.span())?;
						if !self.find_opt_or_default(|o| o.keep_mode) {
							self.set_normal_mode();
						}
					}
					self.ascend(); // leave scope
				}
				ret
			}
			ExprKind::WithBlock { buffer, body } => {
				let mut ret = Val::Null.into();
				let new_buf = self.eval_expr(false,buffer).try_blame(buffer.span())?.borrow().to_string();

				self.descend();
				self.buffers.push(LineBuf::new().with_initial(new_buf, 0));

				for cmd in body {
					ret = self.eval_expr(false,cmd)?;
					if !self.find_opt_or_default(|o| o.keep_mode) {
						self.set_normal_mode();
					}
				}

				self.ascend();
				self.buffers.pop();

				ret
			}
			ExprKind::IfBlock { cond_blocks, else_block } => {
				let mut executed = false;
				let mut ret = Val::Null.into();
				for block in cond_blocks {
					let Expr { value, .. } = block;
					let ExprKind::CondBlock { cond, body } = value else { unreachable!() };
					let cond_value = self.eval_expr(false,cond).try_blame(cond.span())?;
					let result = cond_value.borrow().is_truthy(self);
					if result {
						executed = true;
						self.descend(); // new scope
						for cmd in body {
							ret = self.eval_expr(true,cmd)?;
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
						break;
					}
				}

				if let Some(else_block) = else_block && !executed {
					self.descend(); // new scope
					for cmd in else_block {
						ret = self.eval_expr(false,cmd).try_blame(cmd.span())?;
						if !self.find_opt_or_default(|o| o.keep_mode) {
							self.set_normal_mode();
						}
					}
					self.ascend(); // leave scope
				}
				ret
			}
			ExprKind::ForBlock { var_name, list, body } => {
				let val = self.eval_expr(false,list).try_blame(list.span())?;
				let val_iter = val.borrow().clone().try_into_iter().try_blame(cmd_expr.span())?;

				let mut ret = Val::Null.into();
				'main: for item in val_iter {
					self.descend(); // new scope
					self.set_var(var_name.clone(), item).unwrap_or_else(complain_and_exit);

					for cmd in body {
						let res = self.eval_expr(true,cmd);

						match res {
							Ok(val) => ret = val,
							Err(VicErr::Break(_)) => {
								self.ascend();
								break 'main
							}
							Err(VicErr::Continue(_)) => {
								if !self.find_opt_or_default(|o| o.keep_mode) {
									self.set_normal_mode();
								}
								continue 'main
							}
							Err(e) => {
								self.ascend();
								return Err(e)
							}
						}

						if !self.find_opt_or_default(|o| o.keep_mode) {
							self.set_normal_mode();
						}
					}
					self.ascend(); // leave scope
				}
				ret
			}
			ExprKind::UntilBlock { cond, body } |
			ExprKind::WhileBlock { cond, body } => {
				// This is the function we will use to see if we are still running
				let running = |vicut: &mut ViCut| -> Result<bool,VicErr> {
					let result = vicut.eval_expr(false,cond).try_blame(cond.span())?.borrow().is_truthy(vicut); 
					if matches!(cmd, ExprKind::WhileBlock { .. }) {
						Ok(result)
					} else {
						Ok(!result)
					}
				};

				let mut ret = Val::Null.into();
				'main: while running(self)? {
					self.descend(); // new scope
					for cmd in body {
						let res = self.eval_expr(true,cmd);
						match res {
							Ok(val) => ret = val,
							Err(VicErr::Break(_)) => {
								self.ascend();
								break 'main
							}
							Err(VicErr::Continue(_)) => {
								if !self.find_opt_or_default(|o| o.keep_mode) {
									self.set_normal_mode();
								}
								continue 'main
							}
							Err(e) => {
								self.ascend();
								return Err(e)
							}
						}
						if !self.find_opt_or_default(|o| o.keep_mode) {
							self.set_normal_mode();
						}
					}
					self.ascend(); // leave scope
				}
				ret
			}
			ExprKind::Block(exprs) => todo!(),
			ExprKind::Value(val) => self.eval_value(val.clone()).try_blame(cmd_expr.span())?,
			ExprKind::Opts(exprs) => {
				self.eval_opts(exprs).try_blame(cmd_expr.span())?;
				Val::Null.into()
			}
			ExprKind::Opt { set, name, arg } => {
				// really shouldn't be here, but just in case...
				self.eval_opts(std::slice::from_ref(cmd_expr)).try_blame(cmd_expr.span())?;
				Val::Null.into()
			}
			ExprKind::Range { start, end } => todo!(),
			ExprKind::BinExpr(rpn) => self.eval_bin_expr(rpn).try_blame(cmd_expr.span())?,
			ExprKind::BoolExpr(rpn) => self.eval_bool_expr(rpn).try_blame(cmd_expr.span())?,
			_ => unimplemented!("Unimplemented expression kind: {cmd:?}")
		};
		eval = self.access_val(eval, accessors)?;
		Ok(eval)
	}
	fn eval_bool_expr(&mut self, rpn: &[RpnItem]) -> Result<RcVal, VicErr> {
		let mut stack: Vec<RcVal> = vec![];
		for item in rpn {
			match item {
				RpnItem::Val(expr) => {
					let val = self.eval_expr(false, expr).try_blame(expr.span())?;
					stack.push(val);
				}
				RpnItem::Not(expr) => {
					let val = self.eval_expr(false, expr).try_blame(expr.span())?;
					let truthy = !val.borrow().is_truthy(self);
					stack.push(Val::Bool(truthy).into());
				}
				RpnItem::BoolOp(op) => {
					let right = stack.pop().ok_or("Expected a value on the stack for boolean operation")?;
					let left = stack.pop().ok_or("Expected a value on the stack for boolean operation")?;

					let result = match op {
						BoolOp::Ne => left.borrow().cmp(&right.borrow(), self) != Some(std::cmp::Ordering::Equal),
						BoolOp::Eq => left.borrow().cmp(&right.borrow(), self) == Some(std::cmp::Ordering::Equal),
						BoolOp::Lt => left.borrow().cmp(&right.borrow(), self) == Some(std::cmp::Ordering::Less),
						BoolOp::Gt => left.borrow().cmp(&right.borrow(), self) == Some(std::cmp::Ordering::Greater),
						BoolOp::Lte => left.borrow().cmp(&right.borrow(), self) != Some(std::cmp::Ordering::Greater),
						BoolOp::Gte => left.borrow().cmp(&right.borrow(), self) != Some(std::cmp::Ordering::Less),
						BoolOp::And => left.borrow().is_truthy(self) && right.borrow().is_truthy(self),
						BoolOp::Or => left.borrow().is_truthy(self) || right.borrow().is_truthy(self),
						BoolOp::Not => unreachable!(),
					};

					stack.push(Val::Bool(result).into());
				}
				_ => unreachable!(),
			}
		}

		assert_eq!(stack.len(), 1, "RPN Stack did not fold into a single value: {stack:?}");
		Ok(stack.pop().unwrap())
	}
	fn eval_bin_expr(&mut self, rpn: &[RpnItem]) -> Result<RcVal,VicErr> {
		// Luckily pest does most of the invariant enforcing for this on it's own
		let mut stack = vec![];
		for item in rpn {
			match item {
				RpnItem::Val(val) => {
					let eval = self.eval_expr(false, val).try_blame(val.span())?;
					stack.push(eval);
				}
				RpnItem::BinOp(op) => {
					let right = stack.pop().ok_or("Expected a value on the stack for binary operation")?;
					let right = right.borrow().clone();
					let left = stack.pop().ok_or("Expected a value on the stack for binary operation")?;
					let left = left.borrow().clone();
					let result = match op {
						BinOp::Add => left.add(right)?,
						BinOp::Sub => left.sub(right)?,
						BinOp::Mult => left.mult(right)?,
						BinOp::Div => left.div(right)?,
						BinOp::Mod => left.modulo(right)?,
						BinOp::Pow => left.pow(right)?,
						_ => unreachable!()
					};
					stack.push(result.into());
				}
				_ => unreachable!()
			}
		}
		if stack.len() != 1 {
			return Err(VicErr::Simple(format!("Expected a single value on the stack after evaluation, found {}", stack.len())))
		}
		Ok(stack.pop().unwrap())
	}
	fn eval_opts(&mut self, opts: &[Expr]) -> Result<(),VicErr> {
		for opt in opts {
			let ExprKind::Opt { set, name, arg } = opt.value() else { unreachable!("Expected an Opt expression") };
			match name.as_str() {
				"json" => self.opts_mut().json = Some(*set),
				"linewise" => self.opts_mut().linewise = Some(*set),
				"serial" => self.opts_mut().single_thread = Some(*set),
				"trim_fields" => self.opts_mut().trim_fields = Some(*set),
				"keep_mode" => self.opts_mut().keep_mode = Some(*set),
				"global_uses_line_numbers" => self.opts_mut().global_uses_line_numbers = Some(*set),
				"edit_inplace" => self.opts_mut().edit_inplace = Some(*set),
				"backup" => self.opts_mut().backup_files = Some(*set),
				"trace" => self.opts_mut().trace = Some(*set),
				"no_input" => self.opts_mut().no_input = Some(*set),
				"silent" => self.opts_mut().silent = Some(*set),
				"file" => {
					if let Some(arg) = arg {
						let file_path = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(ref path) = *file_path.borrow() {
							let path = PathBuf::from(path);
							self.push_file(path);
						} else {
							return Err(VicErr::Simple(format!("Expected a string for file path, found {}", file_path.borrow().display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a file path argument for 'file' option".into()))
					}
				}
				"template" => {
					if let Some(arg) = arg {
						let template = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(ref template) = *template.borrow() {
							self.opts_mut().template = Some(template.to_string());
						} else {
							return Err(VicErr::Simple(format!("Expected a string for template, found {}", template.borrow().display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a template argument for 'template' option".into()))
					}
				}
				"delimiter" => {
					if let Some(arg) = arg {
						let delimiter = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(ref delimiter) = *delimiter.borrow() {
							self.opts_mut().delimiter = Some(delimiter.to_string());
						} else {
							return Err(VicErr::Simple(format!("Expected a string for delimiter, found {}", delimiter.borrow().display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a delimiter argument for 'delimiter' option".into()))
					}
				}
				"max_jobs" => {
					if let Some(arg) = arg {
						let max_jobs = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Num(num) = *max_jobs.borrow() {
							if num < 1 {
								return Err(VicErr::Simple("vicut: max jobs must be at least 1".into()));
							}
							self.opts_mut().max_jobs = Some(num as u32);
						} else {
							return Err(VicErr::Simple(format!("Expected a number for max jobs, found {}", max_jobs.borrow().display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a max jobs argument for 'max_jobs' option".into()))
					}
				}
				"backup_ext" => {
					if let Some(arg) = arg {
						let backup_ext = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(ref ext) = *backup_ext.borrow() {
							self.opts_mut().backup_extension = Some(ext.to_string());
						} else {
							return Err(VicErr::Simple(format!("Expected a string for backup extension, found {}", backup_ext.borrow().display_type())))
						}
					} else {
						return Err(VicErr::Simple("vicut: expected a backup extension argument for 'backup_ext' option".into()));
					}
				}
				"write" => todo!(),
				"pipe_in" => todo!(),
				"pipe_out" => todo!(),
				_ => unreachable!("Unknown option: {name}"),
			}
		}
		Ok(())
	}
	fn access_val(&mut self, val: RcVal, accessors: &[Accessor]) -> Result<RcVal,VicErr> {
		if accessors.is_empty() {
			return Ok(val);
		}
		let mut eval = val;
		for accessor in accessors {
			eval = match accessor {
				Accessor::Field(field) => {
					let Val::Dict(ref map) = *eval.borrow() else {
						return Err(VicErr::Simple(format!("Cannot access field '{}' on type '{}'", field, eval.borrow().display_type())))
					};
					if let Some(v) = map.get(field) {
						v.clone()
					} else {
						return Err(VicErr::Simple(format!("Field '{}' not found in value of type '{}'", field, eval.borrow().display_type())))
					}
				}
				Accessor::Index(index) => {
					self.index_val(eval, index)?
				}
			}
		}
		Ok(eval)
	}
	fn index_val(&mut self, val: RcVal, index: &Index) -> Result<RcVal,VicErr> {
		match *val.borrow() {
			Val::Str(ref str) => {
				let graphemes = str.graphemes(true).collect::<Vec<&str>>();
				match index {
					Index::Single(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = *idx.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.borrow().display_type())))
						};
						let Some(gr) = graphemes.get(idx as usize) else {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for string of length {}", str.len())))
						};
						Ok(Val::Str(gr.to_string()).into())
					}
					Index::To(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = *idx.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.borrow().display_type())))
						};
						let idx = idx as usize;
						if idx > str.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for string of length {}", str.len())))
						}
						let slice = graphemes.get(..idx).unwrap().join("");
						Ok(Val::Str(slice).into())
					}
					Index::From(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = *idx.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.borrow().display_type())))
						};
						let idx = idx as usize;
						if idx >= str.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for string of length {}", str.len())))
						}
						let slice = graphemes.get(idx..).unwrap().join("");
						Ok(Val::Str(slice).into())
					}
					Index::Slice(start, end) => {
						let start = self.eval_expr(false, start).try_blame(start.span())?;
						let Val::Num(start) = *start.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", start.borrow().display_type())))
						};
						let end = self.eval_expr(false, end).try_blame(end.span())?;
						let Val::Num(end) = *end.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", end.borrow().display_type())))
						};
						let mut start = start as usize;
						let mut end = end as usize;
						match start.cmp(&end) {
							Ordering::Less => {
								if end > str.len() {
									return Err(VicErr::Simple(format!("Index {end} out of bounds for string of length {}", str.len())))
								}
								if start >= str.len() {
									return Err(VicErr::Simple(format!("Index {start} out of bounds for string of length {}", str.len())))
								}
								let slice = graphemes.get(start..end).unwrap().join("");
								Ok(Val::Str(slice).into())
							}
							Ordering::Greater => {
								std::mem::swap(&mut start, &mut end);
								if end > str.len() {
									return Err(VicErr::Simple(format!("Index {end} out of bounds for string of length {}", str.len())))
								}
								if start >= str.len() {
									return Err(VicErr::Simple(format!("Index {start} out of bounds for string of length {}", str.len())))
								}
								let len = start - end;
								let slice = graphemes
									.into_iter()
									.rev()
									.skip(start)
									.take(len)
									.collect::<Vec<_>>().join("");
								Ok(Val::Str(slice).into())
							}
							Ordering::Equal => Ok(Val::Str(String::new()).into()),
						}
					}
					_ => unimplemented!()
				}
			}
			Val::Arr(ref arr) => {
				match index {
					Index::Single(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = *idx.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.borrow().display_type())))
						};
						let idx = idx as usize;
						if idx >= arr.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for array of length {}", arr.len())))
						}
						Ok(arr[idx].clone())
					}
					Index::To(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = *idx.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.borrow().display_type())))
						};
						let idx = idx as usize;
						if idx > arr.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for array of length {}", arr.len())))
						}
						let slice = arr[..idx].to_vec();
						Ok(Val::Arr(slice).into())
					}
					Index::From(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = *idx.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.borrow().display_type())))
						};
						let idx = idx as usize;
						if idx >= arr.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for array of length {}", arr.len())))
						}
						let slice = arr[idx..].to_vec();
						Ok(Val::Arr(slice).into())
					}
					Index::Slice(start, end) => {
						let start = self.eval_expr(false, start).try_blame(start.span())?;
						let Val::Num(start) = *start.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", start.borrow().display_type())))
						};
						let end = self.eval_expr(false, end).try_blame(end.span())?;
						let Val::Num(end) = *end.borrow() else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", end.borrow().display_type())))
						};
						let mut start = start as usize;
						let mut end = end as usize;

						match start.cmp(&end) {
							Ordering::Less => {
								if end > arr.len() {
									return Err(VicErr::Simple(format!("Index {end} out of bounds for array of length {}", arr.len())))
								}
								if start >= arr.len() {
									return Err(VicErr::Simple(format!("Index {start} out of bounds for array of length {}", arr.len())))
								}
								let slice = arr[start..end].to_vec();
								Ok(Val::Arr(slice).into())
							}
							Ordering::Greater => {
								std::mem::swap(&mut start, &mut end);
								if end > arr.len() {
									return Err(VicErr::Simple(format!("Index {end} out of bounds for array of length {}", arr.len())))
								}
								if start >= arr.len() {
									return Err(VicErr::Simple(format!("Index {start} out of bounds for array of length {}", arr.len())))
								}
								let len = start - end;
								let slice = arr
									.iter()
									.rev()
									.skip(start)
									.take(len)
									.cloned()
									.collect::<Vec<_>>();
								Ok(Val::Arr(slice).into())
							}
							Ordering::Equal => Ok(Val::Arr(vec![]).into()),
						}
					}
					_ => unimplemented!()
				}
			}
			_ => Err(VicErr::Simple(format!("Cannot index into type '{}'", val.borrow().display_type()))),
		}
	}
	/// A little bit redundant, but variables are kept in the Val enum
	fn eval_value(&mut self, val: RcVal) -> Result<RcVal,VicErr> {
		match *val.borrow() {
			Val::Var(ref name) => {
				if name == "_buffer" {
					return Ok(Val::Str(self.current_buffer().buffer.clone()).into())
				}
				let val = self.get_var(name).ok_or_else(|| format!("Variable '{name}' not found"))?;
				return Ok(val)
			}
			Val::Str(ref str) => {
				let val = self.expand_literal(str)?;
				return Ok(Val::Str(val).into())
			}
			Val::Dict(_) => {

			}
			_ => return Ok(val.clone())
		}

		let Val::Dict(ref mut dict) = *val.borrow_mut() else { unreachable!() };
		// there may be unresolved Val::Expr()'s in the dictionary's fields. we need to evaluate them.
		for (_key, value) in dict.iter_mut() {
			let eval = if let Val::Expr(ref expr) = *value.borrow() {
				self.eval_expr(false, expr).try_blame(expr.span())?
			} else { 
				value.clone() 
			};
			*value = eval.borrow().deep_clone().into();
		}
		Ok(val.clone())
	}
}
