//! Logic describing Vim commands is held here.
//!
//! Parsing happens in the `modes` module. This module just holds the structs and enums.
//! ViCmd is described in this module, and is probably *the single most load-bearing struct in the codebase*.
use std::path::PathBuf;

use bitflags::bitflags;

use crate::{linebuf::SelectRange, modes::ex::SubFlags, register::{RegisterContent, REGISTERS}};

use super::register::{append_register, read_register, write_register};

//TODO: write tests that take edit results and cursor positions from actual neovim edits and test them against the behavior of this editor

#[derive(Clone,Copy,Debug,PartialEq)]
pub struct RegisterName {
	name: Option<char>,
	count: usize,
	append: bool
}

impl RegisterName {
	pub fn new(name: Option<char>, count: Option<usize>) -> Self {
		let Some(ch) = name else {
			return Self::default()
		};

		let append = ch.is_uppercase();
		let name = ch.to_ascii_lowercase();
		Self {
			name: Some(name),
			count: count.unwrap_or(1),
			append
		}
	}
	pub fn name(&self) -> Option<char> {
		self.name
	}
	pub fn is_append(&self) -> bool {
		self.append
	}
	pub fn count(&self) -> usize {
		self.count
	}
	pub fn is_line(&self) -> bool {
		REGISTERS.with_borrow(|reg| reg.get_reg(self.name).is_some_and(|r| r.is_line()))
	}
	pub fn is_block(&self) -> bool {
		REGISTERS.with_borrow(|reg| reg.get_reg(self.name).is_some_and(|r| r.is_block()))
	}
	pub fn is_span(&self) -> bool {
		REGISTERS.with_borrow(|reg| reg.get_reg(self.name).is_some_and(|r| r.is_span()))
	}
	pub fn write_to_register(&self, buf: RegisterContent) {
		if self.append {
			append_register(self.name, buf);
		} else {
			write_register(self.name, buf);
		}
	}
	pub fn read_from_register(&self) -> Option<RegisterContent> {
		read_register(self.name)
	}
}

impl Default for RegisterName {
	fn default() -> Self {
		Self {
			name: None,
			count: 1,
			append: false
		}
	}
}

bitflags! {
	#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct CmdFlags: u32 {
		const VISUAL = 1<<0;
		const VISUAL_LINE = 1<<1;
		const VISUAL_BLOCK = 1<<2;
		const EXIT_CUR_MODE = 1<<3; // for instance, when pressing enter during ex mode or search mode
	}
}

/// A Vim Command
///
/// ## Fields
/// `register`: The register to use for yank/delete/change/put.
/// `verb`: The verb to execute, if any.
/// `motion`: The motion to execute, if any.
/// `raw_seq`: The raw sequence of characters that produced this ViCmd.
/// `flags`: Bitflags which alter the execution properties of the command.
///
/// Used extensively throughout the `exec`, `modes`, and `linebuf` modules.
#[derive(Clone,Default,Debug,PartialEq)]
pub struct ViCmd {
	pub register: RegisterName,
	pub verb: Option<VerbCmd>,
	pub motion: Option<MotionCmd>,
	pub raw_seq: String,
	pub flags: CmdFlags,
}

impl ViCmd {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn set_motion(&mut self, motion: MotionCmd) {
		self.motion = Some(motion)
	}
	pub fn set_verb(&mut self, verb: VerbCmd) {
		self.verb = Some(verb)
	}
	pub fn verb(&self) -> Option<&VerbCmd> {
		self.verb.as_ref()
	}
	pub fn motion(&self) -> Option<&MotionCmd> {
		self.motion.as_ref()
	}
	pub fn verb_count(&self) -> usize {
		self.verb.as_ref().map(|v| v.0).unwrap_or(1)
	}
	pub fn motion_count(&self) -> usize {
		self.motion.as_ref().map(|m| m.0).unwrap_or(1)
	}
	/// Combine verb and motion counts
	///
	/// This makes things easier to execute later. This is always executed when a ViCmd is parsed.
	pub fn normalize_counts(&mut self) {
		let Some(verb) = self.verb.as_mut() else { return };
		let Some(motion) = self.motion.as_mut() else { return };
		let VerbCmd(v_count, _) = verb;
		let MotionCmd(m_count, _) = motion;
		let product = *v_count * *m_count;
		verb.0 = 1;
		motion.0 = product;
	}
	pub fn is_repeatable(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| v.1.is_repeatable())
	}
	pub fn is_cmd_repeat(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1,Verb::RepeatLast))
	}
	pub fn is_motion_repeat(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| matches!(m.1,Motion::RepeatMotion | Motion::RepeatMotionRev))
	}
	pub fn is_char_search(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| matches!(m.1, Motion::CharSearch(..)))
	}
	pub fn should_submit(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::AcceptLineOrNewline))
	}
	pub fn is_undo_op(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::Undo | Verb::Redo))
	}
	pub fn is_inplace_edit(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::ReplaceCharInplace(_,_) | Verb::ToggleCaseInplace(_))) &&
		self.motion.is_none()
	}
	pub fn is_ex_normal(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::Normal(_)))
	}
	pub fn is_ex_global(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| matches!(m.1, Motion::Global(_,_) | Motion::NotGlobal(_,_)))
	}
	pub fn is_line_motion(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| {
			matches!(m.1,
				Motion::LineUp |
				Motion::LineDown |
				Motion::LineUpCharwise |
				Motion::LineDownCharwise
			)
		})
	}
	/// If a ViCmd has a linewise motion, but no verb, we change it to charwise
	pub fn alter_line_motion_if_no_verb(&mut self) {
		if self.is_line_motion() && self.verb.is_none() && let Some(motion) = self.motion.as_mut() {
			match motion.1 {
				Motion::LineUp => motion.1 = Motion::LineUpCharwise,
				Motion::LineDown => motion.1 = Motion::LineDownCharwise,
				_ => unreachable!()
			}
		}
	}
	pub fn is_mode_transition(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| {
			matches!(v.1,
				Verb::Change |
				Verb::InsertMode |
				Verb::ExMode |
				Verb::SearchMode(_,_) |
				Verb::InsertModeLineBreak(_) |
				Verb::NormalMode |
				Verb::VisualModeSelectLast |
				Verb::VisualMode |
				Verb::VisualModeLine |
				Verb::VisualModeBlock |
				Verb::ReplaceMode
			)
		})
	}
}

/// A count, and a `Verb`
#[derive(Clone,Debug,PartialEq)]
pub struct VerbCmd(pub usize,pub Verb);
/// A count, and a `Motion`
#[derive(Clone,Debug,PartialEq)]
pub struct MotionCmd(pub usize,pub Motion);

impl MotionCmd {
	pub fn invert_char_motion(self) -> Self {
		let MotionCmd(count,Motion::CharSearch(dir, dest, ch)) = self else {
			unreachable!()
		};
		let new_dir = match dir {
			Direction::Forward => Direction::Backward,
			Direction::Backward => Direction::Forward,
		};
		MotionCmd(count,Motion::CharSearch(new_dir, dest, ch))
	}
}

/// Vim operators
///
/// This enum contains all of the currently supported Vim operators. These are parsed in `modes`, and executed in `linebuf`
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Verb {
	Delete,
	Change,
	Yank,
	Rot13,
	ReplaceChar(char), // char to replace with, number of chars to replace
	ReplaceCharInplace(char,u16), // char to replace with, number of chars to replace
	ToggleCaseInplace(u16), // Number of chars to toggle
	ToggleCaseRange,
	ToLower,
	ToUpper,
	Complete,
	CompleteBackward,
	Undo,
	Redo,
	RepeatLast,
	Put(Anchor),
	/// (old_pat,new_pat,flags)
	Substitute(String,String,SubFlags),
	RepeatSubstitute,
	RepeatGlobal,
	Read(ReadSrc),
	Write(WriteDest),
	SearchMode(usize,Direction),
	Normal(String), // ex mode 'normal!'
	ReplaceMode,
	ExMode,
	InsertMode,
	InsertModeLineBreak(Anchor),
	NormalMode,
	VisualMode,
	VisualModeLine,
	VisualModeBlock,
	VisualModeSelectLast,
	SwapVisualAnchor,
	JoinLines,
	InsertChar(char),
	Insert(String),
	Indent,
	Dedent,
	Equalize,
	AcceptLineOrNewline,
	EndOfFile
}


impl Verb {
	pub fn is_repeatable(&self) -> bool {
		matches!(self,
			Self::Delete |
			Self::Change |
			Self::ReplaceChar(_) |
			Self::ReplaceCharInplace(_,_) |
			Self::ToLower |
			Self::ToUpper |
			Self::ToggleCaseRange |
			Self::ToggleCaseInplace(_) |
			Self::Put(_) |
			Self::ReplaceMode |
			Self::InsertModeLineBreak(_) |
			Self::JoinLines |
			Self::InsertChar(_) |
			Self::Insert(_) |
			Self::Indent |
			Self::Dedent |
			Self::Equalize
		)
	}
	pub fn is_edit(&self) -> bool {
		matches!(self,
			Self::Delete |
			Self::Change |
			Self::ReplaceChar(_) |
			Self::ReplaceCharInplace(_,_) |
			Self::ToggleCaseRange |
			Self::ToggleCaseInplace(_) |
			Self::ToLower |
			Self::ToUpper |
			Self::RepeatLast |
			Self::Put(_) |
			Self::ReplaceMode |
			Self::InsertModeLineBreak(_) |
			Self::JoinLines |
			Self::InsertChar(_) |
			Self::Insert(_) |
			Self::Rot13 |
			Self::EndOfFile
		)
	}
	pub fn is_char_insert(&self) -> bool {
		matches!(self,
			Self::Change |
			Self::InsertChar(_) |
			Self::ReplaceChar(_) |
			Self::ReplaceCharInplace(_,_)
		)
	}
}

/// Vim motions
///
/// This enum contains all of the currently supported Vim motions. These are parsed in `modes`, and executed in `linebuf`
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Motion {
	WholeLine,
	WholeLineExclusive,
	TextObj(TextObj),
	EndOfLastWord,
	BeginningOfFirstWord,
	BeginningOfLine,
	EndOfLine,
	WordMotion(To,Word,Direction),
	CharSearch(Direction,Dest,char),
	Line(LineAddr), 							// x
	LineRange(LineAddr,LineAddr), // x,y
	PatternSearch(String),
	PatternSearchRev(String),
	/// The first field should *always* be `Line(_)` or `LineRange(_,_)`
	Global(Box<Motion>,String),
	/// The first field should *always* be `Line(_)` or `LineRange(_,_)`
	NotGlobal(Box<Motion>,String),
	NextMatch,
	PrevMatch,
	BackwardChar,
	ForwardChar,
	/// Can cross line boundaries
	BackwardCharForced,
	/// Can cross line boundaries
	ForwardCharForced,
	LineUp,
	LineDown,
	LineUpCharwise,
	LineDownCharwise,
	WholeBuffer,
	BeginningOfBuffer,
	EndOfBuffer,
	ToColumn,
	ToDelimMatch,
	ToBrace(Direction),
	ToBracket(Direction),
	ToParen(Direction),
	Range(SelectRange),
	RepeatMotion,
	RepeatMotionRev,

	// TODO: Not sure how to implement these in a non-interactive way...
	ScreenLineUp,
	ScreenLineUpCharwise,
	ScreenLineDown,
	ScreenLineDownCharwise,
	BeginningOfScreenLine,
	FirstGraphicalOnScreenLine,
	HalfOfScreen,
	HalfOfScreenLineText,
	Null
}

impl Motion {
	pub fn is_exclusive(&self) -> bool {
		matches!(&self,
			Self::BeginningOfLine |
			Self::BeginningOfFirstWord |
			Self::BeginningOfScreenLine |
			Self::FirstGraphicalOnScreenLine |
			Self::LineDownCharwise |
			Self::LineUpCharwise |
			Self::ScreenLineUpCharwise |
			Self::ScreenLineDownCharwise |
			Self::ToColumn |
			Self::TextObj(TextObj::Sentence(_)) |
			Self::TextObj(TextObj::Paragraph(_)) |
			Self::CharSearch(Direction::Backward, _, _) |
			Self::WordMotion(To::Start,_,_) |
			Self::ToBrace(_) |
			Self::ToBracket(_) |
			Self::ToParen(_) |
			Self::ScreenLineDown |
			Self::ScreenLineUp |
			Self::Range(_)
		)
	}
	pub fn is_linewise(&self) -> bool {
		matches!(self,
			Self::WholeLine |
			Self::LineUp |
			Self::LineDown |
			Self::ScreenLineDown |
			Self::ScreenLineUp
		)
	}
}

/// Apply a verb before, or after the target
///
/// Used with stuff like `put` to choose where to perform the action
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Anchor {
	After,
	Before
}

/// Vim Text Objects
///
/// Used with Motion::TextObj(_)
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TextObj {
	/// `iw`, `aw` — inner word, around word
	Word(Word, Bound),

	/// `)`, `(` — forward, backward
	Sentence(Direction),

	/// `}`, `{` — forward, backward
	Paragraph(Direction),

	WholeSentence(Bound),
	WholeParagraph(Bound),

	/// `i"`, `a"` — inner/around double quotes
	DoubleQuote(Bound),
	/// `i'`, `a'`
	SingleQuote(Bound),
	/// `i\``, `a\``
	BacktickQuote(Bound),

	/// `i)`, `a)` — round parens
	Paren(Bound),
	/// `i]`, `a]`
	Bracket(Bound),
	/// `i}`, `a}`
	Brace(Bound),
	/// `i<`, `a<`
	Angle(Bound),

	/// `it`, `at` — HTML/XML tags
	Tag(Bound),

	/// Custom user-defined objects maybe?
	Custom(char),
}

/// The source to read from for ex mode's `:r`
///
/// * `:r <FILE>` -> ReadSrc::File(_)
/// * `:r !<COMMAND>` -> ReadSrc::Cmd(_)
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReadSrc {
	File(PathBuf),
	Cmd(String)
}

/// The target for ex mode's `:w`
///
/// * `:w <FILE>` -> WriteDest::File(_)
/// * `:w >> <FILE>` -> WriteDest::FileAppend(_)
/// * `:w !<COMMAND>` -> WriteDest::Cmd(_)
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum WriteDest {
	File(PathBuf),
	FileAppend(PathBuf),
	Cmd(String),
}

/// Line Addresses used by ex mode
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum LineAddr {
	Number(usize),
	Current,
	Last,
	Offset(isize),
	Pattern(String),
	PatternRev(String),
}

/// Word sizes for motions like 'w' and 'B'
///
/// `Word::Big` counts any span of non-whitespace characters as a word
/// `Word::Normal` counts a non-whitespace span of similar characters as a word
/// a span of alphanumeric characters is a `Word::Normal,` and a span of symbols is a `Word::Normal`
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Word {
	Big,
	Normal
}

/// Text Object bounds
///
/// Whether to take the inside of a text object or the entire thing
/// For instance, 'di)' uses Bound::Inside, so `d` will only delete inside the parenthesis
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Bound {
	Inside,
	Around
}

/// Motion Direction
///
/// Used mainly for Motions, but is also repurposed in some places where direction matters for the logic
#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub enum Direction {
	#[default]
	Forward,
	Backward
}

/// Target destination for Char search motions
///
/// `t` uses Dest::Before
/// `f` uses Dest::On
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Dest {
	On,
	Before,
}

/// Target destination for Word motions
///
/// `To::Start` attempts to move to the start of a Word
/// `To::End` attempts to move to the end of a Word
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum To {
	Start,
	End
}
