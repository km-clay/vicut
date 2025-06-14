
use crate::{modes::{common_cmds, ModeReport, ViMode}, vicmd::{CmdFlags, Direction, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd}};


pub struct ViSearch {
	pending_pattern: String,
	count: usize,
	next_is_escaped: bool,
	direction: Direction
}

impl ViSearch {
	pub fn new(count: usize, direction: Direction, ) -> Self {
		Self {
			pending_pattern: Default::default(),
			count,
			next_is_escaped: false,
			direction 
		}
	}
}

impl ViMode for ViSearch {
	fn handle_key(&mut self, key: crate::keys::KeyEvent) -> Option<crate::vicmd::ViCmd> {
		use crate::keys::{KeyEvent as E, KeyCode as C, ModKeys as M};
		match key {
			E(C::Char('\r'), M::NONE) |
			E(C::Enter, M::NONE) => {
				let start_cmd = if self.direction == Direction::Forward { "/" } else { "?" };
				let raw_seq = format!("{start_cmd}{}",self.pending_pattern.clone());
				let motion = match self.direction {
					Direction::Forward => Motion::PatternSearch(std::mem::take(&mut self.pending_pattern)),
					Direction::Backward => Motion::PatternSearchRev(std::mem::take(&mut self.pending_pattern)),
				};
				Some(ViCmd {
					register: RegisterName::default(),
					verb: Some(VerbCmd(1, Verb::NormalMode)),
					motion: Some(MotionCmd(self.count, motion)),
					flags: CmdFlags::empty(),
					raw_seq
				})
			}
			E(C::Esc, M::NONE) => {
				Some(ViCmd {
					register: RegisterName::default(),
					verb: Some(VerbCmd(1, Verb::NormalMode)),
					motion: None,
					flags: CmdFlags::empty(),
					raw_seq: "".into(),
				})
			}
			E(C::Char(ch), M::NONE) => {
				if self.next_is_escaped {
					self.pending_pattern.push(ch);
					self.next_is_escaped = false;
				} else {
					match ch {
						'\\' => {
							self.pending_pattern.push(ch);
							self.next_is_escaped = true;
						}
						_ => self.pending_pattern.push(ch),
					}
				}
				None
			}
			_ => common_cmds(key)
		}
	}

	fn is_repeatable(&self) -> bool {
		false
	}

	fn as_replay(&self) -> Option<super::CmdReplay> {
		None
	}

	fn cursor_style(&self) -> String {
		"\x1b[2 q".to_string()
	}

	fn pending_seq(&self) -> Option<String> {
		Some(self.pending_pattern.clone())
	}

	fn move_cursor_on_undo(&self) -> bool {
		false
	}

	fn clamp_cursor(&self) -> bool {
		true
	}

	fn hist_scroll_start_pos(&self) -> Option<crate::vicmd::To> {
		None
	}

	fn report_mode(&self) -> super::ModeReport {
		ModeReport::Search
	}
}
