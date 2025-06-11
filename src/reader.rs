use std::collections::VecDeque;

use crate::keys::{KeyCode, KeyEvent, ModKeys};

pub trait KeyReader {
	fn read_key(&mut self) -> Option<KeyEvent>;
}

#[derive(Default,Debug)]
pub struct RawReader {
	pub bytes: VecDeque<u8>,
	pub is_escaped: bool // The last byte was a backslash or not
}

impl RawReader {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_initial(mut self, bytes: &[u8]) -> Self {
		let bytes = bytes.iter();
		self.bytes.extend(bytes);
		self
	}

	pub fn load_bytes(&mut self, bytes: &[u8]) {
		self.bytes.clear();
		let bytes = bytes.iter();
		self.bytes.extend(bytes);
	}

	pub fn parse_esc_seq(&mut self) -> Option<KeyEvent> {
		let mut seq = vec![0x1b];
		let b1 = self.bytes.pop_front()?;
		seq.push(b1);

		match b1 {
			b'[' => {
				let b2 = self.bytes.pop_front()?;
				seq.push(b2);

				match b2 {
					b'A' => Some(KeyEvent(KeyCode::Up, ModKeys::empty())),
					b'B' => Some(KeyEvent(KeyCode::Down, ModKeys::empty())),
					b'C' => Some(KeyEvent(KeyCode::Right, ModKeys::empty())),
					b'D' => Some(KeyEvent(KeyCode::Left, ModKeys::empty())),
					b'1'..=b'9' => {
						let mut digits = vec![b2];

						while let Some(&b) = self.bytes.front() {
							seq.push(b);
							self.bytes.pop_front();

							if b == b'~' || b == b';' {
								break;
							} else if b.is_ascii_digit() {
								digits.push(b);
							} else {
								break;
							}
						}

						let key = match digits.as_slice() {
							[b'1'] => KeyCode::Home,
							[b'3'] => KeyCode::Delete,
							[b'4'] => KeyCode::End,
							[b'5'] => KeyCode::PageUp,
							[b'6'] => KeyCode::PageDown,
							[b'7'] => KeyCode::Home, // xterm alternate
							[b'8'] => KeyCode::End,  // xterm alternate

							[b'1', b'5'] => KeyCode::F(5),
							[b'1', b'7'] => KeyCode::F(6),
							[b'1', b'8'] => KeyCode::F(7),
							[b'1', b'9'] => KeyCode::F(8),
							[b'2', b'0'] => KeyCode::F(9),
							[b'2', b'1'] => KeyCode::F(10),
							[b'2', b'3'] => KeyCode::F(11),
							[b'2', b'4'] => KeyCode::F(12),
							_ => KeyCode::Esc,
						};

						Some(KeyEvent(key, ModKeys::empty()))
					}
					_ => Some(KeyEvent(KeyCode::Esc, ModKeys::empty())),
				}
			}

			b'O' => {
				let b2 = self.bytes.pop_front()?;
				seq.push(b2);

				let key = match b2 {
					b'P' => KeyCode::F(1),
					b'Q' => KeyCode::F(2),
					b'R' => KeyCode::F(3),
					b'S' => KeyCode::F(4),
					_ => KeyCode::Esc,
				};

				Some(KeyEvent(key, ModKeys::empty()))
			}

			_ => Some(KeyEvent(KeyCode::Esc, ModKeys::empty())),
		}
	}
	pub fn parse_byte_alias(&mut self) -> Option<KeyEvent> {
		let mut buf = vec![];
		let mut byte_iter = self.bytes.iter().copied();
		for b in byte_iter.by_ref() {
			match b {
				b'>' => break,
				_ => buf.push(b)
			}
		}

		if buf.is_empty() {
			return None
		}

		let mut mods = ModKeys::NONE;

		// Collect mod keys
		// Order does not matter here
		loop {
			if buf.as_slice().starts_with(b"c-") {
				mods |= ModKeys::CTRL;
				buf = buf[2..].to_vec();
			} else if buf.as_slice().starts_with(b"s-") {
				mods |= ModKeys::SHIFT;
				buf = buf[2..].to_vec();
			} else if buf.as_slice().starts_with(b"a-") {
				mods |= ModKeys::ALT;
				buf = buf[2..].to_vec();
			} else {
				break;
			}
		}

		let is_fn_key = buf.len() > 1 && buf[0] == b'f' && buf[1..].iter().all(|c| (*c as char).is_ascii_digit());
		let is_alphanum_key = buf.len() == 1 && (buf[0] as char).is_alphanumeric();

		// Match aliases
		let result = match buf.as_slice() {
			// Common weird keys
			b"esc" => Some(KeyEvent(KeyCode::Esc, mods)),
			b"return" |
			b"enter" => Some(KeyEvent(KeyCode::Enter, mods)),
			b"tab" => Some(KeyEvent(KeyCode::Char('\t'), mods)),
			b"bs" => Some(KeyEvent(KeyCode::Backspace, mods)),
			b"del" => Some(KeyEvent(KeyCode::Delete, mods)),
			b"ins" => Some(KeyEvent(KeyCode::Insert, mods)),
			b"home" => Some(KeyEvent(KeyCode::Home, mods)),
			b"end" => Some(KeyEvent(KeyCode::End, mods)),
			b"left" => Some(KeyEvent(KeyCode::Left, mods)),
			b"right" => Some(KeyEvent(KeyCode::Right, mods)),
			b"up" => Some(KeyEvent(KeyCode::Up, mods)),
			b"down" => Some(KeyEvent(KeyCode::Down, mods)),
			b"pgup" => Some(KeyEvent(KeyCode::PageUp, mods)),
			b"pgdown" => Some(KeyEvent(KeyCode::PageDown, mods)),

			// Check for alphanumeric keys
			b_ch if is_alphanum_key => Some(KeyEvent(KeyCode::Char((b_ch[0] as char).to_ascii_uppercase()), mods)),

			// Check for function keys
			_ if is_fn_key => {
				let stripped = buf.strip_prefix(b"f").unwrap();
				std::str::from_utf8(stripped)
					.ok()
					.and_then(|s| {
						s.parse::<u8>().ok()
					})
					.map(|n| KeyEvent(KeyCode::F(n), mods))
			}
			_ => None
		};
		if result.is_some() {
			self.bytes = byte_iter.collect();
		}
		result
	}
}

impl KeyReader for RawReader {
	fn read_key(&mut self) -> Option<KeyEvent> {
		use core::str;

		let mut collected = Vec::with_capacity(4);

		loop {
			let byte = self.bytes.pop_front()?;

			// Check for byte aliases like '<esc>' and '<c-w>'
			if byte == b'<' && !self.is_escaped {
				if let Some(key) = self.parse_byte_alias() {
					return Some(key)
				}
			}
			if byte == b'\\' {
				self.is_escaped = !self.is_escaped;
			} else {
				self.is_escaped = false;
			}

			collected.push(byte);

			// If it's an escape sequence, delegate
			if collected[0] == 0x1b && collected.len() == 1 {
				if let Some(&_next @ (b'[' | b'O')) = self.bytes.front() {
					let seq = self.parse_esc_seq();
					return seq
				}
			}

			// Try parse as valid UTF-8
			if let Ok(s) = str::from_utf8(&collected) {
				return Some(KeyEvent::new(s, ModKeys::empty()));
			}

			if collected.len() >= 4 {
				break;
			}
		}

		None
	}
}
