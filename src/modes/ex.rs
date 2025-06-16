use std::{iter::Peekable, str::Chars};

use bitflags::bitflags;
use itertools::Itertools;

use crate::{modes::{common_cmds, ModeReport, ViMode}, vicmd::{Anchor, CmdFlags, LineAddr, Motion, MotionCmd, RegisterName, Verb, VerbCmd, ViCmd}};

bitflags! {
	#[derive(Debug,Clone,Copy,PartialEq,Eq)]
	pub struct SubFlags: u16 {
		const GLOBAL           = 1 << 0; // g
		const CONFIRM          = 1 << 1; // c (probably not implemented)
		const IGNORE_CASE      = 1 << 2; // i
		const NO_IGNORE_CASE   = 1 << 3; // I
		const SHOW_COUNT       = 1 << 4; // n
		const PRINT_RESULT     = 1 << 5; // p
		const PRINT_NUMBERED   = 1 << 6; // #
		const PRINT_LEFT_ALIGN = 1 << 7; // l
	}
}

pub struct ViEx {
	pending_cmd: String,
	select_range: Option<(usize,usize)>,
}

impl ViEx {
	pub fn new(select_range: Option<(usize,usize)>) -> Self {
		Self {
			pending_cmd: Default::default(),
			select_range
		}
	}
}

impl ViMode for ViEx {
	// Ex mode can return errors, so we use this fallible method instead of the normal one
	fn handle_key_fallible(&mut self, key: crate::keys::KeyEvent) -> Result<Option<ViCmd>,String> {
		use crate::keys::{KeyEvent as E, KeyCode as C, ModKeys as M};
		match key {
			E(C::Char('\r'), M::NONE) |
			E(C::Enter, M::NONE) => {
				match parse_ex_cmd(&self.pending_cmd, self.select_range) {
					Ok(cmd) => Ok(cmd),
					Err(_) => Err(format!("Not an editor command: {}",&self.pending_cmd))
				}
			}
			E(C::Esc, M::NONE) => {
				Ok(Some(ViCmd {
					register: RegisterName::default(),
					verb: Some(VerbCmd(1, Verb::NormalMode)),
					motion: None,
					flags: CmdFlags::empty(),
					raw_seq: "".into(),
				}))
			}
			E(C::Char(ch), M::NONE) => {
				self.pending_cmd.push(ch);
				Ok(None)
			}
			_ => Ok(common_cmds(key))
		}
	}
	fn handle_key(&mut self, key: crate::keys::KeyEvent) -> Option<crate::vicmd::ViCmd> {
		self.handle_key_fallible(key).ok().flatten()
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
		Some(self.pending_cmd.clone())
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
		ModeReport::Ex
	}
}


fn parse_ex_cmd(raw: &str, select_range: Option<(usize,usize)>) -> Result<Option<ViCmd>,()> {
	let raw = raw.trim();
	if raw.is_empty() {
		return Ok(None)
	}
	let mut chars = raw.chars().peekable();
	let motion = if let Some(range) = select_range {
		Some(MotionCmd(1,Motion::Range(range.0,range.1)))
	} else {
		parse_ex_address(&mut chars)?.map(|m| MotionCmd(1, m))
	};
	let verb = parse_ex_command(&mut chars)?.map(|v| VerbCmd(1, v));

	Ok(Some(ViCmd {
		register: RegisterName::default(),
		verb,
		motion,
		raw_seq: raw.to_string(),
		flags: CmdFlags::EXIT_CUR_MODE,
	}))
}

fn parse_ex_address(chars: &mut Peekable<Chars<'_>>) -> Result<Option<Motion>,()> {
	if chars.peek() == Some(&'%') {
		chars.next();
		return Ok(Some(Motion::LineRange(LineAddr::Number(1),LineAddr::Last)))
	}
	let Some(start) = parse_one_addr(chars)? else { return Ok(Some(Motion::WholeLine)) };
	if let Some(&',') = chars.peek() {
		chars.next();
		let Some(end) = parse_one_addr(chars)? else { return Ok(Some(Motion::WholeLine)) };
		Ok(Some(Motion::LineRange(start, end)))
	} else {
		Ok(Some(Motion::Line(start)))
	}
}

fn parse_one_addr(chars: &mut Peekable<Chars<'_>>) -> Result<Option<LineAddr>,()> {
	let Some(first) = chars.next() else { return Ok(None) };
	match first {
		'0'..='9' => {
			let mut digits = String::new();
			digits.push(first);
			digits.extend(chars.peeking_take_while(|c| c.is_ascii_digit()));

			let number = digits.parse::<usize>()
				.map_err(|_| ())?;

			Ok(Some(LineAddr::Number(number)))
		}
		'+' | '-' => {
			let mut digits = String::new();
			digits.push(first);
			digits.extend(chars.peeking_take_while(|c| c.is_ascii_digit()));

			let number = digits.parse::<isize>()
				.map_err(|_| ())?;

			Ok(Some(LineAddr::Offset(number)))
		}
		'/' | '?' => {
			let mut pattern = String::new();
			while let Some(ch) = chars.next() {
				match ch {
					'\\' => {
						pattern.push('\\');
						if let Some(esc_ch) = chars.next() {
							pattern.push(esc_ch)
						}
					}
					_ if ch == first => break,
					_ => pattern.push(ch)
				}
			}
			match first {
				'/' => Ok(Some(LineAddr::Pattern(pattern))),
				'?' => Ok(Some(LineAddr::PatternRev(pattern))),
				_ => unreachable!()
			}
			
		}
		'.' => Ok(Some(LineAddr::Current)),
		'$' => Ok(Some(LineAddr::Last)),
		_ => Err(())
	}
}

fn parse_ex_command(chars: &mut Peekable<Chars<'_>>) -> Result<Option<Verb>,()> {
	let Some(first) = chars.next() else {
		return Ok(None)
	};

	match first {
		'd' => Ok(Some(Verb::Delete)),
		'y' => Ok(Some(Verb::Yank)),
		'p' => Ok(Some(Verb::Put(Anchor::After))),
		's' => parse_substitute(chars),
		_ => Err(())
	}
}

fn parse_substitute(chars: &mut Peekable<Chars<'_>>) -> Result<Option<Verb>,()> {
	let Some(delimiter) = chars.next() else {
		return Ok(Some(Verb::RepeatSubstitute))
	};
	if delimiter.is_alphanumeric() {
		return Err(())
	}
	let old_pat = parse_sub_pattern(chars, delimiter)?;
	let new_pat = parse_sub_pattern(chars, delimiter)?;
	let mut flags = SubFlags::empty();
	while let Some(ch) = chars.next() {
		match ch {
			'g' => flags |= SubFlags::GLOBAL,
			'i' => flags |= SubFlags::IGNORE_CASE,
			'I' => flags |= SubFlags::NO_IGNORE_CASE,
			'n' => flags |= SubFlags::SHOW_COUNT,
			_ => return Err(())
		}
	}
	Ok(Some(Verb::Substitute(old_pat, new_pat, flags)))
}

fn parse_sub_pattern(chars: &mut Peekable<Chars<'_>>, delimiter: char) -> Result<String,()> {
	let mut pat = String::new();
	let mut closed = false;
	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				if chars.peek().is_some_and(|c| *c == delimiter) {
					// We escaped the delimiter, so we consume the escape char and continue
					pat.push(chars.next().unwrap());
					continue
				} else {
					// The escape char is probably for the regex in the pattern
					pat.push(ch);
					if let Some(esc_ch) = chars.next() {
						pat.push(esc_ch)
					}
				}
			}
			_ if ch == delimiter => {
				closed = true;
				break
			}
			_ => pat.push(ch)
		}
	}
	if !closed {
		return Err(())
	} else {
		Ok(pat)
	}
}
