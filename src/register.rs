//! This module contains logic for emulation of Vim's registers feature.
//!
//! It contains the `Registers` struct, which is held in a thread local, global variable.
use std::{cell::RefCell, fmt::Display};

thread_local! {
	/// The global state for all registers.
	///
	/// This variable is thread local, so it can be freely mutated.
	pub static REGISTERS: RefCell<Registers> = const { RefCell::new(Registers::new()) };
}

/// Attempt to read from the register corresponding to the given character
pub fn read_register(ch: Option<char>) -> Option<RegisterContent> {
	REGISTERS.with_borrow(|regs| regs.get_reg(ch).map(|r| r.content().clone()))
}

/// Attempt to write to the register corresponding to the given character
pub fn write_register(ch: Option<char>, buf: RegisterContent) {
	REGISTERS.with_borrow_mut(|regs| if let Some(r) = regs.get_reg_mut(ch) { r.write(buf); })
}

/// Attempt to append text to the register corresponding to the given character
pub fn append_register(ch: Option<char>, buf: RegisterContent) {
	REGISTERS.with_borrow_mut(|regs| if let Some(r) = regs.get_reg_mut(ch) { r.append(buf) })
}

#[derive(Default,Debug)]
pub struct Registers {
	default: Register,
	a: Register,
	b: Register,
	c: Register,
	d: Register,
	e: Register,
	f: Register,
	g: Register,
	h: Register,
	i: Register,
	j: Register,
	k: Register,
	l: Register,
	m: Register,
	n: Register,
	o: Register,
	p: Register,
	q: Register,
	r: Register,
	s: Register,
	t: Register,
	u: Register,
	v: Register,
	w: Register,
	x: Register,
	y: Register,
	z: Register,
}

impl Registers {
	pub const fn new() -> Self {
		// Wish I could use Self::default() here
		// but Default::default() is not a const fn
		// So here we go
		Self {
			default: Register::new(),
			a: Register::new(),
			b: Register::new(),
			c: Register::new(),
			d: Register::new(),
			e: Register::new(),
			f: Register::new(),
			g: Register::new(),
			h: Register::new(),
			i: Register::new(),
			j: Register::new(),
			k: Register::new(),
			l: Register::new(),
			m: Register::new(),
			n: Register::new(),
			o: Register::new(),
			p: Register::new(),
			q: Register::new(),
			r: Register::new(),
			s: Register::new(),
			t: Register::new(),
			u: Register::new(),
			v: Register::new(),
			w: Register::new(),
			x: Register::new(),
			y: Register::new(),
			z: Register::new(),
		}
	}
	/// Get a register by name. Read only.
	pub fn get_reg(&self, ch: Option<char>) -> Option<&Register> {
		let Some(ch) = ch else {
			return Some(&self.default)
		};
		match ch {
			'a' => Some(&self.a),
			'b' => Some(&self.b),
			'c' => Some(&self.c),
			'd' => Some(&self.d),
			'e' => Some(&self.e),
			'f' => Some(&self.f),
			'g' => Some(&self.g),
			'h' => Some(&self.h),
			'i' => Some(&self.i),
			'j' => Some(&self.j),
			'k' => Some(&self.k),
			'l' => Some(&self.l),
			'm' => Some(&self.m),
			'n' => Some(&self.n),
			'o' => Some(&self.o),
			'p' => Some(&self.p),
			'q' => Some(&self.q),
			'r' => Some(&self.r),
			's' => Some(&self.s),
			't' => Some(&self.t),
			'u' => Some(&self.u),
			'v' => Some(&self.v),
			'w' => Some(&self.w),
			'x' => Some(&self.x),
			'y' => Some(&self.y),
			'z' => Some(&self.z),
			_ => None
		}
	}
	/// Get a mutable reference to a register by name.
	pub fn get_reg_mut(&mut self, ch: Option<char>) -> Option<&mut Register> {
		let Some(ch) = ch else {
			return Some(&mut self.default)
		};
		match ch {
			'a' => Some(&mut self.a),
			'b' => Some(&mut self.b),
			'c' => Some(&mut self.c),
			'd' => Some(&mut self.d),
			'e' => Some(&mut self.e),
			'f' => Some(&mut self.f),
			'g' => Some(&mut self.g),
			'h' => Some(&mut self.h),
			'i' => Some(&mut self.i),
			'j' => Some(&mut self.j),
			'k' => Some(&mut self.k),
			'l' => Some(&mut self.l),
			'm' => Some(&mut self.m),
			'n' => Some(&mut self.n),
			'o' => Some(&mut self.o),
			'p' => Some(&mut self.p),
			'q' => Some(&mut self.q),
			'r' => Some(&mut self.r),
			's' => Some(&mut self.s),
			't' => Some(&mut self.t),
			'u' => Some(&mut self.u),
			'v' => Some(&mut self.v),
			'w' => Some(&mut self.w),
			'x' => Some(&mut self.x),
			'y' => Some(&mut self.y),
			'z' => Some(&mut self.z),
			_ => None
		}
	}
}

#[derive(Default,Clone,Debug)]
pub enum RegisterContent {
	Span(String),
	Line(String),
	Block(Vec<String>),
	#[default]
	Empty
}

impl RegisterContent {
	pub fn clear(&mut self) {
		match self {
			Self::Span(s) => s.clear(),
			Self::Line(s) => s.clear(),
			Self::Block(v) => v.clear(),
			Self::Empty => {}
		}
	}
	pub fn len(&self) -> usize {
		match self {
			Self::Span(s) => s.len(),
			Self::Line(s) => s.len(),
			Self::Block(v) => v.len(),
			Self::Empty => 0
		}
	}
	pub fn is_empty(&self) -> bool {
		match self {
			Self::Span(s) => s.is_empty(),
			Self::Line(s) => s.is_empty(),
			Self::Block(v) => v.is_empty(),
			Self::Empty => true
		}
	}
}

impl Display for RegisterContent {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Span(s) => write!(f, "{}", s),
			Self::Line(s) => write!(f, "{}", s),
			Self::Block(v) => write!(f, "{}", v.join("\n")),
			Self::Empty => write!(f, "")
		}
	}
}

/// A single register.
///
/// The `is_whole_line` field is flipped to `true` when you do something like `dd` to delete an entire line
/// If content is `put` from the register, behavior changes depending on this field.
#[derive(Clone,Default,Debug)]
pub struct Register {
	content: RegisterContent,
}
impl Register {
	pub const fn new() -> Self {
		Self {
			content: RegisterContent::Span(String::new()),
		}
	}
	pub fn content(&self) -> &RegisterContent {
		&self.content
	}
	pub fn write(&mut self, buf: RegisterContent) {
		self.content = buf
	}
	pub fn append(&mut self, buf: RegisterContent) {
		match buf {
			RegisterContent::Empty => {},
			RegisterContent::Span(ref s) |
			RegisterContent::Line(ref s) => {
				match &mut self.content {
					RegisterContent::Empty => self.content = buf,
					RegisterContent::Span(existing) => existing.push_str(s),
					RegisterContent::Line(existing) => existing.push_str(s),
					RegisterContent::Block(_) => {
						self.content = buf
					}
				}
			}
			RegisterContent::Block(v) => {
				match &mut self.content {
					RegisterContent::Block(existing) => existing.extend(v),
					_ => {
						self.content = RegisterContent::Block(v);
					}
				}
			}
		}
	}
	pub fn clear(&mut self) {
		self.content.clear()
	}
	pub fn is_line(&self) -> bool {
		matches!(self.content, RegisterContent::Line(_))
	}
	pub fn is_block(&self) -> bool {
		matches!(self.content, RegisterContent::Block(_))
	}
	pub fn is_span(&self) -> bool {
		matches!(self.content, RegisterContent::Span(_))
	}
}
