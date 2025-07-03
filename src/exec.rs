//! This module contains the `ViCut` struct, which is the central container for state in the program.
//!
//! Everything that moves through this program passes through the `ViCut` struct at some point.
use std::cell::{Ref, RefCell, RefMut};
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
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
use crate::vic::libvic::{Builtin, Func, Var};
use crate::vic::parse::{Accessor, ArcSpan, BinOp, BoolOp, Command, Expr, ExprKind, Index, LogOp, RcVal, RpnItem, Val};
use crate::vicmd::{Bound, LineAddr, Word};
use crate::{complain_and_exit, validate_filename, ExecCtx, Opts};

use super::linebuf::{LineBuf, SelectAnchor, SelectMode};
use super::vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};
use super::modes::{CmdReplay, ModeReport, insert::ViInsert, ViMode, normal::ViNormal, replace::ViReplace, visual::ViVisual};


/// A scope guard for `ViCut` that automatically handles descending and ascending the scope stack
///
/// This struct automatically manages scoping by guaranteeing a scope pop upon return from an inner scope
/// This guarantee is leveraged by Rust's `Drop` trait.
/// When the `ScopeGuard` is dropped, it will call `ascend` on the `ViCut` instance,
///
/// Note: This struct holds a **raw pointer** to the `ViCut` instance (`*mut ViCut`). 
/// This is safe **only under the assumption** that:
/// - The `ScopeGuard` is created and dropped entirely within the lifetime of a `&mut ViCut`
/// - The `ViCut` instance is not moved or deallocated while the guard is alive
///
/// Violating these assumptions (e.g., storing the guard beyond the method scope, or creating it from an invalid reference)
/// can cause **undefined behavior** when the raw pointer is dereferenced in `drop()`.
///
/// As a result, `ScopeGuard` must **never escape the function or method** it is created in,
/// and should only be used inside methods that have exclusive access to the `ViCut` instance.
pub struct ScopeGuard {
	vicut: *mut ViCut,
}

impl ScopeGuard {
	pub fn new(vicut: &mut ViCut) -> Self {
		// You don't need to do `*mut vicut`, just cast
		let ptr = vicut as *mut ViCut;
		unsafe {
			(*ptr).descend();
		}
		Self { vicut: ptr }
	}
}

impl Drop for ScopeGuard {
	fn drop(&mut self) {
		assert!(!self.vicut.is_null());
		unsafe {
			(*self.vicut).ascend();
		}
	}
}

pub enum Call {
	Method { 
		self_val: Val,
		args: Rc<[Expr]>,
		func: Val,
	},
	Function {
		args: Rc<[Expr]>,
		func: Val,
	}
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
	pub variables: Vec<HashMap<String, Val>>,

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
		let builtins = Self::init_builtins();

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
			variables: vec![builtins, HashMap::new()],
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
				let Val::Var(var) = var else { return false };
				var == "_buffers"
			}
			_ => false
		}
	}
	pub fn read_var(&self, name: &str) -> Option<Val> {
		// Search the stack frames for the variable
		// We do this in reverse order, so that we get the most local variable
		for frame in self.variables.iter().rev() {
			if frame.contains_key(name) {
				return frame.get(name).cloned()
			}
		}
		None
	}
	pub fn get_var(&mut self, name: &str) -> Option<Val> {
		// Search the stack frames for the variable
		// We do this in reverse order, so that we get the most local variable
		let mut ret = None;
		for frame in self.variables.iter().rev() {
			if frame.contains_key(name) {
				ret = frame.get(name).cloned()
			}
		}
		if let Some(Val::BuiltinHandle(Builtin::Var(var))) = ret {
			self.get_builtin_var(var)
		} else {
			ret
		}
	}
	pub fn get_var_mut(&mut self, name: &str,) -> Option<&mut Val> {
		// Search the stack frames for the variable
		// We do this in reverse order, so that we get the most local variable
		for frame in self.variables.iter_mut().rev() {
			if frame.contains_key(name) {
				return frame.get_mut(name)
			}
		}
		None
	}
	pub fn set_var(&mut self, name: String, value: Val) -> Result<(),VicErr> {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.insert(name, value);
		Ok(())
	}
	pub fn clear_var(&mut self, name: &str) {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.remove(name);
	}
	pub fn run_shell_cmd(&mut self, cmd: &Expr) -> Result<Val,VicErr> {
		let cmd = self.eval_expr(false, cmd).try_blame(cmd.span())?.to_string();
		let mut outputs = HashMap::new();
		let output = std::process::Command::new("sh")
			.arg("-c")
			.arg(cmd)
			.output()
			.map_err(|e| format!("Failed to run shell command: {e}"))?;
		if !output.status.success() {
			return Err(VicErr::Simple(format!("Shell command failed with status: {}", output.status)))
		}
		// Shell commands return an array containing stdout as index 0 and stderr as index 1
		let stdout = Val::new_str(String::from_utf8_lossy(&output.stdout).trim_end().to_string());
		let stderr = Val::new_str(String::from_utf8_lossy(&output.stderr).trim_end().to_string());
		outputs.insert("stdout".to_string(),stdout);
		outputs.insert("stderr".to_string(), stderr);
		Ok(Val::new_dict(outputs))
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
								expanded.push_str(&var.to_string());
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
	pub fn introspect(&self) {
		self.debug_vars(|i, scope| {
			eprintln!("\x1b[1;31mscope {i}\x1b[0m:");
			for (k, v) in scope {
				if k.contains("ret") || v.contains("string!") {
					eprintln!("  \x1b[1;35m{k}\x1b[0m = {v}");
				}
			}
		});
	}
	pub fn debug_vars<F: FnMut(usize, &HashMap<String, String>)>(&self, mut f: F) {
		for (i, frame) in self.variables.iter().enumerate() {
			let map = frame.iter()
				.map(|(k, v)| (k.to_string(), v.to_string()))
				.collect::<HashMap<_, _>>();
			f(i, &map);
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
	pub fn eval_expr(&mut self, is_top_level: bool, cmd_expr: &Expr) -> Result<Val,VicErr> {
		let Expr { value, accessors, span } = cmd_expr;
		let eval = match value {
			ExprKind::WhileBlock {..}  |
			ExprKind::UntilBlock {..}  => self.eval_loop_block(value)?,
			ExprKind::WithBlock {..}   => self.eval_with_block(value)?,
			ExprKind::SwitchBlock {..} => self.eval_switch_block(value)?,
			ExprKind::IfBlock {..}     => self.eval_if_block(value)?,
			ExprKind::ForBlock {..}    => self.eval_for_block(value)?,
			ExprKind::CatchBlock {..}  => self.eval_catch_block(value)?,
			ExprKind::Value(_)         => self.eval_value(value)?,
			ExprKind::Opts(exprs)      => self.eval_opts(exprs)?,
			ExprKind::VarDec {..}      => self.eval_var_dec(value)?,
			ExprKind::VarMut {..}      => self.eval_var_mutation(value)?,
			ExprKind::Range {..}       => self.eval_range(value)?,
			ExprKind::BinExpr(rpn)     => self.eval_bin_expr(rpn)?,
			ExprKind::BoolExpr(rpn)    => self.eval_bool_expr(rpn)?,
			ExprKind::BoolNode {..}    => self.eval_bool_node(value)?,
			ExprKind::FuncDef {..}     => self.eval_func_def(is_top_level, value)?,
			ExprKind::ClassDef {..}    => self.eval_class_def(value)?,
			ExprKind::Command(_)       => self.eval_command(is_top_level, span.clone(), value)?,
			ExprKind::Block(_)         |
			ExprKind::Vic(_)           |
			ExprKind::TopLevel(_)      |
			ExprKind::Opt {..}         |
			ExprKind::CondBlock {..}   |
			ExprKind::CaseBlock {..}   => unreachable!(),
		};
		// Now apply accessors like indexes, function args, field names, etc
		let eval = self.access_val(eval, accessors)?;
		Ok(eval)
	}
	fn eval_range(&mut self, range: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::Range { start, end } = range else {
			return Err(VicErr::Simple("Expected a range expression".into()));
		};
		let start_eval = self.eval_expr(false, start)
			.try_blame(start.span())?;
		let end_eval = self.eval_expr(false, end)
			.try_blame(end.span())?;

		let Val::Num(start_num) = start_eval else {
			return Err(VicErr::Simple("Expected a number as the start of the range".into()));
		};
		let Val::Num(end_num) = end_eval else {
			return Err(VicErr::Simple("Expected a number as the end of the range".into()));
		};

		match start_num.cmp(&end_num) {
			std::cmp::Ordering::Less => {
				// Create a range from start to end
				let range = ((start_num)..=(end_num)).map(Val::Num);
				let mut range_deque = VecDeque::new();
				range_deque.extend(range);
				Ok(Val::new_arr(range_deque))
			}
			std::cmp::Ordering::Equal => {
				Ok(Val::new_arr(VecDeque::new()))
			}
			std::cmp::Ordering::Greater => {
				// Reverse range, we can just swap the start and end

				let range = (end_num..=start_num).rev().map(Val::Num);
				let mut range_deque = VecDeque::new();
				range_deque.extend(range);
				Ok(Val::new_arr(range_deque))
			}
		}
	}
	fn eval_func_def(&mut self, is_top_level: bool, func_def: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::FuncDef { name, params, body } = func_def else { unreachable!() };

		// Functions are just fancy variables in vic
		// 'def function(arg1,arg2) { body }' is desugared into
		// 'let function = |arg1,arg2| { body }'
		let val = Val::Closure(params.to_vec().into(), body.to_vec().into());
		if is_top_level {
			self.set_var(name.to_string(), val)?;
			Ok(Val::Null)
		} else {
			Ok(val)
		}
	}
	fn eval_class_def(&mut self, class_def: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::ClassDef { name, fields } = class_def else { unreachable!() };

		// 'fields' is full of expressions right now
		// let's evaluate them
		let mut map_eval = HashMap::new();

		for (k,v) in fields.iter() {
			let eval = self.eval_expr(false, v)
				.try_blame(v.span())?;

			map_eval.insert(k.to_string(), eval);
		}

		// Classes are also just fancy variables
		// Internally they are just a named dictionary you can easily spawn instances of
		// Since functions are also just fancy variables, they fit right into the class fields as methods
		let class = Val::Dict(Rc::new(RefCell::new(map_eval)));
		self.set_var(name.to_string(), class)?;
		Ok(Val::Null)
	}
	fn eval_bool_node(&mut self, node: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::BoolNode { op, left, right } = node else {
			return Err(VicErr::Simple("Expected a boolean node expression".into()));
		};
		let left_val = self.eval_expr(false, left)
			.try_blame(left.span())?.is_truthy(self);

		if left_val && matches!(op, LogOp::Or) {
			// Short-circuit OR
			return Ok(Val::Bool(true));
		} else if !left_val && matches!(op, LogOp::And) {
			// Short-circuit AND
			return Ok(Val::Bool(false));
		}
		let right_val = self.eval_expr(false, right)
			.try_blame(right.span())?.is_truthy(self);
		Ok(Val::Bool(right_val))
	}
	fn eval_var_mutation(&mut self, varmut: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::VarMut { name, op, value } = varmut else {
			return Err(VicErr::Simple("Expected a variable mutation expression".into()));
		};

		// This is a raw pointer
		// the compiler really hates grabbing mut borrows
		// from nested stuff like "dict.field[0]"
		// so we have to use Evil Rust to cleanly mutate vars
		let ptr = self.access_val_ptr(name)?;
		let new_val = self.eval_expr(false, value)
			.try_blame(value.span())?;

		unsafe {
			let var = &mut *ptr;
			let result = if let Some(op) = op {
				match op {
					BinOp::Add => var.add(new_val.clone()),
					BinOp::Sub => var.sub(new_val.clone()),
					BinOp::Mult => var.mult(new_val.clone()),
					BinOp::Div => var.div(new_val.clone()),
					BinOp::Mod => var.modulo(new_val.clone()),
					BinOp::Pow => var.pow(new_val.clone()),
					BinOp::Equals => Ok(new_val.clone()),
				}?
			} else {
				new_val
			};
			*var = result;
		}

		Ok(Val::Null)
	}
	fn eval_var_dec(&mut self, vardec: &ExprKind) -> Result<Val,VicErr> {
		let ExprKind::VarDec { name, value } = vardec else {
			return Err(VicErr::Simple("Expected a variable declaration expression".into()));
		};
		let eval = self.eval_expr(false, value)
			.try_blame(value.span())?;
		self.set_var(name.to_string(), eval)?;

		Ok(Val::Null)
	}
	fn eval_command(&mut self, is_top_level: bool, span: ArcSpan, command: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::Command(cmd) = command else {
			return Err(VicErr::Simple("Expected a command expression".into()));
		};
		match cmd {
			Command::Continue => Err(VicErr::Continue(span)),
			Command::Break => Err(VicErr::Break(span)),
			Command::Global { pattern, block } => todo!(),
			Command::NotGlobal { pattern, block } => todo!(),
			Command::Move { motion } => self.eval_move(motion),
			Command::Cut { motion } => self.eval_cut(motion, is_top_level),
			Command::Repeat { count, block } => self.eval_repeat(count, block),
			Command::Yank { register, motion } => self.eval_yank(register, motion),
			Command::Include { path } => self.include_file(path),
			Command::ShellCmd { cmd } => self.run_shell_cmd(cmd),
			Command::Next => {
				self.exec_ctx.field_num = 0;
				let record = std::mem::take(&mut self.exec_ctx.fields);
				self.exec_ctx.fmt_lines.push(record);
				Ok(Val::Null)
			}
			Command::New(expr) => {
				let value = self.eval_expr(is_top_level, expr)
					.try_blame(expr.span())?;

				Ok(value.deep_clone())
			}
			Command::Ref(expr) => {
				let value = self.access_val_ptr(expr)?;
				Ok(Val::Ref(Box::new(value))) 
			}
			Command::Error(arc_span, expr) => {
				let err_val = self.eval_expr(is_top_level, expr)
					.try_blame(expr.span())?;
				Err(VicErr::Return(arc_span.clone(), Val::Err(arc_span.clone(), Box::new(err_val.into()))))
			}
			Command::Return { ret } => {
				let ret = if let Some(expr) = ret {
					self.eval_expr(is_top_level, expr)
						.try_blame(expr.span())?
				} else { Val::Null };
				Err(VicErr::Return(span, ret))
			}
		}
	}
	fn include_file(&mut self, path: &Expr) -> Result<Val, VicErr> {
		let path = self.eval_expr(false, path)
			.try_blame(path.span())?
			.to_string();
		validate_filename(&path)?;
		let src = std::fs::read_to_string(&path)
			.map_err(|e| VicErr::Simple(format!("Failed to read file '{path}': {e}")))?;
		let ExprKind::Vic(cmds) = Expr::parse_vic(Arc::new(src))?.into_value() else { unreachable!() };
		for cmd in cmds {
			self.eval_expr(false, &cmd)
				.try_blame(cmd.span())?;
		}
		Ok(Val::Null)
	}
	fn eval_move(&mut self, motion: &Expr) -> Result<Val, VicErr> {
		let motion_eval = self.eval_expr(false,motion).try_blame(motion.span())?.to_string();

		let start_pos = self.current_buffer().cursor.get();
		self.move_cursor(&motion_eval)?;
		let new_pos = self.current_buffer().cursor.get();
		Ok(Val::Bool(start_pos != new_pos))
	}
	fn eval_yank(&mut self, register: &Expr, motion: &Expr) -> Result<Val, VicErr> {
		let register_eval = self.eval_expr(false, register)
			.try_blame(register.span())?
			.to_string();
		if register_eval.len() != 1 {
			return Err(VicErr::Simple("Yank register must be a single character".to_string()));
		}
		let register = register_eval.chars().next().unwrap();

		let motion_eval = self.eval_expr(false, motion)
			.try_blame(motion.span())?
			.to_string();
		let field = self.read_field(&motion_eval)
			.map_err(|e| VicErr::Simple(format!("Failed to yank field: {e}")))?;
		let reg = RegisterName::new(Some(register), None);
		reg.write_to_register(field.into());
		Ok(Val::Null)
	}
	fn eval_repeat(&mut self, count: &Expr, block: &[Expr]) -> Result<Val, VicErr> {
		let count_eval = self.eval_expr(false, count)
			.try_blame(count.span())?
			.to_int()
			.map_err(|e| VicErr::Simple(format!("Failed to evaluate repeat count: {e}")))?;
		let Val::Num(count) = count_eval else { unreachable!() };
		let mut ret = Val::Null;
		for _ in 0..count {
			let _scope = ScopeGuard::new(self);
			for cmd in block {
				ret = self.eval_expr(true, cmd)
					.try_blame(cmd.span())?;
			}
		}
		Ok(ret)
	}
	fn eval_cut(&mut self, motion: &Expr, is_top_level: bool) -> Result<Val, VicErr> {
		let motion_eval = self.eval_expr(false, motion)
			.try_blame(motion.span())?
			.to_string();
		self.exec_ctx.field_num += 1;
		match self.read_field(&motion_eval) {
			Ok(field) => {
				let name = format!("{}",self.exec_ctx.field_num);
				if is_top_level {
					self.exec_ctx.fields.push((name, field.clone()));
					Ok(Val::Null)
				} else {
					Ok(Val::new_str(field))
				}
			}
			Err(e) => {
				eprintln!("vicut: {e}");
				Ok(Val::Bool(false))
			}
		}
	}
	fn eval_catch_block(&mut self, block: &ExprKind) -> Result<Val,VicErr> {
		let ExprKind::CatchBlock { scrutinee, catch_block, err_bind } = block else { unreachable!() };
		let mut ret = Val::Null;

		let eval = self.eval_expr(false, scrutinee)
			.try_blame(scrutinee.span())?;

		if let Val::Err(_, val) = eval {
			let _scope = ScopeGuard::new(self);
			if let Some(var) = err_bind {
				let reffed = Val::Ref(Box::new((*val).as_ptr()));
				self.set_var(var.to_string(),reffed)?;
			}
			for cmd in catch_block {
				ret = self.eval_expr(true, cmd)
					.try_blame(cmd.span())?;
			}
		} else {
			ret = eval
		}

		Ok(ret)
	}
	fn eval_with_block(&mut self, block: &ExprKind) -> Result<Val,VicErr> {
		let ExprKind::WithBlock { buffer, body } = block else { unreachable!() };
		let buffer = self.eval_expr(false, buffer).try_blame(buffer.span())?.to_string();
		let new_buf = LineBuf::new().with_initial(buffer, 0);
		self.buffers.push(new_buf);

		let _scope = ScopeGuard::new(self);
		let mut ret = Val::Null;
		for cmd in body {
			ret = self.eval_expr(true, cmd)
				.try_blame(cmd.span())?;
			}
		Ok(ret)
	}
	fn eval_loop_block(&mut self, block: &ExprKind) -> Result<Val, VicErr> {
		let (ExprKind::WhileBlock { cond, body } | ExprKind::UntilBlock { cond, body }) = block else { unreachable!() };
		let mut ret = Val::Null;
		let polarity = matches!(block, ExprKind::WhileBlock { .. });
		let should_run = |v: &mut ViCut,c,p: bool| -> Result<bool,VicErr> {
			let res = v.eval_expr(false, c)
				.try_blame(c.span())?
				.is_truthy(v);
			Ok(if p { res } else { !res })
		};

		while should_run(self,cond,polarity)? {
			let _scope = ScopeGuard::new(self);
			for cmd in body {
				ret = self.eval_expr(true, cmd)
					.try_blame(cmd.span())?;
				}
		}
		Ok(ret)
	}
	fn eval_for_block(&mut self, block: &ExprKind) -> Result<Val,VicErr> {
		let ExprKind::ForBlock { var_name, list, body } = block else { unreachable!() };
		let mut ret = Val::Null;
		let list = self.eval_expr(false, list)
			.try_blame(list.span())?;
		let list_iter = list.try_into_iter()?;

		for item in list_iter {
			let _scope = ScopeGuard::new(self);
			// Set the variable in the current scope
			self.set_var(var_name.clone(), item.clone())?;
			for cmd in body {
				ret = self.eval_expr(true, cmd)
					.try_blame(cmd.span())?;
				}
		}

		Ok(ret)
	}
	fn eval_switch_block(&mut self, block: &ExprKind) -> Result<Val,VicErr> {
		let ExprKind::SwitchBlock { scrutinee, case_blocks, default_block } = block else { unreachable!() };
		let scrutinee = self.eval_expr(false, scrutinee)
			.try_blame(scrutinee.span())?;
		let mut executed = false;
		let mut ret = Val::Null;

		for block in case_blocks {
			let ExprKind::CaseBlock { cond, body } = block.value() else { unreachable!() };
			let should_execute = cond.contains(&scrutinee);

			if should_execute {
				executed = true;
				let _scope = ScopeGuard::new(self);
				for cmd in body {
					ret = self.eval_expr(true, cmd)
						.try_blame(cmd.span())?;
					}
			}
		}

		if !executed && let Some(default_block) = default_block {
			let _scope = ScopeGuard::new(self);
			for cmd in default_block {
				ret = self.eval_expr(true, cmd)
					.try_blame(cmd.span())?;
				}
		}

		Ok(ret)
	}
	fn eval_if_block(&mut self, block: &ExprKind) -> Result<Val, VicErr> {
		let ExprKind::IfBlock { cond_blocks, else_block } = block else { unreachable!(); };
		let mut executed = false;
		let mut ret = Val::Null;
		for block in cond_blocks {
			let ExprKind::CondBlock { cond, body } = block.value() else { unreachable!() };

			let should_execute = self.eval_expr(false, cond)
				.try_blame(cond.span())?
				.is_truthy(self);

			if should_execute {
				executed = true;
				let _scope = ScopeGuard::new(self);
				for cmd in body {
					ret = self.eval_expr(true, cmd)
						.try_blame(cmd.span())?;
					}
			}
		}

		if !executed && let Some(else_block) = else_block {
			let _scope = ScopeGuard::new(self);
			for cmd in else_block {
				ret = self.eval_expr(true, cmd)
					.try_blame(cmd.span())?;
				}
		}
		Ok(ret)
	}
	fn eval_bool_expr(&mut self, rpn: &[RpnItem]) -> Result<Val, VicErr> {
		let mut stack: Vec<Val> = vec![];
		for item in rpn {
			match item {
				RpnItem::Val(expr) => {
					let val = self.eval_expr(false, expr).try_blame(expr.span())?;
					stack.push(val);
				}
				RpnItem::Not(expr) => {
					let val = self.eval_expr(false, expr).try_blame(expr.span())?;
					let truthy = !val.is_truthy(self);
					stack.push(Val::Bool(truthy));
				}
				RpnItem::BoolOp(op) => {
					let right = stack.pop().ok_or("Expected a value on the stack for boolean operation")?;
					let left = stack.pop().ok_or("Expected a value on the stack for boolean operation")?;

					let result = match op {
						BoolOp::Ne => left.cmp(&right, self) != Some(std::cmp::Ordering::Equal),
						BoolOp::Eq => left.cmp(&right, self) == Some(std::cmp::Ordering::Equal),
						BoolOp::Lt => left.cmp(&right, self) == Some(std::cmp::Ordering::Less),
						BoolOp::Gt => left.cmp(&right, self) == Some(std::cmp::Ordering::Greater),
						BoolOp::Lte => left.cmp(&right, self) != Some(std::cmp::Ordering::Greater),
						BoolOp::Gte => left.cmp(&right, self) != Some(std::cmp::Ordering::Less),
						BoolOp::Not => unreachable!(),
					};

					stack.push(Val::Bool(result));
				}
				_ => unreachable!(),
			}
		}

		assert_eq!(stack.len(), 1, "RPN Stack did not fold into a single value: {stack:?}");
		Ok(stack.pop().unwrap())
	}
	fn eval_bin_expr(&mut self, rpn: &[RpnItem]) -> Result<Val,VicErr> {
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
					let right = right.clone();
					let left = stack.pop().ok_or("Expected a value on the stack for binary operation")?;
					let left = left.clone();
					let result = match op {
						BinOp::Add => left.add(right)?,
						BinOp::Sub => left.sub(right)?,
						BinOp::Mult => left.mult(right)?,
						BinOp::Div => left.div(right)?,
						BinOp::Mod => left.modulo(right)?,
						BinOp::Pow => left.pow(right)?,
						_ => unreachable!()
					};
					stack.push(result);
				}
				_ => unreachable!()
			}
		}
		if stack.len() != 1 {
			return Err(VicErr::Simple(format!("Expected a single value on the stack after evaluation, found {}", stack.len())))
		}
		Ok(stack.pop().unwrap())
	}
	fn eval_opts(&mut self, opts: &[Expr]) -> Result<Val,VicErr> {
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
						if let Val::Str(path) = file_path {
							let path = PathBuf::from(path.borrow().clone());
							self.push_file(path);
						} else {
							return Err(VicErr::Simple(format!("Expected a string for file path, found {}", file_path.display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a file path argument for 'file' option".into()))
					}
				}
				"template" => {
					if let Some(arg) = arg {
						let template = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(template) = template {
							self.opts_mut().template = Some(template.borrow().to_string());
						} else {
							return Err(VicErr::Simple(format!("Expected a string for template, found {}", template.display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a template argument for 'template' option".into()))
					}
				}
				"delimiter" => {
					if let Some(arg) = arg {
						let delimiter = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(delimiter) = delimiter {
							self.opts_mut().delimiter = Some(delimiter.borrow().to_string());
						} else {
							return Err(VicErr::Simple(format!("Expected a string for delimiter, found {}", delimiter.display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a delimiter argument for 'delimiter' option".into()))
					}
				}
				"max_jobs" => {
					if let Some(arg) = arg {
						let max_jobs = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Num(num) = max_jobs {
							if num < 1 {
								return Err(VicErr::Simple("vicut: max jobs must be at least 1".into()));
							}
							self.opts_mut().max_jobs = Some(num as u32);
						} else {
							return Err(VicErr::Simple(format!("Expected a number for max jobs, found {}", max_jobs.display_type())))
						}
					} else {
						return Err(VicErr::Simple("Expected a max jobs argument for 'max_jobs' option".into()))
					}
				}
				"backup_ext" => {
					if let Some(arg) = arg {
						let backup_ext = self.eval_expr(false,arg).try_blame(arg.span())?;
						if let Val::Str(ext) = backup_ext {
							self.opts_mut().backup_extension = Some(ext.borrow().to_string());
						} else {
							return Err(VicErr::Simple(format!("Expected a string for backup extension, found {}", backup_ext.display_type())))
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
		Ok(Val::Null)
	}
	/// Access a value as a raw mutable pointer
	///
	/// Be careful! :)
	///
	/// This is safe because the ScopeGuard struct ensures variables stay alive for the entire duration of a scope
	fn access_val_ptr(&mut self, val: &Expr) -> Result<*mut Val, VicErr> {
		let Expr { value, accessors, span } = val;
		let ExprKind::Value(Val::Var(var)) = value else {
			return Err(VicErr::Full(span.clone(), "Invalid expression for variable assignment".into()))
		};
		let var_mut = self.get_var_mut(var).ok_or(VicErr::Full(span.clone(), format!("Variable '{var}' not found")))?;
		let mut val_mut = var_mut as *mut Val;
		val_mut = self.peel_refs(val_mut);
		for accessor in accessors {
			val_mut = self.peel_refs(val_mut);
			match accessor {
				Accessor::Field(field) => {
					let deref = unsafe { &mut *val_mut };
					let Val::Dict(map) = deref else {
						return Err(VicErr::Simple(format!("Expected a dictionary for field access, found {}", deref.display_type())))
					};
					let mut map = map.borrow_mut();
					let field_val = map.get_mut(field)
						.ok_or_else(|| VicErr::Simple(format!("Field '{field}' not found in dictionary")))?;
					val_mut = field_val as *mut Val;
				}
				Accessor::Index(idx) => {
					let deref = unsafe { &mut *val_mut };
					let Val::Arr(arr) = deref else {
						return Err(VicErr::Simple(format!("Expected an array for index access, found {}", deref.display_type())))
					};
					let Index::Single(idx) = idx else {
						return Err(VicErr::Simple("Invalid index value for assignment".into()))
					};
					let idx_eval = self.eval_expr(false, idx)
						.try_blame(idx.span())?.to_int()?;
					let Val::Num(idx) = idx_eval else {
						return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx_eval.display_type())))
					};

					let mut arr = arr.borrow_mut();
					let len = arr.len();
					let index_val = arr.get_mut(idx as usize)
						.ok_or_else(|| VicErr::Simple(format!("Index '{idx}' out of bounds for array of length {len}")))?;
					val_mut = index_val as *mut Val;
				}
				_ => {
					return Err(VicErr::Simple(format!("Cannot access a variable with accessor '{accessor:?}'")))
				}
			}
		}
		Ok(val_mut)
	}
	fn peel_refs(&mut self, mut val_ptr: *mut Val) -> *mut Val {
		loop {
			let val = unsafe { &mut *val_ptr };
			match val {
				Val::Ref(inner) => {
					val_ptr = **inner
				}
				_ => return val_ptr,
			}
		}
	}
	fn access_val(&mut self, val: Val, accessors: &[Accessor]) -> Result<Val,VicErr> {
		if accessors.is_empty() {
			return Ok(val);
		}
		let mut eval = val;
		let mut last_self: Option<Val> = None;
		for accessor in accessors {
			eval = match accessor {
				Accessor::ErrProp => {
					let is_err = matches!(eval, Val::Err(_,_));
					if is_err {
						let Val::Err(ref span,_) = eval else { unreachable!() };
						return Err(VicErr::Return(span.clone(), eval))
					} else {
						eval
					}
				}
				Accessor::Call(args) => {
					match eval {
						Val::Var(var) => {
							// If the value is a variable, we need to read it first
							let Some(var_val) = self.get_var(&var) else {
								return Err(VicErr::Simple(format!("Variable '{var}' not found")))
							};
							eval = var_val;

						}
						Val::BuiltinHandle(Builtin::Fn(Func::Method(method_name))) => {
							let Some(self_val) = last_self.as_mut() else {
								return Err(VicErr::Simple("Method call without a self value".into()));
							};
							let mut arg_eval = vec![];
							for arg in args.iter() {
								let eval = self.eval_expr(false, arg)
									.try_blame(arg.span())?;
								arg_eval.push(eval);
							}
							eval = self.dispatch_builtin_method(self_val, &method_name, arg_eval.into())?;
							continue
						}
						_ => {}
					}
					match eval {
						Val::Closure(_, _) => {
							let args = args.clone();
							let call = match last_self {
								Some(ref self_val) => Call::Method { self_val: self_val.clone(), args, func: eval.clone() },
								None => Call::Function { args, func: eval }
							};
							self.eval_call(call)?
						}
						Val::BuiltinHandle(Builtin::Fn(func)) => {
							let mut arg_eval = vec![];
							for arg in args.iter() {
								let eval = self.eval_expr(false, arg)
									.try_blame(arg.span())?;
								arg_eval.push(eval);
							}
							self.try_builtin_function(func, arg_eval.into())?
						}
						_ => {
							return Err(VicErr::Simple(format!("Type '{}' is not callable", eval.display_type())));
						}
					}
				}
				Accessor::Field(field) => {
					match eval {
						Val::Dict(ref map) => {
							last_self = Some(eval.clone());
							let map = map.borrow();
							let field = map.get(field)
								.ok_or_else(|| VicErr::Simple(format!("Field '{field}' not found in dictionary")))?;
							field.clone()
						}
						Val::Ref(ref val) => {
							match unsafe { &***val } {
								Val::Dict(map) => {
									last_self = Some(eval.clone());
									let map = map.borrow();
									let field = map.get(field)
										.ok_or_else(|| VicErr::Simple(format!("Field '{field}' not found in dictionary")))?;
									field.clone()
								}
								_ => {
									last_self = Some(eval.clone());
									Val::BuiltinHandle(Builtin::Fn(Func::Method(field.to_string())))
								}
							}
						}
						_ => {
							last_self = Some(eval.clone());
							Val::BuiltinHandle(Builtin::Fn(Func::Method(field.to_string())))
						}
					}
				}
				Accessor::Index(index) => {
					self.index_val(eval, index)?
				}
			};
		}
		Ok(eval)
	}
	fn eval_call(&mut self, call: Call) -> Result<Val,VicErr> {
		match call {
			Call::Method {
				self_val,
				args,
				func 
			} => {
				let Val::Closure(params, body) = func else {
					return Err(VicErr::Simple(format!("Expected a closure for method call, found {}", func.display_type())))
				};

				let self_pos = params.iter().position(|p| p == "self");
				let expected_given = if self_pos.is_some() {
					params.len() - 1 
				} else {
					params.len()
				};
				if expected_given != args.len() {
					return Err(VicErr::Simple(format!("Expected {} arguments, found {}", expected_given, args.len())));
				}
				let mut arg_eval = vec![];
				for arg in args.iter() {
					let eval = self.eval_expr(false, arg)
						.try_blame(arg.span())?;
					arg_eval.push(eval);
				}
				let _scope = ScopeGuard::new(self);
				// Set the parameters in the current scope
				if let Some(pos) = self_pos {
					self.set_var("self".to_string(), Val::Ref(Box::new(Box::into_raw(Box::new(self_val)))))?;
					let remaining = params
						.iter()
						.enumerate()
						.filter(|(i,_)| *i != pos)
						.map(|(_,p)| p)
						.collect::<Vec<_>>();

					for (param, arg) in remaining.iter().zip(arg_eval.iter()) {
						self.set_var(param.to_string(), arg.clone())?;
					}
				} else {
					for (param, arg) in params.iter().zip(arg_eval.iter()) {
						self.set_var(param.to_string(), arg.clone())?;
					}
				}

				let mut ret = Val::Null;
				for cmd in body.iter() {
					let result = self.eval_expr(true, cmd)
						.try_blame(cmd.span());
					match result {
						Ok(val) => ret = val,
						Err(VicErr::Return(_, val)) => return Ok(val),
						Err(e) => return Err(e),
					}
				}
				Ok(ret)
			}

			Call::Function { 
				args,
				func
			} => {
				let Val::Closure(params, body) = func else {
					return Err(VicErr::Simple(format!("Expected a closure for function call, found {}", func.display_type())))
				};
				if params.len() != args.len() {
					return Err(VicErr::Simple(format!("Expected {} arguments, found {}", params.len(), args.len())));
				}
				let mut arg_eval = vec![];
				for arg in args.iter() {
					let eval = self.eval_expr(false, arg)
						.try_blame(arg.span())?;
					arg_eval.push(eval);
				}

				let _scope = ScopeGuard::new(self);
				// Set the parameters in the current scope
				for (param, arg) in params.iter().zip(arg_eval.iter()) {
					self.set_var(param.to_string(), arg.clone())?;
				}
				let mut ret = Val::Null;
				for cmd in body.iter() {
					let result = self.eval_expr(true, cmd)
						.try_blame(cmd.span());
					match result {
						Ok(val) => ret = val,
						Err(VicErr::Return(_, val)) => return Ok(val),
						Err(e) => return Err(e),
					}
				}
				Ok(ret)
			}
		}
	}
	fn index_val(&mut self, val: Val, index: &Index) -> Result<Val,VicErr> {
		match val {
			Val::Str(ref str) => {
				let str = str.borrow();
				let graphemes = str.graphemes(true).collect::<Vec<&str>>();
				match index {
					Index::Single(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = idx else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.display_type())))
						};
						let Some(gr) = graphemes.get(idx as usize) else {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for string of length {}", str.len())))
						};
						Ok(Val::new_str(gr.to_string()))
					}
					Index::To(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = idx else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.display_type())))
						};
						let idx = idx as usize;
						if idx > str.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for string of length {}", str.len())))
						}
						let slice = graphemes.get(..idx).unwrap().join("");
						Ok(Val::new_str(slice))
					}
					Index::From(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = idx else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.display_type())))
						};
						let idx = idx as usize;
						if idx >= str.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for string of length {}", str.len())))
						}
						let slice = graphemes.get(idx..).unwrap().join("");
						Ok(Val::new_str(slice))
					}
					Index::Slice(start, end) => {
						let start = self.eval_expr(false, start).try_blame(start.span())?;
						let Val::Num(start) = start else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", start.display_type())))
						};
						let end = self.eval_expr(false, end).try_blame(end.span())?;
						let Val::Num(end) = end else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", end.display_type())))
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
								Ok(Val::new_str(slice))
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
								Ok(Val::new_str(slice))
							}
							Ordering::Equal => Ok(Val::new_str(String::new())),
						}
					}
					_ => unimplemented!()
				}
			}
			Val::Arr(ref arr) => {
				let arr = arr.borrow();
				match index {
					Index::Single(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = idx else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.display_type())))
						};
						let idx = idx as usize;
						if idx >= arr.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for array of length {}", arr.len())))
						}
						Ok(arr[idx].clone())
					}
					Index::To(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = idx else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.display_type())))
						};
						let idx = idx as usize;
						if idx > arr.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for array of length {}", arr.len())))
						}
						let slice = arr.iter().take(idx).cloned().collect::<VecDeque<_>>();
						Ok(Val::new_arr(slice))
					}
					Index::From(idx) => {
						let idx = self.eval_expr(false, idx).try_blame(idx.span())?;
						let Val::Num(idx) = idx else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", idx.display_type())))
						};
						let idx = idx as usize;
						if idx >= arr.len() {
							return Err(VicErr::Simple(format!("Index {idx} out of bounds for array of length {}", arr.len())))
						}
						let slice = arr.iter().skip(idx).cloned().collect::<VecDeque<_>>();
						Ok(Val::new_arr(slice))
					}
					Index::Slice(start, end) => {
						let start = self.eval_expr(false, start).try_blame(start.span())?;
						let Val::Num(start) = start else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", start.display_type())))
						};
						let end = self.eval_expr(false, end).try_blame(end.span())?;
						let Val::Num(end) = end else {
							return Err(VicErr::Simple(format!("Expected a number for index, found {}", end.display_type())))
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
								let len = end - start;
								let slice = arr.iter().skip(start).take(len).cloned().collect::<VecDeque<_>>();
								Ok(Val::new_arr(slice))
							}
							Ordering::Greater => {
								std::mem::swap(&mut start, &mut end);
								if end > arr.len() {
									return Err(VicErr::Simple(format!("Index {end} out of bounds for array of length {}", arr.len())))
								}
								if start >= arr.len() {
									return Err(VicErr::Simple(format!("Index {start} out of bounds for array of length {}", arr.len())))
								}
								let len = end - start;
								let slice = arr
									.iter()
									.rev()
									.skip(start)
									.take(len)
									.cloned()
									.collect::<VecDeque<_>>();
								Ok(Val::new_arr(slice))
							}
							Ordering::Equal => Ok(Val::new_arr(VecDeque::new())),
						}
					}
					_ => unimplemented!()
				}
			}
			_ => Err(VicErr::Simple(format!("Cannot index into type '{}'", val.display_type()))),
		}
	}
	/// Evaluate all contained values of a Val
	pub fn deep_eval_value(&mut self, val: &mut Val) -> Result<(),VicErr> {
		// Most of the time, Val::Expr(_) is evaluated lazily
		// but sometimes we want to know everything about a value like an Array or Dictionary
		// In this case we do our best to resolve all expressions inside the value
		let mut is_expr = false;
		match val {
			Val::Expr(_) => {
				// can't do any mutation while we are in this scope
				// at least we know it's a Val::Expr
				is_expr = true;
			}
			Val::Arr(arr) => {
				let mut arr = arr.borrow_mut();
				for item in arr.iter_mut() {
					self.deep_eval_value(item)?;
				}
			}
			Val::Dict(dict) => {
				let mut dict = dict.borrow_mut();
				for (_key, value) in dict.iter_mut() {
					self.deep_eval_value(value)?;
				}
			}
			Val::Var(var) => {
				// If we have a variable, we need to resolve it
				let Some(value) = self.get_var(var) else {
					return Err(VicErr::Simple(format!("Variable '{var}' not found")))
				};
				*val = value;
			}
			_ => {}
		}

		// we defer the evaluation until here
		// since the condition for reaching this branch doesn't require a mutable borrow
		if is_expr {

			// borrow scoping tricks
			let eval = {
				let Val::Expr(expr) = val else { unreachable!() };
				self.eval_expr(false, expr)?
			}; // borrow is dropped here

			// now we mutate
			*val = eval.clone();
		}
		Ok(())
	}
	/// A little bit redundant, but variables are kept in the Val enum
	fn eval_value(&mut self, val: &ExprKind) -> Result<Val,VicErr> {
		let ExprKind::Value(val) = val else {
			return Err(VicErr::Simple("Expected a value expression".into()));
		};
		match val {
			Val::Var(name) => {
				if name == "_buffer" {
					return Ok(Val::new_str(self.current_buffer().buffer.clone()))
				}
				let val = self.get_var(name).ok_or_else(|| format!("Variable '{name}' not found"))?;
				Ok(val)
			}
			Val::Str(str) => {
				let val = self.expand_literal(&str.borrow())?;
				Ok(Val::new_str(val))
			}
			Val::Expr(expr) => {
				let eval = self.eval_expr(false, expr)?;
				Ok(eval)
			}
			Val::Dict(map) => {
				let mut map = map.borrow_mut();
				for (_, value) in map.iter_mut() {
					self.deep_eval_value(value)?;
				}
				Ok(val.clone())
			}
			Val::Arr(arr) => {
				let mut arr = arr.borrow_mut();
				for item in arr.iter_mut() {
					self.deep_eval_value(item)?;
				}
				Ok(val.clone())
			}
			Val::Err(_, expr) => {
				let mut expr = expr.borrow_mut();
				self.deep_eval_value(&mut expr)?;
				Ok(val.clone())
			}
			_ => Ok(val.clone())
		}
	}
}
