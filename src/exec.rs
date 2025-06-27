//! This module contains the `ViCut` struct, which is the central container for state in the program.
//!
//! Everything that moves through this program passes through the `ViCut` struct at some point.
use std::collections::HashMap;
use std::fmt::Display;
use std::io::{self, BufRead, Write as IoWrite};
use std::fmt::Write;
use std::path::PathBuf;
use std::rc::Rc;

use log::{debug, trace};
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::keys::{KeyCode, KeyEvent, ModKeys};
use crate::linebuf::{ordered, ordered_signed, ClampedUsize, MotionKind};
use crate::modes::ex::ViEx;
use crate::modes::search::ViSearch;
use crate::reader::{KeyReader, RawReader};
use crate::register::{append_register, read_register, write_register, RegisterContent};
use crate::vic::parse::{BinOp, Command, Expr, ExprKind, Val};
use crate::vicmd::{Bound, LineAddr, Word};
use crate::{blame_span, complain_and_exit, ExecCtx, Opts};

use super::linebuf::{LineBuf, SelectAnchor, SelectMode};
use super::vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};
use super::modes::{CmdReplay, ModeReport, insert::ViInsert, ViMode, normal::ViNormal, replace::ViReplace, visual::ViVisual};


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
	pub buffers: Vec<LineBuf>, // This is a vector of buffers, so we can have multiple buffers open at once
	pub editor: ClampedUsize, // This is the index of the current buffer in the `buffers` vector

	/// We use a vector of hashmaps here
	/// Each hashmap represents a "stack frame" of variables
	/// So you can shadow variables in vic
	/// The outer-most hashmap always contains the built-in variables
	pub variables: Vec<HashMap<String, Val>>,
	/// We do the same stack frame thing for functions
	/// we want all of our user definitions, variable or otherwise, to be scoped
	/// This way we can get away with having built-in functions *and* not reserving the function names
	pub functions: Vec<HashMap<String, VicFunc>>,

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
	exec_ctx: ExecCtx,
}


impl ViCut {
	const BUILTINS: [&str;12] = [
		"col",
		"line",
		"lines",
		"pos",
		"buf_len",
		"selection",
		"word",
		"WORD",
		"is_eof",
		"is_eol",
		"is_bof",
		"char"
	];
	pub fn new(input: String, cursor: usize) -> Result<Self,String> {
		Ok(Self {
			reader: RawReader::new(),
			mode: Box::new(ViNormal::new()),
			repeat_action: None,
			repeat_motion: None,
			buffers: vec![LineBuf::new().with_initial(input, cursor)], // We start with only the main buffer open
			editor: ClampedUsize::new(0, 1, true), // Index of the currently active buffer
																						 // ClampedUsize is used to ensure that the index is always within bounds

			// Initialize the stack frames
			// The first is the "built-in" scope, which includes built-in variables
			// and standard library functions from "prelude.vic"
			// The second is the "global" scope, which is where user-defined variables and functions go
			// User definitions can shadow built-ins this way.
			// Never allow these vectors to dip below length 2.
			variables: vec![HashMap::new(),HashMap::new()],
			functions: vec![HashMap::new(),HashMap::new()],
			opts: vec![Opts::default()],
			exec_ctx: ExecCtx::default(),
			cmds: vec![],
		})
	}
	pub fn empty() -> Self {
		Self::new(String::new(),0).unwrap()
	}
	pub fn opts(&self) -> &Opts {
		self.opts.last().expect("There is always at least one opts frame")
	}
	pub fn opts_mut(&mut self) -> &mut Opts {
		self.opts.last_mut().expect("There is always at least one opts frame")
	}
	pub fn exec_loop(&mut self) -> Result<(),String> {
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

	pub fn current_buffer(&mut self) -> &mut LineBuf {
		self.buffers.get_mut(self.editor.get())
			.expect("There should always be at least one buffer")
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
																				 // Similar to how Vim works interactively
		}
		self.editor.set_max(self.buffers.len());
		popped.take_buf()
	}

	pub fn read_field(&mut self, cmd: &str) -> Result<String,String> {
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
			Ok(self.current_buffer().selected_content().unwrap())
		} else {
			if self.current_buffer().buffer.is_empty() {
				return Ok(String::new())
			}
			let start = ClampedUsize::new(start, self.current_buffer().cursor.cap(), true);
			let end = ClampedUsize::new(end, self.current_buffer().cursor.cap(), false);
			let start_pos = start.get();
			let end_pos = end.get();
			let slice = self.current_buffer()
				.slice_inclusive(start_pos..=end_pos)
				.map(|slice| slice.to_string())
				.ok_or("Failed to slice buffer".to_string());
			if let Ok(slice) = slice.as_ref() {
				trace!("Cutting from start position to cursor: '{slice}'");
			} else {
				trace!("Failed to slice buffer from cursor motion");
			}
			slice
		}
	}

	pub fn move_cursor(&mut self, cmd: &str) -> Result<(),String> {
		self.read_field(cmd).map(|_| ()) // Same logic, just ignore the returned range
	}

	pub fn load_input(&mut self, input: &str) {
		let bytes = input.as_bytes();
		self.reader.load_bytes(bytes);
	}

	pub fn set_normal_mode(&mut self) {
		let should_go_back_one = self.mode.report_mode() == ModeReport::Insert;
		self.mode = Box::new(ViNormal::new());
		self.current_buffer().stop_selecting();
		if should_go_back_one {
			let new_pos = self.current_buffer().cursor.ret_sub(1);
			// Leaving insert mode moves back one, but never crosses line boundaries
			if self.current_buffer().grapheme_at(new_pos).is_some_and(|gr| gr != "\n") {
				self.current_buffer().cursor.sub(1);
			}
			if self.current_buffer().should_handle_block_insert() {
				self.current_buffer().handle_block_insert();
			}
		}
	}

	fn handle_mode_transition(&mut self, cmd: ViCmd) -> Result<(),String> {
		let mut select_mode = None;
		let mut is_insert_mode = false;
		let count = cmd.verb_count();
		if self.mode.report_mode() == ModeReport::Insert && self.current_buffer().should_handle_block_insert() {
			self.current_buffer().handle_block_insert();
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
					self.current_buffer().start_selecting(SelectMode::Char(SelectAnchor::Start));
				}
				self.current_buffer().inserting_from_visual = false;
				let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
				std::mem::swap(&mut mode, &mut self.mode);
				let should_clamp = self.mode.clamp_cursor();
				self.current_buffer().set_cursor_clamp(should_clamp);

				return self.current_buffer().exec_cmd(cmd)
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
				select_mode = Some(self.current_buffer().get_block_select());
				Box::new(ViVisual::new())
			}

			// For these two we will return early instead of doing all the other stuff.
			// This is to preserve the line buffer's state while we are entering a pattern in search mode
			// If we continue from here, visual mode selections will be lost for instance.
			Verb::ExMode => {
				let mut mode: Box<dyn ViMode> = Box::new(ViEx::new(self.current_buffer().selected_lines()));
				self.current_buffer().inserting_from_visual = false;
				std::mem::swap(&mut mode, &mut self.mode);

				return Ok(())
			}
			Verb::SearchMode(count,dir) => {
				let mut mode: Box<dyn ViMode> = Box::new(ViSearch::new(count,dir));
				self.current_buffer().inserting_from_visual = false;
				std::mem::swap(&mut mode, &mut self.mode);

				return Ok(())
			}

			_ => unreachable!()
		};

		self.current_buffer().inserting_from_visual = inserting_from_visual;

		std::mem::swap(&mut mode, &mut self.mode);

		if mode.is_repeatable() {
			self.repeat_action = mode.as_replay();
		}

		let should_clamp = self.mode.clamp_cursor();
		self.current_buffer().set_cursor_clamp(should_clamp);
		self.current_buffer().exec_cmd(cmd)?;

		if let Some(select_mode) = select_mode {
			self.current_buffer().start_selecting(select_mode);
		} else {
			self.current_buffer().stop_selecting();
		}
		if is_insert_mode {
			self.current_buffer().mark_insert_mode_start_pos();
		} else {
			self.current_buffer().clear_insert_mode_start_pos();
		}
		Ok(())
	}

	fn handle_cmd_repeat(&mut self, cmd: ViCmd) -> Result<(),String> {
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
						self.current_buffer().exec_cmd(cmd)?
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
				self.current_buffer().exec_cmd(cmd)?;
			}
			_ => unreachable!("motions should be handled in the other branch")
		}
		Ok(())
	}

	fn handle_motion_repeat(&mut self, cmd: ViCmd) -> Result<(),String> {
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
				self.current_buffer().exec_cmd(repeat_cmd)
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
				self.current_buffer().exec_cmd(repeat_cmd)
			}
			_ => unreachable!()
		}
	}

	pub fn exec_cmd(&mut self, mut cmd: ViCmd) -> Result<(),String> {
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
				let range = self.current_buffer().select_range().unwrap().clone();
				let motion = match self.current_buffer().select_mode.as_ref().unwrap() {
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
		self.current_buffer().set_cursor_clamp(should_clamp);
		self.current_buffer().exec_cmd(cmd.clone())?;

		if self.mode.report_mode() == ModeReport::Visual && cmd.verb().is_some_and(|v| v.1.is_edit()) {
			self.current_buffer().stop_selecting();
			let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
			std::mem::swap(&mut mode, &mut self.mode);
		}
		Ok(())
	}

	// Easier to handle these out here
	fn exec_ex_global(&mut self, cmd: ViCmd) -> Result<(),String> {
		let ViCmd { register, verb, motion, raw_seq, flags } = cmd;
		let MotionKind::Lines(lines) = self.current_buffer().eval_motion(verb.as_ref().map(|vcmd| &vcmd.1), motion.unwrap()) else { unreachable!() };
		for line in lines {
			let Some((start,_)) = self.current_buffer().line_bounds(line) else { break };
			self.current_buffer().cursor.set(start);
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
	fn exec_ex_normal(&mut self, cmd: ViCmd) -> Result<(),String> {
		let ViCmd { register: _, verb, motion, raw_seq: _, flags: _ } = cmd;
		let VerbCmd(_,Verb::Normal(seq)) = verb.unwrap() else { unreachable!() };
		let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
		std::mem::swap(&mut self.mode, &mut mode);
		match motion.unwrap().1 {
			Motion::Line(addr) => {
				let line_no = self.current_buffer().eval_line_addr(addr)
					.ok_or("Failed to evaluate line address".to_string())?;
				let (start,_) = self.current_buffer().line_bounds(line_no)
					.ok_or(format!("Failed to get line bounds for line {line_no}"))?;
				self.current_buffer().cursor.set(start);
				self.reader.push_bytes_front(seq.as_bytes());

				self.exec_loop()?;
			}
			Motion::LineRange(start, end) => {
				let start_ln = self.current_buffer().eval_line_addr(start)
					.ok_or("Failed to evaluate line address".to_string())?;
				let end_ln = self.current_buffer().eval_line_addr(end)
					.ok_or("Failed to evaluate line address".to_string())?;
				let (start_ln,end_ln) = ordered(start_ln, end_ln);

				for line in start_ln..=end_ln {
					let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
					std::mem::swap(&mut self.mode, &mut mode);

					let (start,_) = self.current_buffer().line_bounds(line)
						.ok_or("Failed to evaluate line address".to_string())?;
					self.current_buffer().cursor.set(start);
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
		self.functions.push(HashMap::new());
		self.opts.push(Opts::default());
	}
	pub fn ascend(&mut self) {
		// Never pop the built-in/global scopes
		if self.variables.len() > 2 {
			self.variables.pop();
		}
		if self.functions.len() > 2 {
			self.functions.pop();
		}
		if self.opts.len() > 1 {
			self.opts.pop();
		}
	}
	pub fn get_var_mut(&mut self, name: &str) -> Option<&mut Val> {
		// We have special handling for the "buffers" variable
		// in self.mutate_var(), so we don't need a case for it here
		for frame in self.variables.iter_mut().rev() {
			if frame.contains_key(name) {
				return frame.get_mut(name)
			}
		}
		None
	}
	pub fn read_var(&self, name: &str) -> Option<Val> {
		if name == "buffers" {
			// This is a reserved variable name, so we return it as a Val::Arr
			return Some(Val::Arr(self.buffers.iter().map(|buf| Val::Str(buf.buffer.clone())).collect()))
		}
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
		if name == "buffers" {
			// This is a reserved variable name, so we return it as a Val::Arr
			return Some(Val::Arr(self.buffers.iter().map(|buf| Val::Str(buf.buffer.clone())).collect()))
		}
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
	pub fn get_builtin_var(&mut self, name: &str) -> Option<Val> {
		if !Self::BUILTINS.contains(&name) {
			return None // Not a built-in variable
		}
		Some(match name {
			"col" => Val::Num((self.current_buffer().cursor_col() + 1) as isize),
			"line" => Val::Num((self.current_buffer().cursor_line_number() + 1) as isize),
			"lines" => Val::Num(self.current_buffer().total_lines() as isize),
			"pos" => Val::Num(self.current_buffer().cursor_byte_pos() as isize),
			"buf_len" => Val::Num(self.current_buffer().buffer.len() as isize),
			"selection" => Val::Str(self.current_buffer().selected_content().unwrap_or_default()),
			"word" => {
				let (word_start,word_end) = self.current_buffer().text_obj_word(1, Bound::Inside, Word::Normal).unwrap_or_default();
				let word_end = ClampedUsize::new(word_end, self.current_buffer().cursor.cap(), false).ret_add(1);
				self.current_buffer().slice_inclusive(word_start..=word_end)
					.map(|slice| Val::Str(slice.to_string()))
					.unwrap_or(Val::Str(String::new()))
			}
			"WORD" => {
				let (big_word_start,big_word_end) = self.current_buffer().text_obj_word(1, Bound::Inside, Word::Big).unwrap_or_default();
				let big_word_end = ClampedUsize::new(big_word_end, self.current_buffer().cursor.cap(), false).ret_add(1);
				self.current_buffer().slice_inclusive(big_word_start..=big_word_end)
					.map(|slice| Val::Str(slice.to_string()))
					.unwrap_or(Val::Str(String::new()))
			}
			"is_eof" => Val::Bool(self.current_buffer().cursor_at_max()),
			"is_eol" => Val::Bool(self.current_buffer().cursor_at_eol()),
			"is_bof" => Val::Bool(self.current_buffer().cursor.get() == 0),
			"char" => {
				self.current_buffer().grapheme_at_cursor()
					.map(|gr| Val::Str(gr.to_string()))
					.unwrap_or(Val::Str(String::new()))
			}
			_ => unreachable!()
		})
	}
	pub fn set_var(&mut self, name: String, value: Val) -> Result<(),String> {
		if &name == "buffers" {
			return Err("'buffers' is a reserved variable name and cannot be set".to_string())
		}
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.insert(name, value);
		Ok(())
	}
	pub fn mutate_var(&mut self, name: &str, op: Option<BinOp>, value: Val) -> Result<(),String> {
		if name == "buffers" {
			let None = op else {
				return Err("'buffers' cannot be used in math expressions".to_string())
			};
		}
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		let var = frame.entry(name.to_string()).or_insert(Val::Num(0));
		if let Some(op) = op {
			match (var, value) {
				(Val::Num(n), Val::Num(v)) => {
					match op {
						BinOp::Add => *n += v,
						BinOp::Sub => *n -= v,
						BinOp::Mult => *n *= v,
						BinOp::Div => *n /= v,
						BinOp::Mod => *n %= v,
						BinOp::Pow => *n = n.pow(v as u32),
						BinOp::Equals => *n = v
					};
				}
				_ => return Err(format!("Cannot apply operation {:?} to variable {}", op, name)),
			}
		} else {
			*var = value;
		}
		Ok(())
	}
	pub fn clear_var(&mut self, name: &str) {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.remove(name);
	}
	pub fn set_function(&mut self, name: String, args: Vec<String>, body: Vec<Expr>) {
		let Some(frame) = self.functions.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		let func = VicFunc {
			args,
			body,
		};
		frame.insert(name, func);
	}
	pub fn get_function(&self, name: &str) -> Option<&VicFunc> {
		for frame in self.functions.iter().rev() {
			if frame.contains_key(name) {
				return frame.get(name)
			}
		}
		None
	}
	pub fn clear_function(&mut self, name: &str) {
		let Some(frame) = self.functions.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.remove(name);
	}
	pub fn eval_count(&mut self, count: &Expr) -> Result<usize,String> {
		let val = self.eval_expr(count).ok_or("Expected a number".to_string())?;
		let Val::Num(n) = val else {
			return Err(format!("Expected a number, got {}", val.display_type()))
		};
		Ok(n as usize)
	}
	pub fn try_builtin_function(&mut self, name: &str, args: Vec<Val>) -> Result<Val,String> {
		match name {
			"type_of" => {
				if args.len() != 1 {
					return Err("type_of expects exactly one argument".to_string())
				}
				let arg = &args[0];
				Ok(Val::Str(arg.display_type()))
			}
			"env" => {
				if args.len() != 1 {
					return Err("env expects exactly one argument".to_string())
				}
				let arg = &args[0];
				let Val::Str(var_name) = arg else {
					return Err(format!("Expected string in env(), got {}",arg.display_type()))
				};
				let var_name = var_name.trim();
				let env_value = std::env::var(var_name).unwrap_or_default();
				Ok(Val::Str(env_value))
			}
			_ => Err(format!("Function {name} not found"))
		}
	}
	pub fn run_shell_cmd(&mut self, cmd: &str) -> Result<Val,String> {
		let mut outputs = vec![];
		let output = std::process::Command::new("sh")
			.arg("-c")
			.arg(cmd)
			.output()
			.map_err(|e| format!("Failed to run shell command: {e}"))?;
		if !output.status.success() {
			return Err(format!("Shell command failed with status: {}", output.status))
		}
		// Shell commands return an array containing stdout as index 0 and stderr as index 1
		let stdout = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
		let stderr = String::from_utf8_lossy(&output.stderr).trim_end().to_string();
		outputs.push(stdout);
		outputs.push(stderr);
		Ok(Val::Arr(outputs.into_iter().map(Val::Str).collect()))
	}
	pub fn expand_literal(&mut self, literal: &str) -> Result<String,String> {
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
								return Err("Unmatched ${{".to_string())
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
	pub fn parse_vic(&mut self, vic: Rc<String>) -> Result<(),String> {
		let result = Expr::parse_vic(vic)
			.map_err(|e| format!("vicut: {e}"))?;
		let ExprKind::Vic(cmds) = result.value else { unreachable!() };
		self.cmds = cmds;
		Ok(())
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
	pub fn format_output_template(&self) -> Result<String,String> {
		let mut lines = self.exec_ctx.fmt_lines.clone();
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
								return Err(e)
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
	fn exec_stdin(&mut self, input: Rc<String>) {
		let mut stdout = io::stdout().lock();
		let mut lines = vec![];
		self.parse_vic(input);
		match self.execute(None) {
			Ok(mut output) => {
				lines.append(&mut output);
			}
			Err(e) => eprintln!("vicut: {e}"),
		};
		let output = self.format_output();
		writeln!(stdout,"{output}").ok();

	}
	pub fn eval_function(&mut self, name: &str, given_args: Vec<Val>) -> Result<Option<Val>,String> {
		// First we need to search for variables in this scope, the user might be trying to execute a closure
		let VicFunc { args, body } = if let Some(closure) = self.read_var(name) {
			if let Val::Closure(args, body) = closure {
				// Let's create a new VicFunc using the Closure's data
				VicFunc { args, body }
			} else {
				return Err(format!("Expected a closure, found {}",closure.display_type()))
			}
		} else if let Some(func) = self.get_function(name) {
			// No closure, so we search for a function now
			func.clone()
		} else {
			// Nothing found by that name, return an error
			return Err(format!("Function '{name}' not found"))
		};
		if given_args.len() != args.len() {
			let given_len = given_args.len();
			let expected_len = args.len();
			return Err(format!("Function '{name}' expects {expected_len} arguments, got {given_len}"))
		}
		let arg_pairs = args.into_iter().zip(given_args.into_iter());
		self.descend();
		for cmd in body {
			for (name,value) in arg_pairs.clone() {
				self.set_var(name.clone(), value).map_err(|e| format!("In function '{name}': {e}"))?;
			}
			let ret = self.eval_expr(&cmd);
			if cmd.is_return() {
				return Ok(ret)
			}
		}
		self.ascend();
		Ok(None)
	}
	pub fn eval_expr(&mut self, cmd_expr: &Expr) -> Option<Val>{
		let Expr { value: cmd, index, ..} = cmd_expr;
		let eval = match cmd {
			ExprKind::Command(Command::ShellCmd { cmd }) => {
				// Evaluate the shell command and execute it
				todo!()
			}
			ExprKind::Command(Command::BufSwitch { id }) => {
				let Val::Num(id) = self.eval_expr(id).unwrap_or_else(|| blame_span(id.span(), "vicut: expected a number for buffer ID")) else {
					blame_span(id.span(), "vicut: expected a number for buffer ID")
				}; 
				self.editor.set(id as usize);
			}
			ExprKind::Command(Command::Include { path }) => {
				todo!()
			}
			ExprKind::Command(Command::BufId) => {
				// Get the current buffer's ID
				let buf_id = self.editor.get();
				if index.is_some() {
					blame_span(cmd_expr.span(), "Cannot index into type 'integer'")
				}
				return Some(Val::Num(buf_id as isize));
			}
			ExprKind::Command(Command::Push { stack, value }) => {
				let stack_var = self.eval_expr(stack).unwrap_or_else(|| blame_span(stack.span(), "vicut: Expected a stack for 'push' command"));
				let value = self.eval_expr(value).unwrap_or_else(|| blame_span(stack.span(), "vicut: invalid value for 'push' command") ).clone();
				if &stack_var.to_string() == "buffers" {
					// the 'buffers' variable is a built-in which holds all of the currently open buffers
					// so now we push the given data onto it as a new LineBuf
					self.push_buffer(value);
					return None
				}

				let stack_val = self.get_var_mut(&stack_var.to_string())
					.ok_or_else(|| format!("vicut: variable '{stack_var}' not found"))
					.unwrap_or_else(complain_and_exit);
				match stack_val {
					Val::Str(str) => {
						str.push_str(&value.to_string());
					}
					Val::Arr(arr) => {
						arr.push(value);
					}
					_ => blame_span(stack.span(), format!("vicut: expected a list or string for variable '{stack_var}', found {stack_val}"))
				}
			}
			ExprKind::Command(Command::Pop { stack }) => {
				let stack_var = self.eval_expr(stack).unwrap_or_else(|| blame_span(stack.span(), "Expected a stack for 'pop' command")).to_string();
				if &stack_var == "buffers" {
					// the 'buffers' variable is a built-in which holds all of the currently open buffers
					// so now we pop the last buffer off of it
					// we are in a command context, so we can ignore the return value
					self.pop_buffer();
					return None
				}
				let Some(stack_val) = self.get_var_mut(&stack_var) else {
					blame_span(stack.span(), format!("vicut: variable '{stack_var}' not found"))
				};

				let popped_value = match stack_val {
					Val::Str(str) => {
						let mut graphemes = str.graphemes(true);
						let popped = graphemes.next_back().map(|gr| Val::Str(gr.into()));
						let remainder = graphemes.collect::<String>();
						*str = remainder;
						popped
					}
					Val::Arr(arr) => {
						arr.pop()
					}
					_ => blame_span(stack.span(), format!("vicut: expected a list or string for variable '{stack_var}', found {stack_val}"))
				};
				return popped_value
			}
			ExprKind::Command(Command::Break) |
				ExprKind::Command(Command::Continue) => {
					// These are only checked for in loop contexts
					// We can just return
					return None
				}
			ExprKind::Command(Command::Yank { register, motion }) => {
				// Evaluate the arg and yank it into the given register
				let reg = self.eval_expr(register).unwrap_or_else(|| blame_span(register.span(), "Expected a register name for 'yank' command")).to_string()
					.chars().next().unwrap_or_else(|| blame_span(register.span(), format!("vicut: expected a register name, found empty string")));
				let span = motion.span();
				let motion_eval = self.eval_expr(motion).unwrap_or_else(|| blame_span(motion.span(), "Expected a motion for 'yank' command")).to_string();

				let value = self.read_field(&motion_eval).unwrap_or_else(|err| blame_span(motion.span(), err));

				// Uppercase register name means "append to the register"
				if reg.is_ascii_uppercase() {
					append_register(Some(reg), RegisterContent::Span(value.to_string()));
				} else {
					write_register(Some(reg), RegisterContent::Span(value.to_string()));
				}
			}
			ExprKind::Command(Command::Return { ret }) => {
				let Some(ret) = ret else {
					return Some(Val::Null)
				};
				// Evaluate the argument and return it
				// This is the only branch that returns a value
				let value = self.eval_expr(ret).unwrap_or_else(|| blame_span(ret.span(), "Failed to evaluate return value"));
				return Some(value)
			}
			ExprKind::Command(Command::Echo { args }) => {
				if args.is_empty() {
					println!();
					return None
				}
				let mut display_args = vec![];
				for arg in args {
					let value = self.eval_expr(arg).unwrap_or_else(|| blame_span(arg.span(), "Failed to evaluate echo arg"));

					display_args.push(value.to_string());
				}
				let output = display_args.join(" ");
				println!("{output}");
			}
			ExprKind::Command(Command::Repeat { count, block }) => {
				let n_repeats = self.eval_count(count).unwrap_or_else(complain_and_exit);
				self.descend(); // new scope
				for _ in 0..n_repeats {

					for r_cmd in block {
						// We use recursion so that we can nest repeats easily
						self.eval_expr(r_cmd);
					}
					if !self.find_opt(|o| o.keep_mode).unwrap_or(false) {
						self.set_normal_mode();
					}
				}
				self.ascend(); // leave scope
			}
			ExprKind::Command(Command::NotGlobal { pattern, block }) |
			ExprKind::Command(Command::Global { pattern, block }) => {
				let polarity = matches!(cmd, ExprKind::Command(Command::Global { .. }));
				let pattern = self.eval_expr(pattern).unwrap_or_else(|| blame_span(pattern.span(), "Failed to evaluate pattern for loop block"));
				let motion = match polarity {
					false  => Motion::NotGlobal(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern),
					true => Motion::Global(Box::new(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)), pattern)
				};

				// Here we ask ViCut's editor directly to evaluate the Global motion for us.
				// LineBuf::eval_motion() *always* returns MotionKind::Lines() for Motion::Global/NotGlobal.
				let MotionKind::Lines(lines) = self.current_buffer().eval_motion(None, MotionCmd(1,motion)) else { unreachable!() };
				if !lines.is_empty() {
					// Positive branch
					for line in lines {
						let mut line_no = line;
						let field_num = if self.find_opt_or_default(|o| o.global_uses_line_numbers) {
							// If we are using line numbers, we need to set the field number to the line number
							&mut line_no
						} else {
							&mut self.exec_ctx.field_num.clone()
						};
						let Some((start,_)) = self.current_buffer().line_bounds(line) else { continue };
						// Set the cursor on the start of the line
						self.current_buffer().cursor.set(start);
						// Execute our commands

						self.descend(); // new scope
						for cmd in block {
							self.eval_expr(cmd);
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
					}
				} 	
			}
			ExprKind::Command(Command::Move { motion }) => {
				let motion_eval = self.eval_expr(motion).unwrap_or_else(|| blame_span(motion.span(), "Expected a vim motion for 'move' command")).to_string();
				if let Err(e) = self.move_cursor(&motion_eval) {
					blame_span(motion.span(), e);
				}
			}
			ExprKind::Command(Command::Cut { motion }) => {
				let motion_eval = self.eval_expr(motion).unwrap_or_else(|| blame_span(motion.span(), "Expected a vim motion for 'cut' command")).to_string();
				self.exec_ctx.field_num += 1;
				match self.read_field(&motion_eval) {
					Ok(field) => {
						let name = format!("{}",self.exec_ctx.field_num);
						self.exec_ctx.fields.push((name,field))
					}
					Err(e) => {
						eprintln!("vicut: {e}");
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
			}
			ExprKind::FuncDef { name, params, body } => {
				// Define a function
				self.set_function(name.clone(), params.clone(), body.clone());
			}
			ExprKind::FuncCall { name, args } => {
				// Func calls use evaluated names, so that stuff like func_ptr_array[0](arg1,arg2) is valid
				let name = self.eval_expr(name).unwrap_or_else(|| blame_span(name.span(), "Invalid name for function call")).to_string();
				let func_args = args
					.iter()
					.map(|arg| self.eval_expr(arg).unwrap_or_else(|| blame_span(arg.span(), "Invalid argument in function call")))
					.collect::<Vec<_>>();
				self.eval_function(&name, func_args).unwrap_or_else(|err| blame_span(cmd_expr.span(), err));
			}
			ExprKind::VarDec { name, value } => {
				let value = self.eval_expr(value).unwrap_or_else(|| blame_span(value.span(), "Invalid value for variable declaration"));
				self.set_var(name.clone(), value.clone()).unwrap_or_else(|err| blame_span(cmd_expr.span(), "Failed to set variable"));
			}
			ExprKind::VarMut { name, op, value } => {
				let value = self.eval_expr(value).unwrap_or_else(|| blame_span(value.span(), "Invalid value for variable mutation"));
				if let Some(index) = index {
					todo!()
				} else {
					self.mutate_var(&name, op.clone(), value.clone()).unwrap_or_else(|err| blame_span(cmd_expr.span(), err));
				}
			}
			ExprKind::IfBlock { cond_blocks, else_block } => {
				let mut executed = false;
				for block in cond_blocks {
					let Expr { value, .. } = block;
					let ExprKind::CondBlock { cond, body } = value else { unreachable!() };
					let cond_value = self.eval_expr(cond).unwrap_or_else(|| blame_span(block.span(), "Failed to evaluate 'if' condition"));
					let result = cond_value.is_truthy(self);
					if result {
						executed = true;
						self.descend(); // new scope
						for cmd in body {
							self.eval_expr(cmd).unwrap_or_else(|| blame_span(cmd.span(), "Failed to execute 'if' statement command"));
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
						break;
					}
				}

				if let Some(else_block) = else_block {
					if !executed {
						self.descend(); // new scope
						for cmd in else_block {
							self.eval_expr(cmd).unwrap_or_else(|| blame_span(cmd.span(), "Failed to execute 'else' statement command"));
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
					}
				}
			}
			ExprKind::ForBlock { var_name, list, body } => {
				let val = self.eval_expr(list).unwrap_or_else(|| blame_span(list.span(), "Failed to evaluate list in 'for' block"));
				let val_iter = val.try_into_iter().unwrap_or_else(|err| blame_span(cmd_expr.span(), err));

				'main: for item in val_iter {
					self.descend(); // new scope
					self.set_var(var_name.clone(), item).unwrap_or_else(complain_and_exit);
					for cmd in body {

						if cmd.is_break() {
							break 'main;
						}
						if cmd.is_continue() {
							continue 'main;
						}
						self.eval_expr(cmd).unwrap_or_else(|| blame_span(cmd.span(), "Failed to execute 'for' statement command"));
						if !self.find_opt_or_default(|o| o.keep_mode) {
							self.set_normal_mode();
						}
					}
					self.ascend(); // leave scope
				}
			}
			ExprKind::UntilBlock { cond, body } |
				ExprKind::WhileBlock { cond, body } => {
					// This is the function we will use to see if we are still running
					let running = |vicut: &mut ViCut| {
						let result = vicut.eval_expr(cond).unwrap_or_else(|| blame_span(cond.span(), "Failed to evaluate loop block condition")).is_truthy(vicut); 
						if matches!(cmd, ExprKind::WhileBlock { .. }) {
							result
						} else {
							!result
						}
					};

					while running(self) {
						self.descend(); // new scope
						for cmd in body {
							if cmd.is_break() {
								break;
							}
							if cmd.is_continue() {
								continue;
							}
							self.eval_expr(cmd);
							if !self.find_opt_or_default(|o| o.keep_mode) {
								self.set_normal_mode();
							}
						}
						self.ascend(); // leave scope
					}
				}
			ExprKind::Vic(exprs) => todo!(),
			ExprKind::TopLevel(expr) => todo!(),
			ExprKind::Block(exprs) => todo!(),
			ExprKind::Value(val) => todo!(),
			ExprKind::Opts(exprs) => todo!(),
			ExprKind::Opt { name, arg } => todo!(),
			ExprKind::CondBlock { cond, body } => todo!(),
			ExprKind::Range { start, end } => todo!(),
			ExprKind::BinaryExpr { left, op, right } => todo!(),
			ExprKind::BoolExpr { left, op, right } => todo!(),
		};
		if let Some(index) = index {

		} else {
		}
		None
	}
	fn execute(&mut self, filename: Option<PathBuf>) -> Result<Vec<Vec<(String,String)>>,String> {
		let basename = filename.clone()
			.map(|s| s.file_name().unwrap_or_default().to_string_lossy().to_string())
			.unwrap_or_else(|| String::from("stdin"));
		let filepath = filename.map(|s| s.to_string_lossy().to_string()).unwrap_or(String::from("stdin"));
		self.set_var("filename".into(), Val::Str(basename))?;
		self.set_var("filepath".into(), Val::Str(filepath))?;


		let cmds = self.cmds.clone(); // FIXME: This might cause some weird desync issues if we do something like allowing scoped 'include' calls later
		for cmd in cmds {
			self.eval_expr(&cmd);
			if !self.find_opt(|o| o.keep_mode).unwrap_or_default() {
				self.set_normal_mode();
			}
		}

		let opts = self.flatten_opts();

		if !self.exec_ctx.fields.is_empty() {
			self.exec_ctx.fmt_lines.push(std::mem::take(&mut self.exec_ctx.fields));
		}

		if self.exec_ctx.fmt_lines.is_empty() && self.find_opt(|o| o.silent).unwrap_or_default() {
			return Ok(vec![]);
		}

		// Let's figure out if we want to print the whole buffer
		let no_fields = self.exec_ctx.fmt_lines.is_empty(); // No fields were extracted
		let has_files = !opts.files.is_some_and(|f| f.is_empty()); // We have files to edit
		let editing_inplace = self.find_opt(|o| o.edit_inplace).unwrap_or_default(); // We are not editing in place

		// If we have not extracted any fields, and the following conditions are true:
		// * We have files without editing in place, or
		// * We don't have any files, order
		// * We have a pattern search with at least one field extraction
		//
		// then we print the entire buffer
		let should_print_entire_buffer = (!editing_inplace || !has_files) && no_fields;

		if should_print_entire_buffer {
			let big_line = self.current_buffer().buffer.clone();
			self.exec_ctx.fmt_lines.push(vec![("0".into(),big_line)]);
		}

		if opts.trim_fields.unwrap_or_default() {
			self.trim_fields();
		}

		Ok(self.exec_ctx.fmt_lines.clone())
	}
	/// Trim the fields 🧑‍🌾
	fn trim_fields(&mut self) {
		for line in self.exec_ctx.fmt_lines.iter_mut() {
			for (_, field) in line {
				*field = field.trim().to_string()
			}
		}
	}
}
