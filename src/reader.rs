use std::collections::VecDeque;

use crate::keys::{KeyCode, KeyEvent, ModKeys};

pub trait KeyReader {
	fn read_key(&mut self) -> Option<KeyEvent>;
}

#[derive(Default,Debug)]
pub struct RawReader {
	pub bytes: VecDeque<u8>
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

	pub fn parse_esc_seq_from_bytes(&mut self) -> Option<KeyEvent> {
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
}

impl KeyReader for RawReader {
	fn read_key(&mut self) -> Option<KeyEvent> {
		use core::str;

		let mut collected = Vec::with_capacity(4);

		loop {
			let byte = self.bytes.pop_front()?;
			collected.push(byte);

			// If it's an escape sequence, delegate
			if collected[0] == 0x1b && collected.len() == 1 {
				if let Some(&_next @ (b'[' | b'0')) = self.bytes.front() {
					println!("found escape seq");
					let seq = self.parse_esc_seq_from_bytes();
					println!("{seq:?}");
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
