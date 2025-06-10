use log::{debug, trace};

use crate::linebuf::{ordered, ClampedUsize};
use crate::reader::{KeyReader, RawReader};

use super::keys::{KeyCode, KeyEvent, ModKeys};
use super::linebuf::{LineBuf, SelectAnchor, SelectMode};
use super::vicmd::{CmdFlags, Motion, MotionCmd, RegisterName, To, Verb, VerbCmd, ViCmd};
use super::vimode::{CmdReplay, ModeReport, ViInsert, ViMode, ViNormal, ViReplace, ViVisual};

pub struct ViCut {
	pub reader: RawReader,
	pub mode: Box<dyn ViMode>,
	pub repeat_action: Option<CmdReplay>,
	pub repeat_motion: Option<MotionCmd>,
	pub editor: LineBuf,
}


impl ViCut {
	pub fn new(input: &str, cursor: usize) -> Result<Self,String> {
		Ok(Self {
			reader: RawReader::new(),
			mode: Box::new(ViNormal::new()),
			repeat_action: None,
			repeat_motion: None,
			editor: LineBuf::new().with_initial(input, cursor),
		})
	}

	pub fn read_field(&mut self, cmd: &str) -> Result<String,String> {
		self.load_input(cmd);
		self.mode = Box::new(ViNormal::new());
		let (mut start,mut end) = (self.editor.cursor.get(), self.editor.cursor.get());
		if self.mode.clamp_cursor() {
			end = self.editor.cursor.ret_add(1);
		}

		loop {
			let Some(key) = self.reader.read_key() else {
				break
			};

			let Some(mut cmd) = self.mode.handle_key(key) else {
				continue
			};
			cmd.alter_line_motion_if_no_verb();


			self.exec_cmd(cmd)?;
			let new_pos_clamped = self.editor.cursor;
			let new_pos = new_pos_clamped.get();
			if new_pos < start {
				start = new_pos;
			} else {
				end = new_pos;
				if self.mode.clamp_cursor() {
					end += 1;
				}
			}
		}

		let (start,end) = ordered(start, end);

		let slice = if let Some((start,mut end)) = self.editor.select_range() {
			// We are in visual mode if we've made it here
			// So we are going to use the editor's selected range
			if self.editor.select_mode == Some(SelectMode::Char(SelectAnchor::End)) {
				end += 1; // Include the cursor's character
			}
			let slice = self.editor
				.slice(start..end)
				.map(|slice| slice.to_string())
				.ok_or("Failed to slice buffer".to_string());
			if let Ok(slice) = slice.as_ref() {
				trace!("Cutting with visual mode range, got: '{slice}'");
			} else {
				trace!("Failed to slice buffer from visual mode range");
			}
			slice
		} else {
			let start = ClampedUsize::new(start, self.editor.cursor.cap(), true);
			let end = ClampedUsize::new(end, self.editor.cursor.cap(), false);
			let slice = self.editor
				.slice(start.get()..end.get())
				.map(|slice| slice.to_string())
				.ok_or("Failed to slice buffer".to_string());
			if let Ok(slice) = slice.as_ref() {
				trace!("Cutting from start position to cursor: '{slice}'");
			} else {
				trace!("Failed to slice buffer from cursor motion");
			}
			slice
		};
		slice
	}

	pub fn move_cursor(&mut self, cmd: &str) -> Result<(),String> {
		self.read_field(cmd).map(|_| ()) // Same logic, just ignore the returned range
	}

	pub fn load_input(&mut self, input: &str) {
		let bytes = input.as_bytes();
		self.reader.load_bytes(bytes);
	}

	pub fn set_normal_mode(&mut self) {
		self.mode = Box::new(ViNormal::new());
		self.editor.stop_selecting();
	}

	pub fn exec_cmd(&mut self, mut cmd: ViCmd) -> Result<(),String> {
		let mut selecting = false;
		let mut is_insert_mode = false;
		if cmd.is_mode_transition() {
			let count = cmd.verb_count();
			let mut mode: Box<dyn ViMode> = match cmd.verb().unwrap().1 {
				Verb::Change |
				Verb::InsertModeLineBreak(_) |
				Verb::InsertMode => {
					is_insert_mode = true;
					Box::new(ViInsert::new().with_count(count as u16))
				}

				Verb::NormalMode => {
					Box::new(ViNormal::new())
				}

				Verb::ReplaceMode => Box::new(ViReplace::new()),

				Verb::VisualModeSelectLast => {
					if self.mode.report_mode() != ModeReport::Visual {
						self.editor.start_selecting(SelectMode::Char(SelectAnchor::End));
					}
					let mut mode: Box<dyn ViMode> = Box::new(ViVisual::new());
					std::mem::swap(&mut mode, &mut self.mode);
					self.editor.set_cursor_clamp(self.mode.clamp_cursor());

					return self.editor.exec_cmd(cmd)
				}
				Verb::VisualMode => {
					selecting = true;
					Box::new(ViVisual::new())
				}

				_ => unreachable!()
			};

			std::mem::swap(&mut mode, &mut self.mode);

			if mode.is_repeatable() {
				self.repeat_action = mode.as_replay();
			}

			self.editor.exec_cmd(cmd)?;
			self.editor.set_cursor_clamp(self.mode.clamp_cursor());

			if selecting {
				self.editor.start_selecting(SelectMode::Char(SelectAnchor::End));
			} else {
				self.editor.stop_selecting();
			}
			if is_insert_mode {
				self.editor.mark_insert_mode_start_pos();
			} else {
				self.editor.clear_insert_mode_start_pos();
			}
			return Ok(())
		} else if cmd.is_cmd_repeat() {
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
			return Ok(())
		} else if cmd.is_motion_repeat() {
			match cmd.motion.as_ref().unwrap() {
				MotionCmd(count,Motion::RepeatMotion) => {
					let Some(motion) = self.repeat_motion.clone() else {
						return Ok(())
					};
					let repeat_cmd = ViCmd {
						register: RegisterName::default(),
						verb: None,
						motion: Some(motion),
						raw_seq: format!("{count};"),
						flags: CmdFlags::empty()
					};
					return self.editor.exec_cmd(repeat_cmd);
				}
				MotionCmd(count,Motion::RepeatMotionRev) => {
					let Some(motion) = self.repeat_motion.clone() else {
						return Ok(())
					};
					let mut new_motion = motion.invert_char_motion();
					new_motion.0 = *count;
					let repeat_cmd = ViCmd {
						register: RegisterName::default(),
						verb: None,
						motion: Some(new_motion),
						raw_seq: format!("{count},"),
						flags: CmdFlags::empty()
					};
					return self.editor.exec_cmd(repeat_cmd);
				}
				_ => unreachable!()
			}
		}

		if cmd.is_repeatable() {
			if self.mode.report_mode() == ModeReport::Visual {
				// The motion is assigned in the line buffer execution, so we also have to assign it here
				// in order to be able to repeat it
				let range = self.editor.select_range().unwrap();
				cmd.motion = Some(MotionCmd(1,Motion::Range(range.0, range.1)))
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
}

