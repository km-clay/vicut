//! This module contains the `ViCut` struct, which is the central container for state in the program.
//!
//! Everything that moves through this program passes through the `ViCut` struct at some point.
use std::collections::HashMap;
use std::fmt::Display;

use log::trace;
use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;

use crate::keys::{KeyCode, KeyEvent, ModKeys};
use crate::linebuf::{ordered, ordered_signed, ClampedUsize, MotionKind};
use crate::modes::ex::ViEx;
use crate::modes::search::ViSearch;
use crate::reader::{KeyReader, RawReader};
use crate::register::read_register;
use crate::vic::{BinOp, BoolOp, CmdArg, Expr};
use crate::vicmd::{Bound, LineAddr, Word};
use crate::{complain_and_exit, Cmd, ExecCtx};

use super::linebuf::{LineBuf, SelectAnchor, SelectMode};
use super::vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd};
use super::modes::{CmdReplay, ModeReport, insert::ViInsert, ViMode, normal::ViNormal, replace::ViReplace, visual::ViVisual};

#[derive(Default, Debug, Clone)]
pub enum Val {
	#[default]
	Null,
	Str(String),
	Arr(Vec<Val>),
	Num(isize),
	Bool(bool),
	Regex(Regex),
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

impl From<CompoundVal> for Val {
	fn from(value: CompoundVal) -> Self {
		match value {
			CompoundVal::Str(s) => Val::Str(s),
			CompoundVal::Arr(arr) => Val::Arr(arr),
		}
	}
}

pub enum CompoundVal {
	Str(String),
	Arr(Vec<Val>),
}

impl CompoundVal {
	pub fn len(&self) -> usize {
		match self {
			CompoundVal::Str(s) => s.len(),
			CompoundVal::Arr(arr) => arr.len(),
		}
	}
	pub fn is_empty(&self) -> bool {
		match self {
			CompoundVal::Str(s) => s.is_empty(),
			CompoundVal::Arr(arr) => arr.is_empty(),
		}
	}
	pub fn set(&mut self, index: usize, value: Val) {
		let len = self.len();
		match self {
			CompoundVal::Str(s) => {
				if let Some((i,_)) = s.grapheme_indices(true).nth(index) {
					s.replace_range(i..=i, &value.to_string());
				} else {
					let padding = index.saturating_sub(len);
					if padding > 0 {
						s.push_str(&" ".repeat(padding));
					}
					s.push_str(&value.to_string());
				}
			}
			CompoundVal::Arr(arr) => {
				if index < arr.len() {
					arr[index] = value;
				}
			}
		}
	}
}

impl IntoIterator for CompoundVal {
	type Item = Val;
	type IntoIter = std::vec::IntoIter<Val>;

	fn into_iter(self) -> Self::IntoIter {
		match self {

			CompoundVal::Str(s) => {
				// There has got to be a better way to do this
				// but I will figure it out later
				s.graphemes(true).map(|s| Val::Str(s.to_string())).collect::<Vec<_>>().into_iter()
			}
			CompoundVal::Arr(arr) => arr.into_iter(),
		}
	}
}

impl TryFrom<Val> for CompoundVal {
	type Error = String;

	fn try_from(value: Val) -> Result<Self, Self::Error> {
		match value {
			Val::Str(s) => Ok(CompoundVal::Str(s)),
			Val::Arr(arr) => Ok(CompoundVal::Arr(arr)),
			_ => Err(format!("Expected a compound value, got {}", value.display_type()))
		}
	}
}

impl Val {
	pub fn display_type(&self) -> String {
		match self {
			Self::Str(_) => "string".to_string(),
			Self::Num(_) => "number".to_string(),
			Self::Arr(_) => "array".to_string(),
			Self::Bool(_) => "boolean".to_string(),
			Self::Regex(_) => "regex".to_string(),
			Self::Null => "null".to_string()
		}
	}
	pub fn is_truthy(&self) -> bool {
		match self {
			Self::Str(s) => !s.is_empty(),
			Self::Num(n) => *n != 0,
			Self::Arr(arr) => !arr.is_empty(),
			Self::Bool(b) => *b,
			Self::Null => false,

			// Weird case. The regex compiled successfully, so we consider it truthy
			Self::Regex(_) => true,
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
			Self::Str(s) => write!(f, "{s}"),
			Self::Num(n) => write!(f, "{n}"),
			Self::Bool(b) => write!(f, "{b}"),
			Self::Regex(r) => write!(f, "{r}"),
			Self::Null => write!(f, "null")
		}
	}
}

#[derive(Debug, Clone)]
pub struct VicFunc {
	pub args: Vec<String>,
	pub body: Vec<Cmd>,
}

pub struct ViCut {
	pub reader: RawReader,
	pub mode: Box<dyn ViMode>,
	pub repeat_action: Option<CmdReplay>,
	pub repeat_motion: Option<MotionCmd>,
	pub editor: LineBuf,

	/// We use a vector of hashmaps here
	/// Each hashmap represents a "stack frame" of variables
	/// So you can shadow variables in vic
	/// The outer-most hashmap always contains the built-in variables
	pub variables: Vec<HashMap<String, Val>>,
	/// We do the same stack frame thing for functions
	/// Though might not be as necessary,
	/// we do want all of our user definitions, variable or otherwise, to be scoped
	pub functions: Vec<HashMap<String, VicFunc>>,
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
			editor: LineBuf::new().with_initial(input, cursor),

			// Initialize the stack frames
			// The first is the "built-in" scope, which includes built-in variables
			// and standard library functions from "prelude.vic"
			// The second is the "global" scope, which is where user-defined variables and functions go
			// User definitions can shadow built-ins this way.
			// Never allow these vectors to dip below length 2.
			variables: vec![HashMap::new(),HashMap::new()],
			functions: vec![HashMap::new(),HashMap::new()],
		})
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

	pub fn read_field(&mut self, cmd: &str) -> Result<String,String> {
		self.load_input(cmd);
		let mut start = self.editor.cursor.get();
		let mut end;

		self.exec_loop()?;

		let new_pos_clamped = self.editor.cursor;
		let new_pos = new_pos_clamped.get();
		end = new_pos;
		(start,end) = ordered(start, end);
		end += 1;



		if self.editor.select_range().is_some() {
			// We are in visual mode if we've made it here
			// So we are going to use the editor's selected content
			Ok(self.editor.selected_content().unwrap())
		} else {
			if self.editor.buffer.is_empty() {
				return Ok(String::new())
			}
			let start = ClampedUsize::new(start, self.editor.cursor.cap(), true);
			let end = ClampedUsize::new(end, self.editor.cursor.cap(), false);
			let start_pos = start.get();
			let end_pos = end.get();
			let slice = self.editor
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
		self.editor.stop_selecting();
		if should_go_back_one {
			let new_pos = self.editor.cursor.ret_sub(1);
			// Leaving insert mode moves back one, but never crosses line boundaries
			if self.editor.grapheme_at(new_pos).is_some_and(|gr| gr != "\n") {
				self.editor.cursor.sub(1);
			}
			if self.editor.should_handle_block_insert() {
				self.editor.handle_block_insert();
			}
		}
	}

	fn handle_mode_transition(&mut self, cmd: ViCmd) -> Result<(),String> {
		let mut select_mode = None;
		let mut is_insert_mode = false;
		let count = cmd.verb_count();
		if self.mode.report_mode() == ModeReport::Insert && self.editor.should_handle_block_insert() {
			self.editor.handle_block_insert();
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
					self.editor.start_selecting(SelectMode::Char(SelectAnchor::Start));
				}
				self.editor.inserting_from_visual = false;
				let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
				std::mem::swap(&mut mode, &mut self.mode);
				self.editor.set_cursor_clamp(self.mode.clamp_cursor());

				return self.editor.exec_cmd(cmd)
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
				select_mode = Some(self.editor.get_block_select());
				Box::new(ViVisual::new())
			}

			// For these two we will return early instead of doing all the other stuff.
			// This is to preserve the line buffer's state while we are entering a pattern in search mode
			// If we continue from here, visual mode selections will be lost for instance.
			Verb::ExMode => {
				let mut mode: Box<dyn ViMode> = Box::new(ViEx::new(self.editor.selected_lines()));
				self.editor.inserting_from_visual = false;
				std::mem::swap(&mut mode, &mut self.mode);

				return Ok(())
			}
			Verb::SearchMode(count,dir) => {
				let mut mode: Box<dyn ViMode> = Box::new(ViSearch::new(count,dir));
				self.editor.inserting_from_visual = false;
				std::mem::swap(&mut mode, &mut self.mode);

				return Ok(())
			}

			_ => unreachable!()
		};

		self.editor.inserting_from_visual = inserting_from_visual;

		std::mem::swap(&mut mode, &mut self.mode);

		if mode.is_repeatable() {
			self.repeat_action = mode.as_replay();
		}

		self.editor.set_cursor_clamp(self.mode.clamp_cursor());
		self.editor.exec_cmd(cmd)?;

		if let Some(select_mode) = select_mode {
			self.editor.start_selecting(select_mode);
		} else {
			self.editor.stop_selecting();
		}
		if is_insert_mode {
			self.editor.mark_insert_mode_start_pos();
		} else {
			self.editor.clear_insert_mode_start_pos();
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
						self.editor.exec_cmd(cmd)?
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
				self.editor.exec_cmd(cmd)?;
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
				self.editor.exec_cmd(repeat_cmd)
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
				self.editor.exec_cmd(repeat_cmd)
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
				let range = self.editor.select_range().unwrap().clone();
				let motion = match self.editor.select_mode.as_ref().unwrap() {
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

		self.editor.set_cursor_clamp(self.mode.clamp_cursor());
		self.editor.exec_cmd(cmd.clone())?;

		if self.mode.report_mode() == ModeReport::Visual && cmd.verb().is_some_and(|v| v.1.is_edit()) {
			self.editor.stop_selecting();
			let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
			std::mem::swap(&mut mode, &mut self.mode);
		}
		Ok(())
	}

	// Easier to handle these out here
	fn exec_ex_global(&mut self, cmd: ViCmd) -> Result<(),String> {
		let ViCmd { register, verb, motion, raw_seq, flags } = cmd;
		let MotionKind::Lines(lines) = self.editor.eval_motion(verb.as_ref().map(|vcmd| &vcmd.1), motion.unwrap()) else { unreachable!() };
		for line in lines {
			let Some((start,_)) = self.editor.line_bounds(line) else { break };
			self.editor.cursor.set(start);
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
				let line_no = self.editor.eval_line_addr(addr)
					.ok_or("Failed to evaluate line address".to_string())?;
				let (start,_) = self.editor.line_bounds(line_no)
					.ok_or(format!("Failed to get line bounds for line {line_no}"))?;
				self.editor.cursor.set(start);
				self.reader.push_bytes_front(seq.as_bytes());

				self.exec_loop()?;
			}
			Motion::LineRange(start, end) => {
				let start_ln = self.editor.eval_line_addr(start)
					.ok_or("Failed to evaluate line address".to_string())?;
				let end_ln = self.editor.eval_line_addr(end)
					.ok_or("Failed to evaluate line address".to_string())?;
				let (start_ln,end_ln) = ordered(start_ln, end_ln);

				for line in start_ln..=end_ln {
					let mut mode: Box<dyn ViMode> = Box::new(ViNormal::new());
					std::mem::swap(&mut self.mode, &mut mode);

					let (start,_) = self.editor.line_bounds(line)
						.ok_or("Failed to evaluate line address".to_string())?;
					self.editor.cursor.set(start);
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
	}
	pub fn ascend(&mut self) {
		// Never pop the built-in/global scopes
		if self.variables.len() > 2 {
			self.variables.pop();
		}
		if self.functions.len() > 2 {
			self.functions.pop();
		}
	}
	pub fn get_var_mut(&mut self, name: &str) -> Option<&mut Val> {
		for frame in self.variables.iter_mut().rev() {
			if frame.contains_key(name) {
				return frame.get_mut(name)
			}
		}
		None
	}
	pub fn get_var(&mut self, name: &str) -> Option<Val> {
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
			"col" => Val::Num((self.editor.cursor_col() + 1) as isize),
			"line" => Val::Num((self.editor.cursor_line_number() + 1) as isize),
			"lines" => Val::Num(self.editor.total_lines() as isize),
			"pos" => Val::Num(self.editor.cursor_byte_pos() as isize),
			"buf_len" => Val::Num(self.editor.buffer.len() as isize),
			"selection" => Val::Str(self.editor.selected_content().unwrap_or_default()),
			"word" => {
				let (word_start,word_end) = self.editor.text_obj_word(1, Bound::Inside, Word::Normal).unwrap_or_default();
				let word_end = ClampedUsize::new(word_end, self.editor.cursor.cap(), false).ret_add(1);
				self.editor.slice_inclusive(word_start..=word_end)
					.map(|slice| Val::Str(slice.to_string()))
					.unwrap_or(Val::Str(String::new()))
			}
			"WORD" => {
				let (big_word_start,big_word_end) = self.editor.text_obj_word(1, Bound::Inside, Word::Big).unwrap_or_default();
				let big_word_end = ClampedUsize::new(big_word_end, self.editor.cursor.cap(), false).ret_add(1);
				self.editor.slice_inclusive(big_word_start..=big_word_end)
					.map(|slice| Val::Str(slice.to_string()))
					.unwrap_or(Val::Str(String::new()))
			}
			"is_eof" => Val::Bool(self.editor.cursor_at_max()),
			"is_eol" => Val::Bool(self.editor.cursor_at_eol()),
			"is_bof" => Val::Bool(self.editor.cursor.get() == 0),
			"char" => {
				self.editor.grapheme_at_cursor()
					.map(|gr| Val::Str(gr.to_string()))
					.unwrap_or(Val::Str(String::new()))
			}
			_ => unreachable!()
		})
	}
	pub fn set_var(&mut self, name: String, value: Val) {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.insert(name, value);
	}
	pub fn clear_var(&mut self, name: &str) {
		let Some(frame) = self.variables.last_mut() else {
			panic!("There is supposed to be a stack frame here")
		};
		frame.remove(name);
	}
	pub fn set_function(&mut self, name: String, args: Vec<String>, body: Vec<Cmd>) {
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
	pub fn eval_count(&mut self, count: &CmdArg) -> Result<usize,String> {
		match count {
			CmdArg::Count(n) => Ok(*n),
			CmdArg::Var(var) => {
				let Some(val) = self.get_var(var) else {
					eprintln!("vicut: variable '{var}' not found");
					std::process::exit(1)
				};
				let Val::Num(n) = val else {
					eprintln!("vicut: variable '{var}' is not a number");
					std::process::exit(1)
				};
				Ok(n as usize)
			}
			_ => Err(format!("Expected count, got {}",count.display_type()))
		}
	}
	pub fn eval_cmd_arg(&mut self, arg: &CmdArg,ctx: &mut ExecCtx) -> Result<Val,String> {
		match arg {
			CmdArg::Literal(value) => {
				let expanded = self.expand_literal(&value.to_string())?;
				Ok(Val::Str(expanded))
			}
			CmdArg::Expr(expr) => self.eval_expr(expr,ctx),
			CmdArg::Var(var) => {
				let Some(val) = self.get_var(var) else {
					return Ok(Val::Null)
				};
				Ok(val.clone())
			}
			_ => unreachable!()
		}
	}
	pub fn eval_expr(&mut self, expr: &Expr, ctx: &mut ExecCtx) -> Result<Val,String> {
		Ok(match expr {
			Expr::Pop(stack_name) => {
				let Some(var) = self.get_var_mut(stack_name) else {
					return Err(format!("Stack variable {stack_name} not found"))
				};
				let iterable = CompoundVal::try_from(var.clone())
					.map_err(|e| format!("Failed to convert variable {stack_name} to iterable: {e}"))?;

				let (ret,new_var) = match iterable {
					CompoundVal::Str(str) => {
						let mut graphemes = str.graphemes(true).collect::<Vec<_>>();
						if graphemes.is_empty() {
							return Ok(Val::Null)
						}
						let last = graphemes.pop().unwrap();
						let new_str = graphemes.join("");
						(Val::Str(last.into()),Val::Str(new_str))
					}
					CompoundVal::Arr(mut vals) => {
						if vals.is_empty() {
							return Ok(Val::Null)
						}
						let last = vals.pop().unwrap();
						(last,Val::Arr(vals))
					}
				};
				*var = new_var;
				ret
			}
			Expr::Null => Val::Null,
			Expr::Regex(raw) => {
				// 'raw' is a String, we compile the regex during evaluation here.
				let regex = Regex::new(raw)
					.map_err(|e| format!("Invalid regex: {e}"))?;
				Val::Regex(regex)
			}
			Expr::RangeInclusive(start, end) |
			Expr::Range(start, end) => {
				let is_inclusive = matches!(expr, Expr::RangeInclusive(_, _));
				let start = self.eval_expr(start,ctx)?;
				let end = self.eval_expr(end,ctx)?;
				let Val::Num(start) = start else {
					return Err(format!("Expected number in range, got {}",start.display_type()))
				};
				let Val::Num(end) = end else {
					return Err(format!("Expected number in range, got {}",end.display_type()))
				};
				let rev = start > end;
				let range = if rev {
					if is_inclusive {
						(start..=end).rev().map(Val::Num).collect()
					} else {
						(start..end).rev().map(Val::Num).collect()
					}
				} else if is_inclusive {
					(start..=end).map(Val::Num).collect()
				} else {
					(start..end).map(Val::Num).collect()
				};
				Val::Arr(range)
			}
			Expr::Register(name) => {
				let Some(content) = read_register(Some(*name)) else {
					return Ok(Val::Null)
				};
				let content = content.to_string();
				Val::Str(content)
			}
			Expr::VarIndex(name, expr) => {
				let val = self.eval_expr(expr,ctx)?;
				let Val::Num(idx) = val else {
					return Err(format!("Attempt to index variable {name} with non-number {val}"))
				};
				let idx = idx as usize;
				self.read_index_var(name.to_string(), idx)?
			}
			Expr::Var(var) => {
				let Some(var) = self.get_var(var) else {
					return Err(format!("Variable {var} not found"))
				};
				var.clone()
			}
			Expr::Array(arr) => {
				let mut val_arr = Vec::new();
				for item in arr {
					let val = self.eval_expr(item,ctx)?;
					val_arr.push(val);
				}
				Val::Arr(val_arr)
			}
			Expr::Literal(lit) => {
				let expanded = self.expand_literal(lit)?;
				Val::Str(expanded)
			}
			Expr::Int(int) => Val::Num(*int as isize),
			Expr::TernaryExp { cond, true_case, false_case } => self.eval_ternary_expr(cond, true_case, false_case,ctx)?,
			Expr::BinExp { op, left, right } => self.eval_bin_expr(op, left, right)?,
			Expr::BoolExp { op, left, right } => self.eval_bool_expr(op, left, right.as_ref(),ctx)?,
			Expr::Bool(bool) => Val::Bool(*bool),
			Expr::Return(cmd) => {
				let Ok(field) = self.read_field(cmd) else {
					return Err("Failed to read field".to_string())
				};
				Val::Str(field)
			}
			Expr::FuncCall(name,args) => {
				let args = args.iter()
					.map(|arg| self.eval_expr(arg,ctx))
					.collect::<Result<Vec<_>,_>>()?;
				self.eval_function(name.clone(), args, ctx)?
			}
		})
	}
	pub fn eval_function(&mut self, name: String, args: Vec<Val>, ctx: &mut ExecCtx) -> Result<Val,String> {
		let Some(func) = self.get_function(&name) else {
			return Err(format!("Function {name} not found"))
		};
		let func = func.clone();
		if func.args.len() != args.len() {
			return Err(format!("Function {name} expects {} arguments, got {}", func.args.len(), args.len()))
		}
		self.descend();
		for (arg_name, arg_value) in func.args.iter().zip(args.into_iter()) {
			self.set_var(arg_name.clone(), arg_value);
		}
		let mut ret_val = None;
		for cmd in &func.body {
			// Here we can use our existing mutable reference to self
			// Along with the function cmd and the ctx we have
			// To maintain context even in nested function calls
			ret_val = super::exec_cmd(cmd, self, ctx);
			if ret_val.is_some() { break } // We got a 'return' call, so we break now
		}
		self.ascend();
		Ok(ret_val.unwrap_or(Val::Null))
	}
	pub fn eval_ternary_expr(&mut self, cond: &(bool,Box<Expr>), true_case: &Expr, false_case: &Expr, ctx: &mut ExecCtx) -> Result<Val,String> {
		let cond = self.eval_expr(&cond.1,ctx)?;
		if cond.is_truthy() {
			self.eval_expr(true_case,ctx)
		} else {
			self.eval_expr(false_case,ctx)
		}
	}
	pub fn eval_bool_expr(&mut self, op: &BoolOp, left: &(bool,Box<Expr>), right: Option<&(bool,Box<Expr>)>, ctx: &mut ExecCtx) -> Result<Val,String> {
		let left_negated = left.0;
		let left = self.eval_expr(&left.1,ctx)?;

		let Some(right) = right else {
			// This is a unary boolean expression
			// We only support negation here
			let right = if left_negated {
				!left.is_truthy()
			} else {
				left.is_truthy()
			};
			return Ok(Val::Bool(right))
		};

		let right_negated = right.0;
		let right = self.eval_expr(&right.1,ctx)?;

		// Check for regex comparisons
		if matches!(left, Val::Regex(_)) || matches!(right, Val::Regex(_)) {
			// Check for weird operators
			if !matches!(op, BoolOp::Eq | BoolOp::Ne) {
				return Err(format!("Cannot compare regex with {op}"))
			}

			// The PartialEq implementation for Val handles regex matching automatically
			// So we can just use Val::eq() here
			match op {
				BoolOp::Eq => {
					return Ok(Val::Bool(left.eq(&right)));
				}
				BoolOp::Ne => {
					return Ok(Val::Bool(!left.eq(&right)));
				}
				_ => unreachable!()
			}
		}

		match left {
			Val::Null => {
				if matches!(op, BoolOp::Eq | BoolOp::Ne) {
					if matches!(right, Val::Null) {
						Ok(Val::Bool(op == &BoolOp::Eq))
					} else {
						Ok(Val::Bool(false)) // Null is not equal to anything but null
					}
				} else {
					Err(format!("Cannot compare null with {op}"))
				}
			}
			Val::Arr(l_arr) => {
				let Val::Arr(r_arr) = right else {
					return Err(format!("Expected array, got {}",right.display_type()))
				};
				match op {
					BoolOp::Eq => Ok(Val::Bool(l_arr == r_arr)),
					BoolOp::Ne => Ok(Val::Bool(l_arr != r_arr)),
					BoolOp::Lt => Err("Cannot compare arrays with <".into()),
					BoolOp::LtEq => Err("Cannot compare arrays with <=".into()),
					BoolOp::Gt => Err("Cannot compare arrays with >".into()),
					BoolOp::GtEq => Err("Cannot compare arrays with >=".into()),
					_ => todo!()
				}
			}
			Val::Str(l_string) => {
				let Val::Str(r_string) = right else {
					return Err(format!("Expected string, got {}",right.display_type()))
				};
				if r_string == "\\\"" {
					panic!("we didn't unescape the string properly, got {r_string}");
				}
				match op {
					BoolOp::Eq => Ok(Val::Bool(l_string == r_string)),
					BoolOp::Ne => Ok(Val::Bool(l_string != r_string)),
					BoolOp::Lt => Err("Cannot compare strings with <".into()),
					BoolOp::LtEq => Err("Cannot compare strings with <=".into()),
					BoolOp::Gt => Err("Cannot compare strings with >".into()),
					BoolOp::GtEq => Err("Cannot compare strings with >=".into()),
					_ => todo!()
				}
			}
			Val::Num(l_num) => {
				let Val::Num(r_num) = right else {
					return Err(format!("Expected number, got {}",right.display_type()))
				};
				match op {
					BoolOp::Eq => Ok(Val::Bool(l_num == r_num)),
					BoolOp::Ne => Ok(Val::Bool(l_num != r_num)),
					BoolOp::Lt => Ok(Val::Bool(l_num < r_num)),
					BoolOp::LtEq => Ok(Val::Bool(l_num <= r_num)),
					BoolOp::Gt => Ok(Val::Bool(l_num > r_num)),
					BoolOp::GtEq => Ok(Val::Bool(l_num >= r_num)),
					_ => todo!()
				}
			}
			Val::Bool(l_bool) => {
				let l_bool = if left_negated {
					!l_bool
				} else {
					l_bool
				};
				let Val::Bool(r_bool) = right else {
					return Err(format!("Expected boolean, got {}",right.display_type()))
				};
				let r_bool = if right_negated {
					!r_bool
				} else {
					r_bool
				};
				match op {
					BoolOp::And => Ok(Val::Bool(l_bool && r_bool)),
					BoolOp::Or => Ok(Val::Bool(l_bool || r_bool)),
					// The structure of vic's grammar should mean that we only get And/Or here
					// (probably)
					_ => unreachable!()
				}
			}
			Val::Regex(_) => {
				// Already handled above
				unreachable!()
			}
		}
	}
	pub fn eval_bin_expr(&mut self, op: &BinOp, left: &Expr, right: &Expr) -> Result<Val,String> {
		let left = match left {
			Expr::Var(var) => {
				let Some(var) = self.get_var(var) else {
					return Err(format!("Variable {var} not found"))
				};
				let Val::Num(_) = &var else {
					return Err(format!("Variable {var} is not a number"))
				};
				var.clone()
			}
			Expr::Int(int) => Val::Num(*int as isize),
			Expr::BinExp { op, left, right } => self.eval_bin_expr(op, left, right)?,
			_ => unreachable!(),
		};
		let right = match right {
			Expr::Var(var) => {
				let Some(var) = self.get_var(var) else {
					return Err(format!("Variable {var} not found"))
				};
				let Val::Num(_) = &var else {
					return Err(format!("Variable {var} is not a number"))
				};
				var.clone()
			}
			Expr::Int(int) => Val::Num(*int as isize),
			Expr::BinExp { op, left, right } => self.eval_bin_expr(op, left, right)?,
			_ => unreachable!(),
		};
		let Val::Num(left) = left else {
			return Err(format!("Left value {left} is not a number"))
		};
		let Val::Num(right) = right else {
			return Err(format!("Right value {right} is not a number"))
		};
		Ok(Val::Num(match op {
			BinOp::Add => left + right,
			BinOp::Sub => left - right,
			BinOp::Mult => left * right,
			BinOp::Div => left / right,
			BinOp::Mod => left % right,
			BinOp::Pow => left.pow(right as u32),
			BinOp::Equals => unreachable!() // Not used in this context
		}))
	}
	pub fn mutate_var(&mut self, name: String, op: BinOp, value: Val) -> Result<(),String> {
		let Some(var) = self.get_var_mut(&name) else {
			return Err(format!("Variable {name} not found"))
		};
		match var {
			Val::Bool(b) => {
				let Val::Bool(value) = value else {
					return Err(format!("Value {value} is not a boolean"))
				};
				match op {
					BinOp::Equals => {
						*b = value;
					}
					BinOp::Add | BinOp::Sub | BinOp::Mult | BinOp::Div | BinOp::Mod | BinOp::Pow => {
						return Err(format!("Cannot perform {op} on boolean variable {name}"))
					}
				}
			}
			Val::Num(var) => {
				let Val::Num(value) = value else {
					return Err(format!("Value {value} is not a number"))
				};
				match op {
					BinOp::Equals => {
						*var = value;
					}
					BinOp::Add => {
						*var += value;
					}
					BinOp::Sub => {
						*var -= value;
					}
					BinOp::Mult => {
						*var *= value;
					}
					BinOp::Div => {
						if value == 0 {
							return Err("Division by zero".to_string())
						}
						*var /= value;
					}
					BinOp::Mod => {
						*var %= value;
					}
					BinOp::Pow => {
						*var = var.pow(value as u32);
					}
				}
			}
			_ => unimplemented!()
		}
		Ok(())
	}
	pub fn set_index_var(&mut self, name: String, index: usize, value: Val) -> Result<(),String> {
		let Some(var) = self.get_var_mut(&name) else {
			return Err(format!("Variable {name} not found"))
		};
		let Some(mut compound) = CompoundVal::try_from(var.clone()).ok() else {
			return Err(format!("Variable {name} is not indexable"))
		};
		let len = compound.len();
		if index >= len {
			return Err(format!("Index {index} out of bounds for array {name}, length is {len}",))
		}
		compound.set(index, value);
		self.set_var(name, compound.into());
		Ok(())
	}
	pub fn read_index_var(&mut self, name: String, index: usize) -> Result<Val,String> {
		let Some(var) = self.get_var_mut(&name) else {
			return Err(format!("Variable {name} not found"))
		};
		let Some(compound) = CompoundVal::try_from(var.clone()).ok() else {
			return Err(format!("Variable {name} is not indexable"))
		};
		let len = compound.len();
		let item = compound.into_iter().nth(index).ok_or(format!("Index {index} out of bounds for array {name}, length is {len}",))?;
		Ok(item)
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
}
