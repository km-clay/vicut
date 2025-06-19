use std::{cell::RefCell, sync::Mutex};

thread_local! {
	pub static REGISTERS: RefCell<Registers> = const { RefCell::new(Registers::new()) };
}

pub fn read_register(ch: Option<char>) -> Option<String> {
	REGISTERS.with_borrow(|regs| regs.get_reg(ch).map(|r| r.buf().clone()))
}

pub fn write_register(ch: Option<char>, buf: String, is_whole_line: bool) {
	REGISTERS.with_borrow_mut(|regs| if let Some(r) = regs.get_reg_mut(ch) { r.write(buf); r.set_is_whole_line(is_whole_line); })
}

pub fn append_register(ch: Option<char>, buf: String) {
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
		// default() isn't constant :(
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

#[derive(Clone,Default,Debug)]
pub struct Register {
	content: String,
	is_whole_line: bool
}
impl Register {
	pub const fn new() -> Self {
		Self {
			content: String::new(),
			is_whole_line: false
		}
	}
	pub fn buf(&self) -> &String {
		&self.content
	}
	pub fn write(&mut self, buf: String) {
		self.content = buf
	}
	pub fn append(&mut self, buf: String) {
		self.content.push_str(&buf)
	}
	pub fn clear(&mut self) {
		self.content.clear()
	}
	pub fn is_whole_line(&self) -> bool {
		self.is_whole_line
	}
	pub fn set_is_whole_line(&mut self, yn: bool) {
		self.is_whole_line = yn;
	}
}
