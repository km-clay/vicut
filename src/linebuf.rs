//! This module contains the core editor logic. This logic is held in the monolithic `LineBuf` struct.
//!
//! `LineBuf` is responsible for any and all mutations of the internal buffer.

use std::cmp::Ordering;
use std::env;
use std::io::Write as IoWrite;
use std::process::{Command,Stdio};
use std::ops::{Range, RangeInclusive};
use std::fmt::Write;

use regex::Regex;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::exec::Val;
use crate::register::RegisterContent;
use crate::{modes::ex::SubFlags, vicmd::{LineAddr, ReadSrc, WriteDest}};

use super::vicmd::{Anchor, Bound, CmdFlags, Dest, Direction, Motion, MotionCmd, RegisterName, TextObj, To, Verb, ViCmd, Word};

const PUNCTUATION: [&str;3] = [
	"?",
	"!",
	"."
];

#[derive(PartialEq,Eq,Debug,Clone,Copy)]
pub enum Delim {
	Paren,
	Brace,
	Bracket,
	Angle
}

#[repr(u8)]
#[derive(Default,PartialEq,Eq,Debug,Clone,Copy)]
/// Categories of various characters.
///
/// This enum is used with std::mem::transmute() in it's From<&str> impl.
/// Keep that in mind if you decide to mess with this
pub enum CharClass {
	#[default]
	Symbol = 0b00,
	Whitespace = 0b01,
	Alphanum = 0b10,
	Other = 0b11
}

impl From<&str> for CharClass {
	/// Convert a str into a CharClass
	///
	/// It is imperative that this function is *as fast as humanly possible*.
	/// CharClass conversion and comparison exists multiple times in nearly every hot path of this codebase.
	fn from(value: &str) -> Self {
		let mut chars = value.chars();

		let Some(first) = chars.next() else {
			return Self::Other;
		};

		// 0b10 = alphanumeric
		// 0b01 = whitespace
		// 0b00 = symbol
		// 0b11 = something weird?
		let mut flags = 0u8;

		match first {
			c if c.is_alphanumeric() || c == '_' => flags |= 0b10,
			c if c.is_whitespace() => flags |= 0b01,
			_ => {}
		}

		if value.len() == first.len_utf8() {
			// HOCUS POCUS
			return unsafe { std::mem::transmute::<u8, CharClass>(flags) }
		}

		for c in value[first.len_utf8()..].chars() {
			match c {
				c if c.is_alphanumeric() || c == '_' => flags |= 0b10,
				c if c.is_whitespace() => flags |= 0b01,
				_ => {}
			}
			if flags == 0b11 {
				return CharClass::Other;
			}
		}

		unsafe { std::mem::transmute::<u8, CharClass>(flags) }
	}
}

impl From<char> for CharClass {
	fn from(value: char) -> Self {
		let mut buf = [0u8; 4]; // max UTF-8 char size
		let slice = value.encode_utf8(&mut buf); // get str slice
		CharClass::from(slice as &str)
	}
}

fn is_whitespace(a: &str) -> bool {
	CharClass::from(a) == CharClass::Whitespace
}

fn is_other_class(a: &str, b: &str) -> bool {
	let a = CharClass::from(a);
	let b = CharClass::from(b);
	a != b
}

fn is_other_class_not_ws(a: &str, b: &str) -> bool {
	if is_whitespace(a) || is_whitespace(b) {
		false
	} else {
		is_other_class(a, b)
	}
}

fn is_other_class_or_is_ws(a: &str, b: &str) -> bool {
	if is_whitespace(a) || is_whitespace(b) {
		true
	} else {
		is_other_class(a, b)
	}
}

#[derive(Clone,PartialEq,Eq,Debug)]
pub enum SelectRange {
	OneDim((usize,usize)), // (start,end)
	TwoDim(Vec<(usize,usize)>), // (start,end) pairs
}

/// The side of the selection that is anchored in place.
///
/// Start means the anchor is on the left side of the selection,
/// End means the anchor is on the right side of the selection.
/// The cursor is always on the opposite side of the anchor.
#[derive(Default,Clone,Copy,PartialEq,Eq,Debug)]
pub enum SelectAnchor {
	#[default]
	End,
	Start
}

/// Visual selection modes
#[derive(Clone,Copy,PartialEq,Eq,Debug)]
pub enum SelectMode {
	Char(SelectAnchor),
	Line(SelectAnchor),
	// Block select is weird, we can't just swap to the other side of the selection
	// We have to calculate the anchor position and the column offset
	Block { anchor: SelectAnchor, anchor_pos: usize }
}

impl SelectMode {
	pub fn set_anchor(&mut self, anchor: SelectAnchor) {
		match self {
			SelectMode::Char(a) |
			SelectMode::Line(a) |
			SelectMode::Block{ anchor: a, .. } => *a = anchor
		}
	}
	pub fn anchor(&self) -> &SelectAnchor {
		match self {
			SelectMode::Char(anchor) |
				SelectMode::Line(anchor) |
				SelectMode::Block{ anchor, .. } => anchor
		}
	}
	pub fn invert_anchor(&mut self) {
		match self {
			SelectMode::Char(anchor) |
				SelectMode::Line(anchor) |
				SelectMode::Block{ anchor, .. } => {
					*anchor = match anchor {
						SelectAnchor::End => SelectAnchor::Start,
						SelectAnchor::Start => SelectAnchor::End
					}
				}
		}
	}
}

/// The main driver for motion logic in `LineBuf`
///
/// All of the motions passed in through `ViCmd`s are eventually watered down to one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MotionKind {
	To(usize), // Absolute position, exclusive
	On(usize), // Absolute position, inclusive
	Onto(usize), // Absolute position, operations include the position but motions exclude it (wtf vim)
	Inclusive((usize,usize)), // Range, inclusive
	Exclusive((usize,usize)), // Range, exclusive
	Line(usize),
	/// A list of specific lines
	Lines(Vec<usize>),
	/// A range between a start and end line
	LineRange(usize,usize),
	LineOffset(isize), // Relative to the current line

	BlockRange(Vec<(usize,usize)>), // Windows of lines
	// Used for linewise operations like 'dj', left is the selected range, right is the cursor's new position on the line
	InclusiveWithTargetCol((usize,usize),usize),
	ExclusiveWithTargetCol((usize,usize),usize),
	Null
}

impl MotionKind {
	pub fn inclusive(range: RangeInclusive<usize>) -> Self {
		Self::Inclusive((*range.start(),*range.end()))
	}
	pub fn exclusive(range: Range<usize>) -> Self {
		Self::Exclusive((range.start,range.end))
	}
	pub fn from_select_range(range: SelectRange) -> Self {
		match range {
			SelectRange::OneDim((start,end)) => Self::Inclusive((start,end)),
			SelectRange::TwoDim(lines) => Self::BlockRange(lines.clone())
		}
	}
}

/// Used for undo/redo logic.
#[derive(Clone,Default,Debug)]
pub struct Edit {
	pub pos: usize,
	pub cursor_pos: usize,
	pub merge_pos: usize,
	pub old: String,
	pub old_diff: String,
	pub new: String,
	pub new_diff: String,
	pub merging: bool,
}

impl Edit {
	pub fn diff(a: &str, b: &str, old_cursor_pos: usize) -> Edit {
		use std::cmp::min;

		let mut start = 0;
		let max_start = min(a.len(), b.len());

		// Calculate the prefix of the edit
		while start < max_start && a.as_bytes()[start] == b.as_bytes()[start] {
			start += 1;
		}

		if start == a.len() && start == b.len() {
			return Edit {
				pos: start,
				merge_pos: 0,
				cursor_pos: old_cursor_pos,
				old: a.to_string(),
				old_diff: String::new(),
				new: b.to_string(),
				new_diff: String::new(),
				merging: false,
			};
		}

		let mut end_a = a.len();
		let mut end_b = b.len();

		// Calculate the suffix of the edit
		while end_a > start && end_b > start && a.as_bytes()[end_a - 1] == b.as_bytes()[end_b - 1] {
			end_a -= 1;
			end_b -= 1;
		}

		// Slice off the prefix and suffix for both (safe because start/end are byte offsets)
		let old_diff = a[start..end_a].to_string();
		let new_diff = b[start..end_b].to_string();

		Edit {
			pos: start,
			merge_pos: 0,
			cursor_pos: old_cursor_pos,
			old: a.to_string(),
			old_diff,
			new: b.to_string(),
			new_diff,
			merging: false
		}
	}
	pub fn get_raw_diff(&self) -> String {
		let old_tail = &self.old[self.pos..];
		let new_tail = &self.new[self.pos..];

		// Find the common suffix length
		let mut suffix_len = 0;
		while suffix_len < old_tail.len()
			&& suffix_len < new_tail.len()
			&& old_tail.as_bytes()[old_tail.len() - 1 - suffix_len]
			== new_tail.as_bytes()[new_tail.len() - 1 - suffix_len]
			{
				suffix_len += 1;
			}

		// The raw diff is the inserted/changed portion of `new`
		self.new[self.pos..self.new.len() - suffix_len].to_string()
	}
	pub fn get_len_delta(&self) -> isize {
		let old_len = self.old_diff.len() as isize;
		let new_len = self.new_diff.len() as isize;
		new_len - old_len
	}
	pub fn start_merge(&mut self) {
		self.merging = true
	}
	pub fn stop_merge(&mut self) {
		self.merging = false
	}
	pub fn is_empty(&self) -> bool {
		self.new.is_empty() &&
			self.old.is_empty()
	}
}

/// A `usize` which will always exist between `0` and a given upper bound
///
/// * The upper bound can be either inclusive or exclusive
/// * Used for the `LineBuf` cursor to strictly enforce the `0 <= cursor < self.buffer.len()` invariant.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ClampedUsize {
	value: usize,
	min: usize,
	max: usize,
	exclusive: bool
}

impl ClampedUsize {
	pub fn new(value: usize, max: usize, exclusive: bool) -> Self {
		let mut c = Self { value: 0, min: 0, max, exclusive };
		c.set(value);
		c
	}
	pub fn with_min(mut self, min: usize) -> Self {
		self.min = min;
		self.set(self.value);
		self
	}
	pub fn get(self) -> usize {
		self.value
	}
	pub fn cap(&self) -> usize {
		self.max
	}
	pub fn upper_bound(&self) -> usize {
		if self.exclusive {
			self.max.saturating_sub(1)
		} else {
			self.max
		}
	}
	/// Increment the ClampedUsize value
	///
	/// Returns false if the attempted increment is rejected by the clamp
	pub fn inc(&mut self) -> bool {
		let max = self.upper_bound();
		if self.value == max {
			return false;
		}
		self.add(1);
		true
	}
	/// Decrement the ClampedUsize value
	///
	/// Returns false if the attempted decrement would cause underflow
	pub fn dec(&mut self) -> bool {
		if self.value == 0 {
			return false;
		}
		self.sub(1);
		true
	}
	pub fn set(&mut self, value: usize) -> bool {
		let max = self.upper_bound();
		let before = self.value;
		self.value = value.clamp(0,max);
		let after = self.value;
		before != after
	}
	pub fn set_max(&mut self, max: usize) {
		self.max = max;
		self.set(self.get()); // Enforces the new maximum
	}
	pub fn add_signed(&mut self, value: isize) {
		if value < 0 {
			self.sub(value.unsigned_abs());
		} else {
			self.add(value as usize);
		}
	}
	pub fn ret_add_signed(&self, value: isize) -> usize {
		if value < 0 {
			self.ret_sub(value.unsigned_abs())
		} else {
			self.ret_add(value as usize)
		}
	}
	pub fn add(&mut self, value: usize) {
		let max = self.upper_bound();
		self.value = (self.value + value).clamp(0,max)
	}
	pub fn sub(&mut self, value: usize) {
		self.value = self.value.saturating_sub(value)
	}
	/// Add a value to the wrapped usize, return the result
	///
	/// Returns the result instead of mutating the inner value
	pub fn ret_add(&self, value: usize) -> usize {
		let max = self.upper_bound();
		(self.value + value).clamp(0,max)
	}
	/// Add a value to the wrapped usize, forcing inclusivity
	pub fn ret_add_inclusive(&self, value: usize) -> usize {
		let max = self.max;
		(self.value + value).clamp(0,max)
	}
	/// Subtract a value from the wrapped usize, return the result
	///
	/// Returns the result instead of mutating the inner value
	pub fn ret_sub(&self, value: usize) -> usize {
		self.value.saturating_sub(value)
	}
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct ClampedIsize {
	value: isize,
	min: isize,
	max: isize,
	exclusive: bool
}

impl ClampedIsize {
	pub fn new(value: isize, min: isize, max: isize, exclusive: bool) -> Self {
		let mut c = Self { value: 0, min, max, exclusive };
		c.set(value);
		c
	}
	/// Create a new `ClampedIsize` from a `ClampedUsize`
	///
	/// The `value` of the newly created `ClampedIsize` will be 0
	/// and `min` and `max` will be replaced by the offset of the `ClampedUsize` value.
	///
	/// For example:
	/// ```rust
	/// // Min is 2, max is 10, value is 5
	/// let clamped_usize = ClampedUsize::new(5, 10, false).with_min(2);
	/// let clamped_isize = ClampedIsize::from_clamped_usize(clamped_usize);
	/// assert_eq!(clamped_isize.get(), 0); // value becomes 0
	/// assert_eq!(clamped_isize.min, -3); // 2 - 5 = -3
	/// assert_eq!(clamped_isize.max, 5); // 10 - 5 = 5
	///
	pub fn from_clamped_usize(clamped_usize: ClampedUsize) -> Self {
		let ClampedUsize { value, min, max, exclusive } = clamped_usize;
		let mut value = value as isize;
		let mut min = min as isize;
		let mut max = max as isize;
		min -= value;
		max -= value;
		value = 0;
		Self { value, min, max, exclusive }
	}
	pub fn set(&mut self, value: isize) {
		let max = if self.exclusive { self.max - 1 } else { self.max };
		self.value = value.clamp(self.min,max);
	}
	pub fn get(&self) -> isize {
		self.value
	}
	pub fn cap(&self) -> isize {
		self.max
	}
	pub fn upper_bound(&self) -> isize {
		if self.exclusive {
			self.max - 1
		} else {
			self.max
		}
	}
	pub fn set_max(&mut self, max: isize) {
		self.max = max;
		self.set(self.value); // Enforces the new maximum
	}
	pub fn set_min(&mut self, min: isize) {
		self.min = min;
		self.set(self.value); // Enforces the new minimum
	}
	pub fn inc(&mut self) -> bool {
		let max = self.upper_bound();
		if self.value == max {
			return false;
		}
		self.add(1);
		true
	}
	pub fn dec(&mut self) -> bool {
		if self.value == self.min {
			return false;
		}
		self.sub(1);
		true
	}
	pub fn add(&mut self, value: isize) {
		let max = self.upper_bound();
		self.value = (self.value + value).clamp(self.min,max)
	}
	pub fn sub(&mut self, value: isize) {
		self.value = (self.value - value).clamp(self.min,self.max)
	}
	pub fn ret_add(&self, value: isize) -> isize {
		let max = self.upper_bound();
		(self.value + value).clamp(self.min,max)
	}
	pub fn ret_sub(&self, value: isize) -> isize {
		(self.value - value).clamp(self.min,self.max)
	}
}

/// The central buffer and state manager for `vicut`'s editing logic.
///
/// `LineBuf` operates entirely on **grapheme clusters** (not `char`s or byte offsets),
/// allowing it to handle complex Unicode text safely and predictably.
///
/// ### Internals
/// - `buffer`: The raw text.
/// - `grapheme_indices`: Cached start byte indices for each grapheme cluster.
///   - Set to `None` when edits occur; lazily recomputed on demand.
/// - `cursor`: Points to a grapheme index in the buffer.
///
/// ### Selections and Motion
/// - `select_mode` and `select_range`: Represent active selections.
/// - `last_selection`: Stores the most recent selection span.
/// - `saved_col`: Used for vertical motion and visual alignment.
///
/// ### Command History
/// - `last_pattern_search`: Most recent `/pattern` used.
/// - `last_substitution`: Stores the last `:s` command and flags.
/// - `last_global`: Stores the last global command (`:g`, `:v`, etc).
///
/// ### Insert Mode
/// - `insert_mode_start_pos`: Marks where insert mode began (for `.`, undo).
///
/// ### Undo/Redo
/// - `undo_stack` / `redo_stack`: Hold `Edit` entries representing mutations.
///
/// ### Notes
/// Slicing, motion, and indexing are always performed using grapheme indices,
/// with utility methods handling conversion to/from byte offsets internally.
/// This design ensures high-level methods remain boundary-safe and Unicode-aware.
#[derive(Default,Clone,Debug)]
pub struct LineBuf {
	pub buffer: String,
	pub grapheme_indices: Option<Vec<usize>>, // Used to slice the buffer
	pub cursor: ClampedUsize, // Used to index grapheme_indices

	pub select_mode: Option<SelectMode>,
	pub select_range: Option<SelectRange>,

	pub last_selection: Option<SelectRange>,
	pub last_pattern_search: Option<Regex>,
	pub last_substitution: Option<(Regex,String,SubFlags)>,
	pub last_global: Option<Verb>,

	pub insert_mode_start_pos: Option<usize>,
	pub inserting_from_visual: bool,
	pub saved_col: Option<usize>,

	pub undo_stack: Vec<Edit>,
	pub redo_stack: Vec<Edit>,
}

impl LineBuf {
	pub fn new() -> Self {
		Self::default()
	}
	/// Only update self.grapheme_indices if it is None
	///
	/// self.grapheme_indices is set to None when it is invalidated by changes to the buffer.
	pub fn update_graphemes_lazy(&mut self) {
		if self.grapheme_indices.is_none() {
			self.update_graphemes();
		}
	}
	/// Set the initial state of the editor
	pub fn with_initial(mut self, buffer: String, cursor: usize) -> Self {
		self.buffer = buffer;
		self.update_graphemes();
		self.cursor = ClampedUsize::new(cursor, self.grapheme_indices().len(), self.cursor.exclusive);
		self
	}
	pub fn take_buf(&mut self) -> String {
		std::mem::take(&mut self.buffer)
	}
	pub fn set_cursor_clamp(&mut self, yn: bool) {
		self.cursor.exclusive = yn;
	}
	pub fn read_cursor_byte_pos(&self) -> usize {
		self.read_idx_byte_pos(self.cursor.get())
	}
	pub fn cursor_byte_pos(&mut self) -> usize {
		self.index_byte_pos(self.cursor.get())
	}
	pub fn find_index_for_byte_pos(&self, index: usize) -> Option<usize> {
		self.grapheme_indices().iter().find(|idx| **idx == index).copied()
	}
	pub fn index_byte_pos(&mut self, index: usize) -> usize {
		self.update_graphemes_lazy();
		self.grapheme_indices()
			.get(index)
			.copied()
			.unwrap_or(self.buffer.len())
	}
	pub fn read_idx_byte_pos(&self, index: usize) -> usize {
		self.grapheme_indices()
			.get(index)
			.copied()
			.unwrap_or(self.buffer.len())
	}
	/// Update self.grapheme_indices with the indices of the current buffer
	///
	/// Be careful with this. Code paths with any amount of regular traffic should use update_graphemes_lazy instead
	/// Slicing the buffer with grapheme_indices(true) is surprisingly expensive
	pub fn update_graphemes(&mut self) {
		let indices: Vec<_> = self.buffer
			.grapheme_indices(true)
			.map(|(i,_)| i)
			.collect();
		self.cursor.set_max(indices.len());
		self.grapheme_indices = Some(indices)
	}
	pub fn grapheme_indices(&self) -> &[usize] {
		self.grapheme_indices.as_ref().unwrap()
	}
	pub fn grapheme_indices_owned(&self) -> Vec<usize> {
		self.grapheme_indices.as_ref().cloned().unwrap_or_default()
	}
	pub fn grapheme_is_escaped(&mut self, pos: usize) -> bool {
		let mut pos = ClampedUsize::new(pos, self.cursor.max, false);
		let mut escaped = false;

		while pos.dec() {
			let Some(gr) = self.grapheme_at(pos.get()) else { return escaped };
			if gr == "\\" {
				escaped = !escaped;
			} else {
				return escaped
			}
		}

		escaped
	}
	/// Does not update graphemes
	/// Useful in cases where you have to check many graphemes at once
	/// And don't want to trigger any mutable borrowing issues
	pub fn read_grapheme_at(&self, pos: usize) -> Option<&str> {
		let indices = self.grapheme_indices();
		let start = indices.get(pos).copied()?;
		let end = indices.get(pos + 1).copied().or_else(|| {
			if pos + 1 == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(start..end)
	}
	pub fn grapheme_at(&mut self, pos: usize) -> Option<&str> {
		self.update_graphemes_lazy();
		let indices = &self.grapheme_indices.as_ref().unwrap();
		if pos + 1 > indices.len() { return None }
		let start = indices[pos];
		let end = if pos+1 == indices.len() { self.buffer.len() } else { indices[pos+1] };
		self.buffer.get(start..end)
	}
	pub fn read_grapheme_before(&self, pos: usize) -> Option<&str> {
		if pos == 0 {
			return None
		}
		let pos = ClampedUsize::new(pos, self.cursor.max, false);
		self.read_grapheme_at(pos.ret_sub(1))
	}
	pub fn grapheme_before(&mut self, pos: usize) -> Option<&str> {
		if pos == 0 {
			return None
		}
		let pos = ClampedUsize::new(pos, self.cursor.max, false);
		self.grapheme_at(pos.ret_sub(1))
	}
	pub fn read_grapheme_after(&self, pos: usize) -> Option<&str> {
		if pos == self.cursor.max {
			return None
		}
		let pos = ClampedUsize::new(pos, self.cursor.max, false);
		self.read_grapheme_at(pos.ret_add(1))
	}
	pub fn grapheme_after(&mut self, pos: usize) -> Option<&str> {
		if pos == self.cursor.max {
			return None
		}
		let pos = ClampedUsize::new(pos, self.cursor.max, false);
		self.grapheme_at(pos.ret_add(1))
	}
	pub fn grapheme_at_cursor(&mut self) -> Option<&str> {
		self.grapheme_at(self.cursor.get())
	}
	pub fn grapheme_before_cursor(&mut self) -> Option<&str> {
		self.grapheme_before(self.cursor.get())
	}
	pub fn grapheme_after_cursor(&mut self) -> Option<&str> {
		self.grapheme_after(self.cursor.get())
	}
	pub fn mark_insert_mode_start_pos(&mut self) {
		self.insert_mode_start_pos = Some(self.cursor.get())
	}
	pub fn clear_insert_mode_start_pos(&mut self) {
		self.insert_mode_start_pos = None
	}
	pub fn slice(&mut self, range: Range<usize>) -> Option<&str> {
		self.update_graphemes_lazy();
		let start_index = self.grapheme_indices().get(range.start).copied()?;
		let end_index = self.grapheme_indices().get(range.end).copied().or_else(|| {
			if range.end == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(start_index..end_index)
	}
	pub fn slice_inclusive(&mut self, range: RangeInclusive<usize>) -> Option<&str> {
		self.update_graphemes_lazy();
		let start_index = self.grapheme_indices().get(*range.start()).copied()?;
		let end_index = self.grapheme_indices().get(*range.end()).copied().or_else(|| {
			if *range.end() == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(start_index..end_index)
	}
	pub fn read_slice_to(&self, end: usize) -> Option<&str> {
		let grapheme_index = self.grapheme_indices().get(end).copied().or_else(|| {
			if end == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(..grapheme_index)
	}
	pub fn slice_to(&mut self, end: usize) -> Option<&str> {
		self.update_graphemes_lazy();
		let grapheme_index = self.grapheme_indices().get(end).copied().or_else(|| {
			if end == self.grapheme_indices().len() {
				Some(self.buffer.len())
			} else {
				None
			}
		})?;
		self.buffer.get(..grapheme_index)
	}
	pub fn slice_from(&mut self, start: usize) -> Option<&str> {
		self.update_graphemes_lazy();
		let grapheme_index = *self.grapheme_indices().get(start)?;
		self.buffer.get(grapheme_index..)
	}
	pub fn read_slice_to_cursor(&self) -> Option<&str> {
		self.read_slice_to(self.cursor.get())
	}
	pub fn slice_to_cursor(&mut self) -> Option<&str> {
		self.slice_to(self.cursor.get())
	}
	pub fn slice_to_cursor_inclusive(&mut self) -> Option<&str> {
		self.slice_to(self.cursor.ret_add(1))
	}
	pub fn slice_from_cursor(&mut self) -> Option<&str> {
		self.slice_from(self.cursor.get())
	}
	pub fn remove(&mut self, pos: usize) {
		let idx = self.index_byte_pos(pos);
		self.buffer.remove(idx);
		self.update_graphemes();
	}
	pub fn drain(&mut self, start: usize, end: usize) -> String {
		let drained = if end == self.grapheme_indices().len() {
			if start == self.grapheme_indices().len() {
				return String::new()
			}
			let start = self.grapheme_indices()[start];
			self.buffer.drain(start..).collect()
		} else {
			let start = self.grapheme_indices()[start];
			let end = self.grapheme_indices()[end];
			self.buffer.drain(start..end).collect()
		};
		self.update_graphemes();
		drained
	}
	pub fn push(&mut self, ch: char) {
		self.buffer.push(ch);
		self.update_graphemes();
	}
	pub fn push_str(&mut self, slice: &str) {
		self.buffer.push_str(slice);
		self.update_graphemes();
	}
	pub fn insert_at_cursor(&mut self, ch: char) {
		self.insert_at(self.cursor.get(), ch);
	}
	pub fn insert_at(&mut self, pos: usize, ch: char) {
		let pos = self.index_byte_pos(pos);
		self.buffer.insert(pos, ch);
		self.update_graphemes();
	}
	pub fn set_buffer_lazy(&mut self, buffer: String) {
		if buffer != self.buffer {
			self.buffer = buffer;
			// Here we set it to none
			// The methods which access grapheme_indices will update it if it is None
			// so this way, we only update it if we really need to
			self.grapheme_indices = None;
		}
	}
	pub fn set_buffer(&mut self, buffer: String) {
		self.buffer = buffer;
		self.update_graphemes();
	}
	pub fn select_range(&self) -> Option<&SelectRange> {
		self.select_range.as_ref()
	}
	pub fn selected_lines(&mut self) -> Option<(usize,usize)> {
		let range = self.select_range()?;
		match range {
			SelectRange::OneDim((start,end)) => {
				let start_ln = self.index_line_number(*start) + 1;
				let end_ln = self.index_line_number(*end) + 1;
				Some((start_ln,end_ln))
			},
			SelectRange::TwoDim(lines) => {
				let start_ln = self.index_line_number(lines.first()?.0) + 1;
				let end_ln = self.index_line_number(lines.last()?.1) + 1;
				Some((start_ln,end_ln))
			}
		}
	}
	pub fn get_block_select(&mut self) -> SelectMode {
		let anchor = SelectAnchor::Start;
		let anchor_pos = self.cursor.get();
		SelectMode::Block {
			anchor,
			anchor_pos,
		}
	}
	pub fn line_col_offset(&mut self, pos: usize) -> (isize,isize) {
		let cursor_col = self.cursor_col();
		let cursor_line = self.cursor_line_number();
		let pos_col = self.index_col(pos);
		let pos_line = self.index_line_number(pos);

		let col_offset = cursor_col as isize - pos_col as isize;
		let line_offset = pos_line as isize - cursor_line as isize;
		(line_offset, col_offset)
	}
	pub fn get_block_select_windows(&mut self, mode: &SelectMode) -> Vec<(usize,usize)> {
		let SelectMode::Block { anchor: _, anchor_pos } = mode else { unreachable!() };
		let mut anchor_pos = *anchor_pos;
		let mut cursor_pos = self.cursor.get();
		let cursor_col = self.index_col(cursor_pos);
		let anchor_col = self.index_col(anchor_pos);

		// horizontal end of the selection must be incremented
		if cursor_col >= anchor_col {
			cursor_pos += 1;
		} else {
			anchor_pos += 1;
		}
		let cursor_col = self.index_col(cursor_pos);
		let anchor_col = self.index_col(anchor_pos);

		let (line_offset, _) = {
			let cursor_line = self.cursor_line_number();
			let anchor_line = self.index_line_number(anchor_pos);

			let col_offset = cursor_col as isize - anchor_col as isize;
			let line_offset = anchor_line as isize - cursor_line as isize;
			(line_offset, col_offset)
		};



		let anchor_col = self.index_col(anchor_pos);
		let cursor_line = self.cursor_line_number();
		let (start,end) = ordered(cursor_line, cursor_line.saturating_add_signed(line_offset));

		let line_range = start..=end;

		let mut windows = vec![];

		for line in line_range {
			let Some((start,end)) = self.line_bounds(line) else { continue };
			let exclusive = end != self.cursor.max;
			let clamped_start = ClampedUsize::new(start, end, exclusive).with_min(start);
			let pos1 = clamped_start.ret_add(anchor_col);
			let pos2 = clamped_start.ret_add(cursor_col);

			let (start,end) = ordered(pos1, pos2);

			windows.push((start,end));
		}

		windows
	}
	pub fn start_selecting(&mut self, mode: SelectMode) {

		self.select_mode = Some(mode);
		let range = match mode {
			SelectMode::Char(_) => SelectRange::OneDim((self.cursor.get(),self.cursor.ret_add(1))),
			SelectMode::Line(_) => SelectRange::OneDim(self.this_line()),
			SelectMode::Block {..} => SelectRange::TwoDim(self.get_block_select_windows(&mode))
		};
		self.select_range = Some(range);
	}
	pub fn stop_selecting(&mut self) {
		self.select_mode = None;
		if self.select_range.is_some() {
			self.last_selection = self.select_range.take();
		}
	}
	pub fn is_selecting(&self) -> bool {
		self.select_mode.is_some() && self.select_range.is_some()
	}
	pub fn selected_content(&mut self) -> Option<String> {
		let range = self.select_range()?.clone();
		match range {
			SelectRange::OneDim((start,end)) => {
				match self.select_mode.as_ref().unwrap() {
					SelectMode::Char(_) => {
						let slice = self.slice_inclusive(start..=end + 1)?;
						Some(slice.to_string())
					}
					SelectMode::Line(_) => {
						let slice = self.slice_inclusive(start..=end)?;
						Some(slice.to_string())
					}
					_ => unreachable!()
				}
			},
			SelectRange::TwoDim(lines) => {
				let mut content = vec![];
				for (start,end) in lines {
					if let Some(slice) = self.slice(start..end) {
						content.push(slice.to_string());
					}
				}
				Some(content.join("\n"))
			}
		}
	}
	pub fn total_lines(&self) -> usize {
		self.buffer
			.chars()
			.filter(|ch| *ch == '\n')
			.count() + 1
	}
	pub fn cursor_line_number(&mut self) -> usize {
		self.slice_to_cursor()
			.map(|slice| {
				slice.chars()
					.filter(|ch| *ch == '\n')
					.count()
			}).unwrap_or(0)
	}
	pub fn byte_pos_line_number(&self, pos: usize) -> usize {
		self.buffer.get(..pos)
			.map(|slice| {
				slice.chars()
					.filter(|ch| *ch == '\n')
					.count()
			}).unwrap_or(0)
	}
	pub fn index_line_number(&self, pos: usize) -> usize {
		self.grapheme_indices().get(..pos)
			.map(|slice| {
				slice
					.iter()
					.filter(|idx| self.read_grapheme_at(**idx) == Some("\n"))
					.count()
			}).unwrap_or(0)
	}
	pub fn is_sentence_punctuation(&self, pos: usize) -> bool {
		self.next_sentence_start_from_punctuation(pos).is_some()
	}
	#[allow(clippy::collapsible_if)] // chaotic evil
	pub fn next_sentence_start_from_punctuation(&self, pos: usize) -> Option<usize> {
		if let Some(gr) = self.read_grapheme_at(pos) {
			if PUNCTUATION.contains(&gr) && self.read_grapheme_after(pos).is_some() {
				let mut fwd_indices = (pos + 1..self.cursor.max).peekable();
				if self.read_grapheme_after(pos).is_some_and(|gr| [")","]","\"","'"].contains(&gr)) {
					while let Some(idx) = fwd_indices.peek() {
						if self.read_grapheme_at(*idx).is_some_and(|gr| [")","]","\"","'"].contains(&gr)) {
							fwd_indices.next();
						} else {
							break
						}
					}
				}
				if let Some(idx) = fwd_indices.next() {
					if let Some(gr) = self.read_grapheme_at(idx) {
						if is_whitespace(gr) {
							if gr == "\n" {
								return Some(idx)
							}
							while let Some(idx) = fwd_indices.next() {
								if let Some(gr) = self.read_grapheme_at(idx) {
									if is_whitespace(gr) {
										if gr == "\n" {
											return Some(idx)
										}
										continue
									} else {
										return Some(idx)
									} // Oh look, a slide
								} // Weeee
							} // eeee
						} // eeee
					} // eeee
				} // eeee
			} // eeee
		} // eee.
		None
	}

	pub fn is_paragraph_start(&mut self, pos: usize) -> bool {
		self.grapheme_at(pos) == Some("\n") && self.grapheme_before(pos) == Some("\n")
	}
	pub fn is_sentence_start(&mut self, pos: usize) -> bool {
		if self.grapheme_before(pos).is_some_and(is_whitespace) {
			let pos = pos.saturating_sub(1);
			let mut bkwd_indices = (0..pos).rev().peekable();
			while let Some(idx) = bkwd_indices.next() {
				let Some(gr) = self.read_grapheme_at(idx) else { break };
				if [")","]","\"","'"].contains(&gr) {
					while let Some(idx) = bkwd_indices.peek() {
						let Some(gr) = self.read_grapheme_at(*idx) else { break };
						if [")","]","\"","'"].contains(&gr) {
							bkwd_indices.next();
						} else {
							break
						}
					}
				}
				if !is_whitespace(gr)  {
					if [".","?","!"].contains(&gr) {
						return true
					} else {
						break
					}
				}
			}
		}
		false
	}
	pub fn nth_next_line(&mut self, n: usize) -> Option<(usize,usize)> {
		let line_no = self.cursor_line_number() + n;
		if line_no >= self.total_lines() {
			return None
		}
		self.line_bounds(line_no)
	}
	pub fn nth_prev_line(&mut self, n: usize) -> Option<(usize,usize)> {
		let cursor_line_no = self.cursor_line_number();
		if cursor_line_no == 0 {
			return None
		}
		let line_no = cursor_line_no.saturating_sub(n);
		if line_no >= self.total_lines() {
			return None
		}
		self.line_bounds(line_no)
	}
	pub fn this_line(&mut self) -> (usize,usize) {
		let line_no = self.cursor_line_number();
		self.line_bounds(line_no).unwrap()
	}
	pub fn start_of_line(&mut self) -> usize {
		self.this_line().0
	}
	pub fn end_of_line(&mut self) -> usize {
		self.this_line().1
	}
	pub fn select_lines_up(&mut self, n: usize) -> Option<(usize,usize)> {
		if self.start_of_line() == 0 {
			return None
		}
		let target_line = self.cursor_line_number().saturating_sub(n);
		let end = self.end_of_line();
		let (start,_) = self.line_bounds(target_line)?;

		Some((start,end))
	}
	pub fn select_lines_down(&mut self, n: usize) -> Option<(usize,usize)> {
		if self.end_of_line() == self.cursor.max {
			return None
		}
		let target_line = self.cursor_line_number() + n;
		let start = self.start_of_line();
		let (_,end) = self.line_bounds(target_line)?;

		Some((start,end))
	}
	pub fn line_bounds(&self, n: usize) -> Option<(usize,usize)> {
		if n > self.total_lines() {
			return None
		}

		let mut start = 0;
		let mut idx_iter = 0..self.cursor.max;

		// Fine the start of the line
		for _ in 0..n {
			while let Some(idx) = idx_iter.next() {
				let gr = self.read_grapheme_at(idx).unwrap();
				if gr == "\n" {
					start = (idx + 1).min(self.cursor.max);
					break
				}
			}
		}

		let mut end = start;
		let mut found_newline = false;
		// Find the end of the line
		while let Some(idx) = idx_iter.next() {
			end = (end + 1).min(self.cursor.max);
			let gr = self.read_grapheme_at(idx).unwrap();
			if gr == "\n" {
				found_newline = true;
				break
			}
		}

		if !found_newline {
			end = self.cursor.max;
		}

		Some((start, end))
	}
	/// Compare the old and new buffers, and update the undo stack
	///
	/// This function is called whenever the buffer is edited.
	/// It compares the old and new buffers, and updates the undo stack accordingly.
	///
	/// If the edit is merging, it will try to merge the edit into the last edit in the undo stack.
	/// If the edit is not merging, it will push the edit to the undo stack.
	pub fn handle_edit(&mut self, old: String, new: String, curs_pos: usize) {
		let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);
		if edit_is_merging {
			let diff = Edit::diff(&old, &new, curs_pos);
			if diff.is_empty() {
				return
			}
			let Some(mut edit) = self.undo_stack.pop() else {
				self.undo_stack.push(diff);
				return
			};



			let mut merge_pos = edit.merge_pos;
			let diff_len = diff.new_diff.len();
			edit.new = diff.new;
			edit.new_diff.insert_str(merge_pos, &diff.new_diff);
			merge_pos += diff_len;
			edit.merge_pos = merge_pos;

			self.undo_stack.push(edit);
		} else {
			let diff = Edit::diff(&old, &new, curs_pos);
			if !diff.is_empty() {
				self.undo_stack.push(diff);
			}
		}
	}

	/// Check if a character is a word boundary
	pub fn is_word_bound(&mut self, pos: usize, word: Word, dir: Direction) -> bool {
		let clamped_pos = ClampedUsize::new(pos, self.cursor.max, true);
		let Some(cur_char) = self.grapheme_at(clamped_pos.get()).map(|c| c.to_string()) else { return false };
		let other_pos = match dir {
			Direction::Forward => clamped_pos.ret_add(1),
			Direction::Backward => clamped_pos.ret_sub(1)
		};
		if other_pos == clamped_pos.get() { return true }

		let other_char = self.grapheme_at(other_pos).unwrap();
		match word {
			Word::Big => is_whitespace(other_char),
			Word::Normal => is_other_class_or_is_ws(other_char, &cur_char)
		}
	}
	pub fn dispatch_text_obj(
		&mut self,
		count: usize,
		text_obj: TextObj
	) -> Option<(usize,usize)> {
		match text_obj {
			// Text groups
			TextObj::Word(word,bound) => self.text_obj_word(count, bound, word),
			TextObj::Sentence(dir) => {
				let (start,end) = self.text_obj_sentence(self.cursor.get(), count, Bound::Around)?;
				let cursor = self.cursor.get();
				match dir {
					Direction::Forward => Some((cursor,end)),
					Direction::Backward => Some((start,cursor)),
				}
			}
			TextObj::Paragraph(dir) => {
				let (start,end) = self.text_obj_paragraph(self.cursor.get(), count, Bound::Around)?;
				let cursor = self.cursor.get();
				match dir {
					Direction::Forward => Some((cursor,end)),
					Direction::Backward => Some((start,cursor)),
				}
			}
			TextObj::WholeSentence(bound) => self.text_obj_sentence(self.cursor.get(), count, bound),
			TextObj::WholeParagraph(bound) => self.text_obj_paragraph(self.cursor.get(), count, bound),

			// Quoted blocks
			TextObj::DoubleQuote(bound) |
				TextObj::SingleQuote(bound) |
				TextObj::BacktickQuote(bound) => self.text_obj_quote(count, text_obj, bound),

				// Delimited blocks
				TextObj::Paren(bound) |
					TextObj::Bracket(bound) |
					TextObj::Brace(bound) |
					TextObj::Angle(bound) => self.text_obj_delim(count, text_obj, bound),

					// Other stuff
				TextObj::Tag(bound) => todo!(),
				TextObj::Custom(_) => todo!(),
		}
	}
	pub fn text_obj_word(&mut self, count: usize, bound: Bound, word: Word) -> Option<(usize,usize)> {
		match bound {
			Bound::Inside => {
				let start = if self.is_word_bound(self.cursor.get(), word, Direction::Backward) {
					self.cursor.get()
				} else {
					self.start_of_word_backward(self.cursor.get(), word)
				};
				let end = if self.is_word_bound(self.cursor.get(), word, Direction::Forward) {
					self.cursor.get()
				} else {
					self.end_of_word_forward(self.cursor.get(), word)
				};
				Some((start,end))
			}
			Bound::Around => {
				let start = if self.is_word_bound(self.cursor.get(), word, Direction::Backward) {
					self.cursor.get()
				} else {
					self.start_of_word_backward(self.cursor.get(), word)
				};
				let end = if self.is_word_bound(self.cursor.get(), word, Direction::Forward) {
					self.cursor.get()
				} else {
					self.end_of_word_forward(self.cursor.get(), word)
				};
				Some((start,end))
			}
		}
	}
	/// Get the span of the current `sentence`
	///
	/// A sentence is defined as a "sequence of characters with punctuation at the end, followed by any number of closing delimiters, followed by whitespace, which is itself followed by non-whitespace"
	/// Thanks vim!
	pub fn text_obj_sentence(&mut self, start_pos: usize, count: usize, bound: Bound) -> Option<(usize, usize)> {
		let mut start = None;
		let mut end = None;
		let mut fwd_indices = (start_pos..self.cursor.max).peekable();
		while let Some(idx) = fwd_indices.next() {
			if self.grapheme_at(idx).is_none() { break }

			if let Some(next_sentence_start) = self.next_sentence_start_from_punctuation(idx) {
				match bound {
					Bound::Inside => {
						end = Some(idx);
						break
					}
					Bound::Around => {
						end = Some(next_sentence_start);
						break
					}
				}
			}
		}
		let mut end = end.unwrap_or(self.cursor.max);

		let mut bkwd_indices = (0..end).rev();
		while let Some(idx) = bkwd_indices.next() {
			if self.is_sentence_start(idx) {
				start = Some(idx);
				break
			}
		}
		let start = start.unwrap_or(0);

		if count > 1 {
				if let Some((_,new_end)) = self.text_obj_sentence(end, count - 1, bound) {
			end = new_end;
			}
		}

		Some((start,end))
	}
	/// Get the span of the current `paragraph`
	///
	/// A paragraph is a block of text delimited by empty lines.
	pub fn text_obj_paragraph(&mut self, start_pos: usize, count: usize, bound: Bound) -> Option<(usize, usize)> {
		// FIXME: This is a pretty naive approach
		let mut start = None;
		let mut end = None;
		let mut fwd_indices = (start_pos..self.cursor.max).peekable();

		while let Some(idx) = fwd_indices.next() {
			let Some("\n") = self.grapheme_at(idx) else {
				continue
			};
			let Some(next_idx) = fwd_indices.peek() else { break };
			if let Some("\n") = self.grapheme_at(*next_idx) {
				match bound {
					Bound::Inside => end = Some(*next_idx),
					Bound::Around => {
						fwd_indices.next();
						while let Some(idx) = fwd_indices.next() {
							match self.grapheme_at(idx) {
								Some("\n") => continue,
								_ => {
									end = Some(idx);
								}
							}
						}
					}
				}

				break
			}
		}
		let mut end = end.unwrap_or(self.cursor.max);

		let mut bkwd_indices = (0..end).rev().peekable();
		while let Some(idx) = bkwd_indices.next() {
			let Some("\n") = self.grapheme_at(idx) else {
				continue
			};
			let Some(next_idx) = bkwd_indices.peek() else { break };
			if let Some("\n") = self.grapheme_at(*next_idx) {
				start = Some(idx);
				break
			}
		}
		let start = start.unwrap_or(0);

		if count > 1 {
				if let Some((_,new_end)) = self.text_obj_sentence(end, count - 1, bound) {
			end = new_end;
			}
		}
		Some((start,end))
	}
	/// Get the span of the next delimited block in this line
	pub fn text_obj_delim(&mut self, count: usize, text_obj: TextObj, bound: Bound) -> Option<(usize,usize)> {
		let mut backward_indices = (0..self.cursor.get()).rev();
		let (opener,closer) = match text_obj {
			TextObj::Paren(_)   => ("(",")"),
			TextObj::Bracket(_) => ("[","]"),
			TextObj::Brace(_)   => ("{","}"),
			TextObj::Angle(_)   => ("<",">"),
			_ => unreachable!()
		};

		let mut start_pos = None;
		let mut closer_count: u32 = 0;
		while let Some(idx) = backward_indices.next() {
			let gr = self.grapheme_at(idx)?.to_string();
			if (gr != closer && gr != opener) || self.grapheme_is_escaped(idx) { continue }

			if gr == closer {
				closer_count += 1;
			} else if closer_count == 0 {
				start_pos = Some(idx);
				break
			} else {
				closer_count = closer_count.saturating_sub(1)
			}
		}

		let (mut start, mut end) = if let Some(pos) = start_pos {
			let start = pos;
			let mut forward_indices = start+1..self.cursor.max;
			let mut end = None;
			let mut opener_count: u32 = 0;

			while let Some(idx) = forward_indices.next() {
				if self.grapheme_is_escaped(idx) { continue }
				match self.grapheme_at(idx)? {
					gr if gr == opener => opener_count += 1,
					gr if gr == closer => {
						if opener_count == 0 {
							end = Some(idx);
							break
						} else {
							opener_count = opener_count.saturating_sub(1);
						}
					}
					_ => { /* Continue */ }
				}
			}

			(start,end?)
		} else {
			let mut forward_indices = self.cursor.get()..self.cursor.max;
			let mut start = None;
			let mut end = None;
			let mut opener_count: u32 = 0;

			while let Some(idx) = forward_indices.next() {
				if self.grapheme_is_escaped(idx) { continue }
				match self.grapheme_at(idx)? {
					gr if gr == opener => {
						if opener_count == 0 {
							start = Some(idx);
						}
						opener_count += 1;
					}
					gr if gr == closer => {
						if opener_count == 1 {
							end = Some(idx);
							break
						} else {
							opener_count = opener_count.saturating_sub(1)
						}
					}
					_ => { /* Continue */ }
				}
			}

			(start?,end?)
		};

		match bound {
			Bound::Inside => {
				// Start includes the quote, so push it forward
				start += 1;
			}
			Bound::Around => {
				// End excludes the quote, so push it forward
				end += 1;

				// We also need to include any trailing whitespace
				let end_of_line = self.end_of_line();
				let remainder = end..end_of_line;
				for idx in remainder {
					let Some(gr) = self.grapheme_at(idx) else { break };
					if is_whitespace(gr) {
						end += 1;
					} else {
						break
					}
				}
			}
		}

		Some((start,end))
	}
	pub fn text_obj_quote(&mut self, count: usize, text_obj: TextObj, bound: Bound) -> Option<(usize,usize)> {
		let (start,end) = self.this_line(); // Only operates on the current line

		// Get the grapheme indices backward from the cursor
		let mut backward_indices = (start..self.cursor.get()).rev();
		let target = match text_obj {
			TextObj::DoubleQuote(_) => "\"",
			TextObj::SingleQuote(_) => "'",
			TextObj::BacktickQuote(_) => "`",
			_ => unreachable!()
		};
		let mut start_pos = None;
		while let Some(idx) = backward_indices.next() {
			match self.grapheme_at(idx)? {
				gr if gr == target => {
					// We are going backwards, so we need to handle escapes differently
					// These things were not meant to be read backwards, so it's a little fucked up
					let mut escaped = false;
					while let Some(idx) = backward_indices.next() {
						// Keep consuming indices as long as they refer to a backslash
						let Some("\\") = self.grapheme_at(idx) else {
							break
						};
						// On each backslash, flip this boolean
						escaped = !escaped
					}

					// If there are an even number of backslashes, we are not escaped
					// Therefore, we have found the start position
					if !escaped {
						start_pos = Some(idx);
						break
					}
				}
				_ => { /* Continue */ }
			}
		}

		// Try to find a quote backwards
		let (mut start, mut end) = if let Some(pos) = start_pos {
			// Found one, only one more to go
			let start = pos;
			let mut forward_indices = start+1..end;
			let mut end = None;

			while let Some(idx) = forward_indices.next() {
				match self.grapheme_at(idx)? {
					"\\" => { forward_indices.next(); }
					gr if gr == target => {
						end = Some(idx);
						break;
					}
					_ => { /* Continue */ }
				}
			}
			let end = end?;

			(start,end)
		} else {
			// Did not find one, have two find two of them forward now
			let mut forward_indices = self.cursor.get()..end;
			let mut start = None;
			let mut end = None;

			while let Some(idx) = forward_indices.next() {
				match self.grapheme_at(idx)? {
					"\\" => { forward_indices.next(); }
					gr if gr == target => {
						start = Some(idx);
						break
					}
					_ => { /* Continue */ }
				}
			}
			let start = start?;

			while let Some(idx) = forward_indices.next() {
				match self.grapheme_at(idx)? {
					"\\" => { forward_indices.next(); }
					gr if gr == target => {
						end = Some(idx);
						break;
					}
					_ => { /* Continue */ }
				}
			}
			let end = end?;

			(start,end)
		};

		match bound {
			Bound::Inside => {
				// Start includes the quote, so push it forward
				start += 1;
			}
			Bound::Around => {
				// End excludes the quote, so push it forward
				end += 1;

				// We also need to include any trailing whitespace
				let end_of_line = self.end_of_line();
				let remainder = end..end_of_line;
				for idx in remainder {
					let Some(gr) = self.grapheme_at(idx) else { break };
					if is_whitespace(gr) {
						end += 1;
					} else {
						break
					}
				}
			}
		}

		Some((start, end))
	}
	pub fn find_next_matching_delim(&mut self) -> Option<usize> {
		let (start,end) = self.this_line();
		let opener_delims = [
			"[",
			"{",
			"(",
			"<",
		];
		let delims = [
			"[", "]",
			"{", "}",
			"(", ")",
			"<", ">",
		];
		let mut fwd_indices = self.cursor.get()..end;
		let mut bkwd_indices = (start..self.cursor.get()).rev();
		let idx = fwd_indices.find(|idx| self.grapheme_at(*idx).is_some_and(|gr| delims.contains(&gr)))
			.or_else(|| bkwd_indices.find(|idx| self.grapheme_at(*idx).is_some_and(|gr| opener_delims.contains(&gr))))?;
		let search_direction = match self.grapheme_at(idx)? {
			"[" |
			"{" |
			"(" |
			"<" => Direction::Forward,
			"]" |
			"}" |
			")" |
			">" => Direction::Backward,
			_ => unreachable!()
		};
		// 'target_delim' is the character that will decrement the depth counter
		let target_delim = match self.grapheme_at(idx)? {
			"[" => "]",
			"]" => "[",
			"{" => "}",
			"}" => "{",
			"(" => ")",
			")" => "(",
			"<" => ">",
			">" => "<",
			_ => unreachable!()
		};
		// 'new_delim' is the character that will increment the depth counter
		let new_delim = self.read_grapheme_at(idx)?;
		let mut depth = 0u32;

		match search_direction {
			Direction::Forward => {
				let mut fwd_indices = idx..self.cursor_max();
				while let Some(idx) = fwd_indices.next() {
					let gr = self.read_grapheme_at(idx)?;
					match gr {
						_ if gr == new_delim => depth += 1,
						_ if gr == target_delim => {
							depth = depth.saturating_sub(1);
							if depth == 0 {
								return Some(idx)
							}
						}
						_ => { /* Keep going */ }
					}
				}
				None
			}
			Direction::Backward => {
				let mut bkwd_indices = (0..idx).rev();
				while let Some(idx) = bkwd_indices.next() {
					let gr = self.read_grapheme_at(idx)?;
					match gr {
						_ if gr == new_delim => depth += 1,
						_ if gr == target_delim => {
							depth -= 1;
							if depth == 0 {
								return Some(idx)
							}
						}
						_ => { /* Keep going */ }
					}
				}
				None
			}
		}
	}
	pub fn find_unmatched_delim(&mut self, delim: Delim, dir: Direction) -> Option<usize> {
		let (opener,closer) = match delim {
			Delim::Paren   => ("(",")"),
			Delim::Brace   => ("{","}"),
			Delim::Bracket => ("[","]"),
			Delim::Angle   => ("<",">"),
		};
		match dir {
			Direction::Forward => {
				let mut fwd_indices = self.cursor.get()..self.cursor.max;
				let mut depth = 0;

				while let Some(idx) = fwd_indices.next() {
					if self.grapheme_is_escaped(idx) { continue }
					let gr = self.grapheme_at(idx)?;
					match gr {
						_ if gr == opener => depth += 1,
						_ if gr == closer => {
							if depth == 0 {
								return Some(idx)
							} else {
								depth -= 1;
							}
						}
						_ => { /* Continue */ }
					}
				}

				None
			}
			Direction::Backward => {
				let mut bkwd_indices = (0..self.cursor.get()).rev();
				let mut depth = 0;

				while let Some(idx) = bkwd_indices.next() {
					if self.grapheme_is_escaped(idx) { continue }
					let gr = self.grapheme_at(idx)?;
					match gr {
						_ if gr == closer => depth += 1,
						_ if gr == opener => {
							if depth == 0 {
								return Some(idx)
							} else {
								depth -= 1;
							}
						}
						_ => { /* Continue */ }
					}
				}

				None
			}
		}
	}
	pub fn dispatch_word_motion(
		&mut self,
		count: usize,
		to: To,
		word: Word,
		dir: Direction,
		include_last_char: bool
	) -> usize {
		// Not sorry for these method names btw
		let mut pos = ClampedUsize::new(self.cursor.get(), self.cursor.max, false);
		for i in 0..count {
			// We alter 'include_last_char' to only be true on the last iteration
			// Therefore, '5cw' will find the correct range for the first four and stop on the end of the fifth word
			let include_last_char_and_is_last_word = include_last_char && i == count.saturating_sub(1);
			pos.set(match to {
				To::Start => {
					match dir {
						Direction::Forward => self.start_of_word_forward(pos.get(), word, include_last_char_and_is_last_word),
						Direction::Backward => 'backward: {
							// We also need to handle insert mode's Ctrl+W behaviors here
							let target = self.start_of_word_backward(pos.get(), word);

							// Check to see if we are in insert mode
							let Some(start_pos) = self.insert_mode_start_pos else {
								break 'backward target
							};
							// If we are in front of start_pos, and we would cross start_pos to reach target
							// then stop at start_pos
							if start_pos > target && self.cursor.get() > start_pos {
								return start_pos
							} else {
								// We are behind start_pos, now we just reset it
								if self.cursor.get() < start_pos {
									self.clear_insert_mode_start_pos();
								}
								break 'backward target
							}
						}
					}
				}
				To::End => {
					match dir {
						Direction::Forward => self.end_of_word_forward(pos.get(), word),
						Direction::Backward => self.end_of_word_backward(pos.get(), word, false),
					}
				}
			});
		}
		pos.get()
	}
	pub fn start_of_word_forward(&mut self, mut pos: usize, word: Word, include_last_char: bool) -> usize {
		let default = self.grapheme_indices().len();
		let mut indices_iter = (pos..self.cursor.max).peekable();

		match word {
			Word::Big => {
				let Some(next) = indices_iter.peek() else {
					return default
				};
				let on_boundary = self.grapheme_at(*next).is_none_or(is_whitespace);
				if on_boundary {
					let Some(idx) = indices_iter.next() else { return default };
					// We have a 'cw' call, do not include the trailing whitespace
					if include_last_char {
						return idx;
					} else {
						pos = idx;
					}
				}

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Find the next whitespace
				if !on_whitespace {
					let Some(ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace)) else {
						return default
					};
					if include_last_char {
						return ws_pos
					}
				}

				// Return the next visible grapheme position
				indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))).unwrap_or(default)
			}
			Word::Normal => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let Some(next_idx) = indices_iter.peek() else { return default };
				let on_boundary = !is_whitespace(&cur_char) && self.grapheme_at(*next_idx).is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
				if on_boundary {
					if include_last_char {
						return *next_idx
					} else {
						pos = *next_idx;
					}
				}

				let Some(next_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				if is_other_class_not_ws(&cur_char, &next_char) {
					return pos
				}
				let on_whitespace = is_whitespace(&cur_char);

				// Advance until hitting whitespace or a different character class
				if !on_whitespace {
					let other_class_pos = indices_iter.find(
						|i| {
							self.grapheme_at(*i)
								.is_some_and(|c| is_other_class_or_is_ws(c, &next_char))
						}
					);
					let Some(other_class_pos) = other_class_pos else {
						return default
					};
					// If we hit a different character class, we return here
					if self.grapheme_at(other_class_pos).is_some_and(|c| !is_whitespace(c)) || include_last_char {
						return other_class_pos
					}
				}

				// We are now certainly on a whitespace character. Advance until a non-whitespace character.
				indices_iter.find(
					|i| {
						self.grapheme_at(*i)
							.is_some_and(|c| !is_whitespace(c))
					}
				).unwrap_or(default)
			}
		}
	}

	pub fn end_of_word_backward(&mut self, mut pos: usize, word: Word, include_last_char: bool) -> usize {
		let default = self.grapheme_indices().len();
		let mut indices_iter = (0..pos).rev().peekable();

		match word {
			Word::Big => {
				let Some(next) = indices_iter.peek() else {
					return default
				};
				let on_boundary = self.grapheme_at(*next).is_none_or(is_whitespace);
				if on_boundary {
					let Some(idx) = indices_iter.next() else { return default };
					// We have a 'cw' call, do not include the trailing whitespace
					if include_last_char {
						return idx;
					} else {
						pos = idx;
					}
				}

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Find the next whitespace
				if !on_whitespace {
					let Some(ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace)) else {
						return default
					};
					if include_last_char {
						return ws_pos
					}
				}

				// Return the next visible grapheme position

				indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))).unwrap_or(default)
			}
			Word::Normal => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let Some(next_idx) = indices_iter.peek() else { return default };
				let on_boundary = !is_whitespace(&cur_char) && self.grapheme_at(*next_idx).is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
				if on_boundary {
					if include_last_char {
						return *next_idx
					} else {
						pos = *next_idx;
					}
				}

				let Some(next_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				if is_other_class_not_ws(&cur_char, &next_char) {
					return pos
				}
				let on_whitespace = is_whitespace(&cur_char);

				// Advance until hitting whitespace or a different character class
				if !on_whitespace {
					let other_class_pos = indices_iter.find(
						|i| {
							self.grapheme_at(*i)
								.is_some_and(|c| is_other_class_or_is_ws(c, &next_char))
						}
					);
					let Some(other_class_pos) = other_class_pos else {
						return default
					};
					// If we hit a different character class, we return here
					if self.grapheme_at(other_class_pos).is_some_and(|c| !is_whitespace(c)) || include_last_char {
						return other_class_pos
					}
				}

				// We are now certainly on a whitespace character. Advance until a non-whitespace character.

				indices_iter.find(
					|i| {
						self.grapheme_at(*i)
							.is_some_and(|c| !is_whitespace(c))
					}
				).unwrap_or(default)
			}
		}
	}
	pub fn end_of_word_forward(&mut self, mut pos: usize, word: Word) -> usize {
		let default = self.cursor.max;
		if pos >= default {
			return default
		}
		let mut fwd_indices = (pos + 1..default).peekable();

		match word {
			Word::Big => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let Some(next_idx) = fwd_indices.peek() else { return default };
				let on_boundary = !is_whitespace(&cur_char) && self.grapheme_at(*next_idx).is_none_or(is_whitespace);
				if on_boundary {
					let Some(idx) = fwd_indices.next() else { return default };
					pos = idx;
				}
				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Advance iterator to next visible grapheme
				if on_whitespace {
					let Some(_non_ws_pos) = fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))) else {
						return default
					};
				}

				// The position of the next whitespace will tell us where the end (or start) of the word is
				let Some(next_ws_pos) = fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace)) else {
					return default
				};
				pos = next_ws_pos;

				if pos == self.grapheme_indices().len() {
					// We reached the end of the buffer
					pos
				} else {
					// We hit some whitespace, so we will go back one
					pos.saturating_sub(1)
				}
			}
			Word::Normal => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let Some(next_idx) = fwd_indices.peek() else { return default };
				let on_boundary = !is_whitespace(&cur_char) && self.grapheme_at(*next_idx).is_none_or(|c| is_other_class_or_is_ws(c, &cur_char));
				if on_boundary {
					let next_idx = fwd_indices.next().unwrap();
					pos = next_idx
				}

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Proceed to next visible grapheme
				if on_whitespace {
					let Some(non_ws_pos) = fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))) else {
						return default
					};
					pos = non_ws_pos
				}

				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return self.grapheme_indices().len()
				};
				// The position of the next differing character class will tell us where the start of the word is
				let Some(next_ws_pos) = fwd_indices.find(|i| self.grapheme_at(*i).is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))) else {
					return default
				};
				pos = next_ws_pos;

				if pos == self.grapheme_indices().len() {
					// We reached the end of the buffer
					pos
				} else {
					// We hit some other character class, so we go back one
					pos.saturating_sub(1)
				}
			}
		}
	}
	pub fn start_of_word_backward(&mut self, mut pos: usize, word: Word) -> usize {
		let default = 0;

		let mut indices_iter = (0..pos).rev().peekable();

		match word {
			Word::Big => {
				let on_boundary = 'bound_check: {
					let Some(next_idx) = indices_iter.peek() else { break 'bound_check false };
					self.grapheme_at(*next_idx).is_none_or(is_whitespace)
				};
				if on_boundary {
					let Some(idx) = indices_iter.next() else { return default };
					pos = idx;
				}
				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Advance iterator to next visible grapheme
				if on_whitespace {
					let Some(_non_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))) else {
						return default
					};
				}

				// The position of the next whitespace will tell us where the end (or start) of the word is
				let Some(next_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(is_whitespace)) else {
					return default
				};
				pos = next_ws_pos;

				if pos == self.grapheme_indices().len() {
					// We reached the end of the buffer
					pos
				} else {
					// We hit some whitespace, so we will go back one
					pos + 1
				}
			}
			Word::Normal => {
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else { return default };
				let on_boundary = 'bound_check: {
					let Some(next_idx) = indices_iter.peek() else { break 'bound_check false };
					!is_whitespace(&cur_char) && self.grapheme_at(*next_idx).is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))
				};
				if on_boundary {
					let next_idx = indices_iter.next().unwrap();
					pos = next_idx
				}

				// Check current grapheme
				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return default
				};
				let on_whitespace = is_whitespace(&cur_char);

				// Proceed to next visible grapheme
				if on_whitespace {
					let Some(non_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| !is_whitespace(c))) else {
						return default
					};
					pos = non_ws_pos
				}

				let Some(cur_char) = self.grapheme_at(pos).map(|c| c.to_string()) else {
					return self.grapheme_indices().len()
				};
				// The position of the next differing character class will tell us where the start of the word is
				let Some(next_ws_pos) = indices_iter.find(|i| self.grapheme_at(*i).is_some_and(|c| is_other_class_or_is_ws(c, &cur_char))) else {
					return default
				};
				pos = next_ws_pos;

				if pos == 0 {
					// We reached the start of the buffer
					pos
				} else {
					// We hit some other character class, so we go back one
					pos + 1
				}
			}
		}
	}
	fn grapheme_index_for_display_col(&self, line: &str, target_col: usize) -> usize {
		let mut col = 0;
		for (grapheme_index, g) in line.graphemes(true).enumerate() {
			if g == "\n" {
				if self.cursor.exclusive {
					return grapheme_index.saturating_sub(1)
				} else {
					return grapheme_index;
				}
			}
			let w = g.width();
			if col + w > target_col {
				return grapheme_index;
			}
			col += w;
		}
		// If we reach here, the target_col is past end of line
		line.graphemes(true).count()
	}
	pub fn cursor_max(&self) -> usize {
		self.cursor.max
	}
	pub fn cursor_at_max(&mut self) -> bool {
		// hack
		let cursor_pos = self.cursor.get();
		if self.cursor.exclusive {
			cursor_pos == self.cursor.max.saturating_sub(2)
		} else {
			cursor_pos == self.cursor.max.saturating_sub(1)
		}
	}
	pub fn cursor_at_eol(&mut self) -> bool {
		self.grapheme_after_cursor().is_none_or(|gr| gr == "\n")
	}
	pub fn cursor_col(&mut self) -> usize {
		let start = self.start_of_line();
		let cursor_pos = self.cursor.get();
		cursor_pos - start
	}
	pub fn index_col(&self, pos: usize) -> usize {
		let pos_line = self.index_line_number(pos);
		let (start, _) = self.line_bounds(pos_line).expect("Indexing a line that does not exist");
		// We can be reasonably sure that start is less than pos
		pos - start
	}
	pub fn insert_register_content(&mut self, insert_idx: usize, content: RegisterContent, _anchor: Anchor) {
		let byte_pos = self.index_byte_pos(insert_idx);
		match content {
			RegisterContent::Span(text) => {
				self.buffer.insert_str(byte_pos, &text);
				self.update_graphemes();
			}
			RegisterContent::Line(mut line) => {
				if self.grapheme_before(insert_idx).is_some_and(|gr| gr != "\n") {
					line = format!("\n{}", line);
				}
				self.buffer.insert_str(byte_pos, &line);
				self.update_graphemes();
			}
			RegisterContent::Block(windows) => {
				eprintln!("Inserting block at {}", insert_idx);
				eprintln!("Block: {:?}", windows);
				let col = self.index_col(insert_idx);
				let line = self.index_line_number(insert_idx);

				let windows_iter = windows.iter()
					.rev() // Reverse it once
					.enumerate() // Enumerate it so we can get the line number
					.rev(); // Reverse it again
									// Do not question my methods

				for (i,window) in windows_iter {
					let line = line + i;
					if line > self.total_lines() {
						break
					}
					let (start, end) = self.line_bounds(line).unwrap();
					let insert_idx = start + col;

					if insert_idx >= end {
						let offset = if self.grapheme_before(end).is_some_and(|gr| gr == "\n") {
							end - 1
						} else {
							end
						};
						// We are trying to insert past the end of the line
						// So we have to pad the line with spaces
						// To accomodate the insertion
						let pad = " ".repeat(insert_idx.saturating_sub(offset));
						let window = format!("{}{}", pad, window);
						let insert_idx = if end != self.cursor.max {
							end.saturating_sub(1) // We want to insert before the newline
						} else {
							end // We are at the end of the buffer, so no newline
						};
						let byte_pos = self.index_byte_pos(insert_idx);
						self.buffer.insert_str(byte_pos, &window);
					} else {
						let byte_pos = self.index_byte_pos(insert_idx);
						self.buffer.insert_str(byte_pos, window);
					}

				}
			}
			RegisterContent::Empty => {}
		}
	}
	pub fn insert_str_at(&mut self, pos: usize, new: &str) {
		let idx = self.index_byte_pos(pos);
		self.buffer.insert_str(idx, new);
		self.update_graphemes();
	}
	pub fn replace_range(&mut self, start: usize, end: usize, new: &str) {
		self.update_graphemes_lazy();
		let start_byte_pos = self.grapheme_indices().get(start).copied().unwrap_or(0);
		let end_byte_pos = self.grapheme_indices().get(end).copied().unwrap_or(self.buffer.len());
		self.buffer.replace_range(start_byte_pos..end_byte_pos, new);
	}
	pub fn replace_at_cursor(&mut self, new: &str) {
		self.replace_at(self.cursor.get(), new);
	}
	pub fn force_replace_at(&mut self, pos: usize, new: &str) {
		let Some(gr) = self.grapheme_at(pos).map(|gr| gr.to_string()) else {
			self.buffer.push_str(new);
			return
		};
		let start = self.index_byte_pos(pos);
		let end = start + gr.len();
		self.buffer.replace_range(start..end, new);
	}
	pub fn replace_at(&mut self, pos: usize, new: &str) {
		let Some(gr) = self.grapheme_at(pos).map(|gr| gr.to_string()) else {
			self.buffer.push_str(new);
			return
		};
		if &gr == "\n" {
			// Do not replace the newline, push it forward instead
			let byte_pos = self.index_byte_pos(pos);
			self.buffer.insert_str(byte_pos, new);
			return
		}
		let start = self.index_byte_pos(pos);
		let end = start + gr.len();
		self.buffer.replace_range(start..end, new);
	}
	pub fn eval_line_addr(&mut self, addr: LineAddr) -> Option<usize> {
		match addr {
			LineAddr::Number(num) => Some(num.saturating_sub(1)), // Line ranges are one indexed for input, zero indexed internally
																														// Both zero and one refer to the first line
			LineAddr::Current => Some(self.cursor_line_number()),
			LineAddr::Last => Some(self.total_lines()),
			LineAddr::Offset(offset) => {
				let current = self.cursor_line_number();
				Some(current.saturating_add_signed(offset))
			}
			LineAddr::PatternRev(ref pat) |
			LineAddr::Pattern(ref pat) => {
				if let Ok(regex) = Regex::new(pat) {
					self.last_pattern_search = Some(regex.clone());
					let haystack = self.buffer.as_str();
					let matches = regex.find_iter(haystack).collect::<Vec<_>>();
					// We will use this match if we don't find any in our desired direction, just like vim
					let wrap_match: Option<&regex::Match> = match &addr {
						LineAddr::Pattern(_) => matches.first(),
						LineAddr::PatternRev(_) => matches.last(),
						_ => unreachable!()
					};
					let cursor_byte_pos = self.read_cursor_byte_pos();
					match addr {
						LineAddr::Pattern(_) => {
							for mat in &matches {
								if mat.start() > cursor_byte_pos {
									let match_line_no = self.byte_pos_line_number(mat.start());
									return Some(match_line_no)
								}
							}
						}
						LineAddr::PatternRev(_) => {
							let matches = matches.iter().rev();
							for mat in matches {
								if mat.start() < cursor_byte_pos {
									let match_line_no = self.byte_pos_line_number(mat.start());
									return Some(match_line_no)
								}
							}
						}
						_ => unreachable!()
					}
					let mat = wrap_match?;
					let match_line_no = self.byte_pos_line_number(mat.start());
					Some(match_line_no)
				} else {
					match addr {
						LineAddr::Pattern(_) => {
							let haystack = self.slice_from_cursor()?;
							let pos = haystack.as_bytes().windows(pat.len()).position(|win| win == pat.as_bytes())?;
							let line_no = self.byte_pos_line_number(pos);
							Some(line_no)
						}
						LineAddr::PatternRev(_) => {
							let haystack = self.slice_from_cursor()?;
							let haystack_rev = haystack.bytes().rev().collect::<Vec<_>>();
							let pat_rev = pat.bytes().rev().collect::<Vec<_>>();
							let pos = haystack_rev.windows(pat.len()).position(|win| win == pat_rev)?;
							let line_no = self.byte_pos_line_number(pos);
							Some(line_no)
						}
						_ => unreachable!()
					}
				}
			}
		}
	}
	pub fn should_handle_block_insert(&self) -> bool {
		self.inserting_from_visual &&
		self.last_selection.as_ref().is_some_and(|sel| matches!(sel, SelectRange::TwoDim(_)))
	}
	pub fn handle_block_insert(&mut self) {
		/*
		 * The last selection was a visual block, so we need to insert the text
		 * at the start of each window in the selection.
		 *
		 * We can be clever here and use the last edit in the undo stack to figure out what to insert.
		 */
		let Some(last_edit) = self.undo_stack.last().cloned() else { return };
		let Some(SelectRange::TwoDim(sel)) = self.last_selection.clone() else { return };
		let last_diff = last_edit.new_diff[..last_edit.merge_pos].to_string();

		let start_pos = last_edit.pos;
		let start_line = self.index_line_number(start_pos);
		let end_line = start_line + sel.len();

		let mut line_range = start_line..end_line;
		line_range.next(); // Skip the first line, we already inserted it
		let line_range = line_range.rev(); // Reverse the range to preserve position validity as we iterate

		let start_col = self.index_col(start_pos);

		for line in line_range {
			let Some((start,_)) = self.line_bounds(line) else { continue };
			let pos = start + start_col;
			self.insert_str_at(pos, &last_diff);
		}

	}
	pub fn eval_motion(&mut self, verb: Option<&Verb>, motion: MotionCmd) -> MotionKind {
		match motion {
			MotionCmd(_,Motion::NotGlobal(ref addr, ref pattern)) |
			MotionCmd(_,Motion::Global(ref addr, ref pattern)) => {
				let (start_line,end_line) = match **addr {
					Motion::Line(ref n) => {
						let line_no = self.eval_line_addr(n.clone()).unwrap();
						(line_no,line_no)
					}
					Motion::LineRange(ref s,ref e) => {
						let start_ln = self.eval_line_addr(s.clone()).unwrap();
						let end_ln = self.eval_line_addr(e.clone()).unwrap();
						(start_ln,end_ln)
					}
					_ => (0,self.total_lines())
				};
				let mut lines = vec![];
				let line_range = start_line..end_line;
				let regex = match pattern {
					Val::Regex(regex) => regex.clone(),
					_ => match Regex::new(&pattern.to_string()) {
						Ok(regex) => regex,
						Err(e) => {
							eprintln!("vicut: {e}");
							std::process::exit(1);
						}
					}
				};
				for line_no in line_range {
					let Some((start,end)) = self.line_bounds(line_no) else { continue };
					let line = self.slice(start..end).unwrap_or_default();

					match motion.1 {
						Motion::NotGlobal(_,_) => {
							let None = regex.find(line).map(|mat| (mat.start(),mat.end())) else { continue };
						}
						Motion::Global(_,_) => {
							let Some(_) = regex.find(line).map(|mat| (mat.start(),mat.end())) else { continue };
						}
						_ => unreachable!()
					}
					lines.push(line_no);
				}
				let reversed = lines.into_iter().rev().collect::<Vec<_>>();
				MotionKind::Lines(reversed)
			}
			MotionCmd(count,Motion::WholeLineExclusive) |
			MotionCmd(count,Motion::WholeLine) => {
				let Some((start,end)) = (match motion.1 {
					Motion::WholeLineExclusive => {
						self.select_lines_down(count.saturating_sub(1))
					}
					Motion::WholeLine => {
						self.select_lines_down(count)
					}
					_ => unreachable!()
				}) else { return MotionKind::Null };

				let target_col = if let Some(col) = self.saved_col {
					col
				} else {
					let col = self.cursor_col();
					self.saved_col = Some(col);
					col
				};

				let Some(line) = self.slice(start..end).map(|s| s.to_string()) else {
					return MotionKind::Null
				};
				let mut target_pos = self.grapheme_index_for_display_col(&line, target_col);
				if self.cursor.exclusive && line.ends_with("\n") && self.grapheme_at(target_pos) == Some("\n") {
					target_pos = target_pos.saturating_sub(1); // Don't land on the newline
				}
				MotionKind::InclusiveWithTargetCol((start,end),target_pos)
			}
			MotionCmd(count,Motion::WordMotion(to, word, dir)) => {
				// 'cw' is a weird case
				// if you are on the word's left boundary, it will not delete whitespace after the end of the word
				let include_last_char = verb == Some(&Verb::Change) &&
					matches!(motion.1, Motion::WordMotion(To::Start, _, Direction::Forward));

				let pos = self.dispatch_word_motion(count, to, word, dir, include_last_char);
				let pos = ClampedUsize::new(pos,self.cursor.max,false);
				// End-based operations must include the last character
				// But the cursor must also stop just before it when moving
				// So we have to do some weird shit to reconcile this behavior
				if to == To::End {
					match dir {
						Direction::Forward => {
							MotionKind::Onto(pos.get())
						}
						Direction::Backward => {
							let (start,end) = ordered(self.cursor.get(),pos.get());
							MotionKind::Inclusive((start,end))
						}
					}
				} else {
					MotionKind::On(pos.get())
				}
			}
			MotionCmd(count,Motion::TextObj(text_obj)) => {
				let Some((start,end)) = self.dispatch_text_obj(count, text_obj.clone()) else {
					return MotionKind::Null
				};
				match text_obj {
					TextObj::Paragraph(dir) => {
						match dir {
							Direction::Forward => MotionKind::On(end),
							Direction::Backward => {
								let cur_paragraph_start = start;
								let mut start_pos = self.cursor.get();
								for _ in 0..count {
									if self.is_paragraph_start(start_pos) {
										let Some((new_start,_)) = self.text_obj_paragraph(start_pos.saturating_sub(1), 1, Bound::Inside) else {
											return MotionKind::Null
										};
										start_pos = new_start;
										continue
									} else {
										start_pos = cur_paragraph_start;
									}
								}
								MotionKind::On(start_pos)
							}
						}
					}
					TextObj::Sentence(dir) => {
						match dir {
							Direction::Forward => MotionKind::On(end),
							Direction::Backward => {
								let cur_sentence_start = start;
								let mut start_pos = self.cursor.get();
								for _ in 0..count {
									if self.is_sentence_start(start_pos) {
										// We know there is some punctuation before us now
										// Let's find it
										let mut bkwd_indices = (0..start_pos).rev();
										let punct_pos = bkwd_indices
											.find(|idx| self.grapheme_at(*idx).is_some_and(|gr| PUNCTUATION.contains(&gr)))
											.unwrap();
										if self.grapheme_before(punct_pos).is_some() {
											let Some((new_start,_)) = self.text_obj_sentence(punct_pos - 1, count, Bound::Inside) else {
												return MotionKind::Null
											};
											start_pos = new_start;
											continue
										} else {
											return MotionKind::Null
										}
									} else {
										start_pos = cur_sentence_start;
									}
								}
								MotionKind::On(start_pos)
							}
						}
					}
					TextObj::Word(_, bound) |
					TextObj::WholeSentence(bound) |
					TextObj::WholeParagraph(bound) => {
						match bound {
							Bound::Inside => MotionKind::Inclusive((start,end)),
							Bound::Around => MotionKind::Exclusive((start,end)),
						}
					}
					TextObj::DoubleQuote(_) |
					TextObj::SingleQuote(_) |
					TextObj::BacktickQuote(_) |
					TextObj::Paren(_) |
					TextObj::Bracket(_) |
					TextObj::Brace(_) |
					TextObj::Angle(_) => MotionKind::Exclusive((start,end)),
					_ => todo!()
				}
			}
			MotionCmd(_,Motion::ToDelimMatch) => {
				// Just ignoring the count here, it does some really weird stuff in Vim
				// try doing something like '5%' in vim, it is really strange
				let Some(pos) = self.find_next_matching_delim() else {
					return MotionKind::Null
				};
				MotionKind::Onto(pos)
			}
			MotionCmd(_,Motion::ToBrace(direction)) |
			MotionCmd(_,Motion::ToBracket(direction)) |
			MotionCmd(_,Motion::ToParen(direction)) => {
				// Counts don't seem to do anything significant for these either
				let delim = match motion.1 {
					Motion::ToBrace(_) => Delim::Brace,
					Motion::ToBracket(_) => Delim::Bracket,
					Motion::ToParen(_) => Delim::Paren,
					_ => unreachable!()
				};
				let Some(pos) = self.find_unmatched_delim(delim, direction) else {
					return MotionKind::Null
				};

				MotionKind::On(pos)
			}
			MotionCmd(count,Motion::EndOfLastWord) => {
				let start = self.start_of_line();
				let mut newline_count = 0;
				let mut indices = start..self.cursor.max;
				let mut last_graphical = None;
				while let Some(idx) = indices.next() {
					let grapheme = self.grapheme_at(idx).unwrap();
					if !is_whitespace(grapheme) {
						last_graphical = Some(idx);
					}
					if grapheme == "\n" {
						newline_count += 1;
						if newline_count == count {
							break
						}
					}
				}
				let Some(last) = last_graphical else {
					return MotionKind::Null
				};
				MotionKind::On(last)
			}
			MotionCmd(_,Motion::FirstGraphicalOnScreenLine) |
				MotionCmd(_,Motion::BeginningOfFirstWord) => {
					let start = self.start_of_line();
					let mut indices = start..self.cursor.max;
					let mut first_graphical = None;
					while let Some(idx) = indices.next() {
						let grapheme = self.grapheme_at(idx).unwrap();
						if !is_whitespace(grapheme) {
							first_graphical = Some(idx);
							break
						}
						if grapheme == "\n" {
							break
						}
					}
					let Some(first) = first_graphical else {
						return MotionKind::Null
					};
					MotionKind::On(first)
				}
			MotionCmd(_,Motion::BeginningOfScreenLine) |
			MotionCmd(_,Motion::BeginningOfLine) => MotionKind::On(self.start_of_line()),
			MotionCmd(count,Motion::EndOfLine) => {
				let pos = if count == 1 {
					self.end_of_line()
				} else if let Some((_,end)) = self.select_lines_down(count.saturating_sub(1)) {
					end
				} else {
					self.end_of_line()
				};
				if self.grapheme_at(pos) == Some("\n") {
					// If we are at the end of the line, we want to go back one
					// So we don't land on the newline
					MotionKind::On(pos.saturating_sub(1))
				} else {
					MotionKind::On(pos)
				}
			}
			MotionCmd(count,Motion::CharSearch(direction, dest, ch)) => {
				let mut ch_buf = [0u8;4];
				let ch_str = ch.encode_utf8(&mut ch_buf);
				let mut pos = self.cursor;
				for _ in 0..count {
					match direction {
						Direction::Forward => {
							let after = pos.ret_add(1);
							let mut indices_iter = after..pos.max;

							let Some(ch_pos) = indices_iter.find(|i| {
								self.grapheme_at(*i) == Some(ch_str)
							}) else {
								return MotionKind::Null
							};
							pos.set(ch_pos);
						}
						Direction::Backward => {
							let before = pos.ret_sub(1);
							let mut indices_iter = (0..before).rev();

							let Some(ch_pos) = indices_iter.find(|i| {
								self.grapheme_at(*i) == Some(ch_str)
							}) else {
								return MotionKind::Null
							};
							pos.set(ch_pos);
						}
					}
					if dest == Dest::Before {
						match direction {
							Direction::Forward => pos.sub(1),
							Direction::Backward => pos.add(1),
						}
					}
				}
				MotionKind::Onto(pos.get())
			}
			MotionCmd(count,motion @ (Motion::ForwardChar | Motion::BackwardChar)) => {
				let mut target = self.cursor;
				target.exclusive = false;
				for _ in 0..count {
					match motion {
						Motion::BackwardChar => target.sub(1),
						Motion::ForwardChar => {
							if !self.is_selecting() && self.cursor.exclusive && self.grapheme_at(target.ret_add(1)) == Some("\n") {
								return MotionKind::Null
							}
							if self.is_selecting() && self.grapheme_at(target.get()) == Some("\n") {
								break
							}
							target.add(1);
							continue
						}
						_ => unreachable!()
					}
					if self.grapheme_at(target.get()) == Some("\n") {
						return MotionKind::Null
					}
				}
				MotionKind::On(target.get())
			}
			MotionCmd(count, Motion::NextMatch) => {
				let Some(regex) = self.last_pattern_search.as_ref() else {
					return MotionKind::Null
				};
				let haystack = self.buffer.as_str();
				let matches = regex.find_iter(haystack).collect::<Vec<_>>();
				let wrap_match: Option<&regex::Match> = matches.first();
				let cursor_byte_pos = self.read_cursor_byte_pos();
				let mut fwd_matches = 0;
				for mat in &matches {
					if mat.start() > cursor_byte_pos {
						fwd_matches += 1;
						if fwd_matches == count {
							let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
							return MotionKind::On(match_idx)
						}
					}
				}
				let Some(mat) = wrap_match else { return MotionKind::Null };
				let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
				MotionKind::Onto(match_idx)
			}
			MotionCmd(count, Motion::PrevMatch) => {
				let Some(regex) = self.last_pattern_search.as_ref() else {
					return MotionKind::Null
				};
				let haystack = self.read_slice_to_cursor().unwrap();
				let matches = regex
					.find_iter(haystack)
					.collect::<Vec<_>>()
					.into_iter()
					.rev()
					.collect::<Vec<_>>(); // I'm gonna be sick
				let wrap_match: Option<&regex::Match> = matches.last();
				let cursor_byte_pos = self.read_cursor_byte_pos();
				let mut bkwd_matches = 0;
				for mat in &matches {
					if mat.start() < cursor_byte_pos {
						bkwd_matches += 1;
						if bkwd_matches == count {
							let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
							return MotionKind::On(match_idx)
						}
					}
				}
				let Some(mat) = wrap_match else { return MotionKind::Null };
				let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
				MotionKind::Onto(match_idx)
			}
			MotionCmd(_count, Motion::PatternSearchRev(ref pat)) |
			MotionCmd(_count, Motion::PatternSearch(ref pat)) => {
				match Regex::new(pat) {
					Ok(regex) => {
						self.last_pattern_search = Some(regex.clone());
						let haystack = self.buffer.as_str();
						let matches = regex.find_iter(haystack).collect::<Vec<_>>();
						// We will use this match if we don't find any in our desired direction, just like vim
						let wrap_match: Option<&regex::Match> = match &motion.1 {
							Motion::PatternSearch(_) => matches.first(),
							Motion::PatternSearchRev(_) => matches.last(),
							_ => unreachable!()
						};
						let cursor_byte_pos = self.read_cursor_byte_pos();
						match &motion.1 {
							Motion::PatternSearch(_) => {
								for mat in &matches {
									if mat.start() > cursor_byte_pos {
										let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
										return MotionKind::Onto(match_idx)
									}
								}
							}
							Motion::PatternSearchRev(_) => {
								let matches = matches.iter().rev();
								for mat in matches {
									if mat.start() < cursor_byte_pos {
										let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
										return MotionKind::Onto(match_idx)
									}
								}
							}
							_ => unreachable!()
						}
						let Some(mat) = wrap_match else { return MotionKind::Null };
						let Some(match_idx) = self.find_index_for_byte_pos(mat.start()) else { return MotionKind::Null };
						MotionKind::Onto(match_idx)
					}
					Err(e) => {
						eprintln!("vicut: {e}");
						std::process::exit(1);
					}
				}
			}
			MotionCmd(count, Motion::ForwardCharForced) => MotionKind::On(self.cursor.ret_add(count)),
			MotionCmd(count, Motion::BackwardCharForced) => MotionKind::On(self.cursor.ret_sub(count)),
			MotionCmd(count,Motion::LineDown) |
			MotionCmd(count,Motion::ScreenLineUp) |
			MotionCmd(count,Motion::ScreenLineDown) |
			MotionCmd(count,Motion::LineUp) => {
				let Some((start,end)) = (match motion.1 {
					Motion::ScreenLineUp |
					Motion::LineUp => self.nth_prev_line(count),
					Motion::ScreenLineDown |
					Motion::LineDown => self.nth_next_line(count),
					_ => unreachable!()
				}) else {
					return MotionKind::Null
				};

				let target_col = if let Some(col) = self.saved_col {
					col
				} else {
					let col = self.cursor_col();
					self.saved_col = Some(col);
					col
				};

				let Some(line) = self.slice(start..end).map(|s| s.to_string()) else {
					return MotionKind::Null
				};
				let mut target_pos = self.grapheme_index_for_display_col(&line, target_col);
				if self.cursor.exclusive && line.ends_with("\n") && self.grapheme_at(target_pos) == Some("\n") {
					target_pos = target_pos.saturating_sub(1); // Don't land on the newline
				}

				let (start,end) = match motion.1 {
					Motion::LineUp => (start,self.end_of_line()),
					Motion::LineDown => (self.start_of_line(),end),
					_ => unreachable!()
				};

				MotionKind::InclusiveWithTargetCol((start,end),target_pos)
			}
			MotionCmd(count,Motion::LineDownCharwise) |
			MotionCmd(count,Motion::ScreenLineUpCharwise) |
			MotionCmd(count,Motion::ScreenLineDownCharwise) |
			MotionCmd(count,Motion::LineUpCharwise) => {
				let Some((start,end)) = (match motion.1 {
					Motion::ScreenLineUpCharwise |
					Motion::LineUpCharwise => self.nth_prev_line(count),
					Motion::ScreenLineDownCharwise |
					Motion::LineDownCharwise => self.nth_next_line(count),
					_ => unreachable!()
				}) else {
					return MotionKind::Null
				};

				let target_col = if let Some(col) = self.saved_col {
					col
				} else {
					let col = self.cursor_col();
					self.saved_col = Some(col);
					col
				};

				let Some(line) = self.slice(start..end).map(|s| s.to_string()) else {
					return MotionKind::Null
				};
				let target_pos = start + self.grapheme_index_for_display_col(&line, target_col);

				MotionKind::On(target_pos)
			}
			MotionCmd(_count,Motion::WholeBuffer) => MotionKind::Exclusive((0,self.grapheme_indices().len())),
			MotionCmd(_count,Motion::BeginningOfBuffer) => {
				let lines_up = self.cursor_line_number();
				let cursor_col = self.cursor_col();
				self.saved_col = Some(cursor_col);
				MotionKind::LineOffset(-(lines_up as isize))
			}
			MotionCmd(_count,Motion::EndOfBuffer) => {
				let lines_down = self.total_lines() - self.cursor_line_number();
				let cursor_col = self.cursor_col();
				self.saved_col = Some(cursor_col);
				MotionKind::LineOffset(lines_down as isize)
			}
			MotionCmd(count,Motion::ToColumn) => {
				let start = ClampedUsize::new(self.start_of_line(), self.cursor.max, false);
				let target_col = count.saturating_sub(1);
				MotionKind::On(start.ret_add(target_col))
			}
			MotionCmd(count,Motion::RangeInclusive(ref range)) |
			MotionCmd(count,Motion::Range(ref range)) => {
				let is_inclusive = matches!(motion.1, Motion::RangeInclusive(_));
				match range {
					SelectRange::OneDim((start,end)) => {
						let mut final_end = *end;
						if self.cursor.exclusive {
							final_end += 1;
						}
						let delta = end - start;
						let count = count.saturating_sub(1); // Becomes number of times to multiply the range

						for _ in 0..count {
							final_end += delta;
						}

						final_end = final_end.min(self.cursor.max);
						if is_inclusive {
							MotionKind::Inclusive((*start,final_end))
						} else {
							MotionKind::Exclusive((*start,final_end))
						}
					}
					SelectRange::TwoDim(windows) => {
						MotionKind::BlockRange(windows.to_vec())
					}
				}
			}
			MotionCmd(_, Motion::Line(addr)) => {
				let Some(line_no) = self.eval_line_addr(addr) else {
					return MotionKind::Null
				};
				MotionKind::Line(line_no)
			}
			MotionCmd(_, Motion::LineRange(start_addr, end_addr)) => {
				let Some(start_line_no) = self.eval_line_addr(start_addr) else {
					return MotionKind::Null
				};
				let Some(end_line_no) = self.eval_line_addr(end_addr) else {
					return MotionKind::Null
				};
				MotionKind::LineRange(start_line_no, end_line_no)
			}
			MotionCmd(_,Motion::RepeatMotion) | // These two were already handled in exec.rs
			MotionCmd(_,Motion::RepeatMotionRev) |
			MotionCmd(_,Motion::Null) => MotionKind::Null,
			_ => unimplemented!("Not implemented: {motion:?}")
		}
	}
	pub fn apply_motion(&mut self, motion: MotionKind) {
		self.move_cursor(motion);
		self.update_graphemes_lazy();
		self.update_select_range();
	}
	pub fn update_select_range(&mut self) {
		if let Some(mut mode) = self.select_mode {
			let Some(range) = self.select_range.clone() else {
				return
			};
			match range {
				SelectRange::OneDim((mut start,mut end)) => {
					match mode {
						SelectMode::Char(anchor) => {
							// Just use literal cursor position
							match anchor {
								SelectAnchor::End => {
									start = self.cursor.get();
								}
								SelectAnchor::Start => {
									end = self.cursor.get();
								}
							}
							if start >= end {
								mode.invert_anchor();
								std::mem::swap(&mut start, &mut end);

								self.select_mode = Some(mode);
							}
							self.select_range = Some(SelectRange::OneDim((start,end)));
						}
						SelectMode::Line(anchor) => {
							let old_end = end;
							// If we are in line select mode, we need to update based on the cursor's line
							match anchor {
								SelectAnchor::End => {
									start = self.start_of_line();
								}
								SelectAnchor::Start => {
									end = self.end_of_line();
								}
							}
							if start >= end {
								mode.invert_anchor();
								std::mem::swap(&mut start, &mut end);
								end = old_end; // Hack powers activate
															 // I have no idea why this works
															 // And I'm not going to question it
															 // or alter this code ever again
								match mode.anchor() {
									SelectAnchor::End => {
										start = self.start_of_line();
									}
									SelectAnchor::Start => {
										end = self.end_of_line();
									}
								}

								self.select_mode = Some(mode);
							}
							self.select_range = Some(SelectRange::OneDim((start,end)));
						}
						_ => unreachable!()
					}
				}
				SelectRange::TwoDim(mut windows) => {
					windows = self.get_block_select_windows(&mode);
					self.select_range = Some(SelectRange::TwoDim(windows));
				}
			}
			// If we are in select mode, we need to update the selection range
		}
	}
	pub fn move_cursor(&mut self, motion: MotionKind) {
		match motion {
			MotionKind::Onto(pos) | // Onto follows On's behavior for cursor movements
			MotionKind::On(pos) => { self.cursor.set(pos); },
			MotionKind::To(pos) => {
				self.cursor.set(pos);

				match pos.cmp(&self.cursor.get()) {
					std::cmp::Ordering::Less => {
						self.cursor.add(1);
					}
					std::cmp::Ordering::Greater => {
						self.cursor.sub(1);
					}
					std::cmp::Ordering::Equal => { /* Do nothing */ }
				}
			}
			MotionKind::BlockRange(windows) => {
				let Some(first) = windows.first() else { return };
				self.cursor.set(first.0);
			}
			MotionKind::LineRange(n,_) |
			MotionKind::Line(n) => {
				let Some((start,_)) = self.line_bounds(n) else { return };
				self.cursor.set(start);
			}
			MotionKind::LineOffset(offset) => {
				let cursor_line = self.cursor_line_number();
				let target_line = cursor_line.saturating_add_signed(offset);
				if target_line > self.total_lines() {
					self.cursor.set(self.cursor.max);
				} else {
					let Some((mut target_pos,_)) = self.line_bounds(target_line) else { return };
					if let Some(col) = self.saved_col {
						target_pos += col;
					}
					self.cursor.set(target_pos);
				}
			}
			MotionKind::ExclusiveWithTargetCol((_,_),col) |
				MotionKind::InclusiveWithTargetCol((_,_),col) => {
					let (start,end) = self.this_line();
					let end = end.min(col);
					self.cursor.set(start + end);
				}
			MotionKind::Inclusive((start,mut end)) => {
				if self.select_range().is_none() {
					self.cursor.set(start);
				} else {
					if start < self.cursor.get() {
						self.cursor.set(start);
						if let Some(mode) = self.select_mode.as_mut() {
							mode.set_anchor(SelectAnchor::End);
						}
						end += 1;
					} else {
						self.cursor.set(end);
						if let Some(mode) = self.select_mode.as_mut() {
							mode.set_anchor(SelectAnchor::Start);
						}
					}
					self.select_range = Some(SelectRange::OneDim((start,end)));
				}
			}
			MotionKind::Exclusive((start,end)) => {
				if self.select_range().is_none() {
					self.cursor.set(start);
				} else {
					if start < self.cursor.get() {
						let start = start + 1;
						self.cursor.set(start);
						if let Some(mode) = self.select_mode.as_mut() {
							mode.set_anchor(SelectAnchor::End);
						}
					} else {
						let end = end.saturating_sub(1);
						self.cursor.set(end);
						if let Some(mode) = self.select_mode.as_mut() {
							mode.set_anchor(SelectAnchor::Start);
						}
					}
					self.select_range = Some(SelectRange::OneDim((start,end)));
				}
			}
			MotionKind::Lines(_) => {
				/*
					This motionkind is only created by :g
					And global is handled in a Vicut method, not in here
				 */
				unreachable!()
			}
			MotionKind::Null => { /* Do nothing */ }
		}
	}
	pub fn range_from_motion(&mut self, motion: &MotionKind) -> Option<(usize,usize)> {
		let range = match motion {
			MotionKind::BlockRange(_) => {
				/*
				 * This one requires special handling
				 * So we shouldn't be here
				 */
				return None
			}
			MotionKind::LineOffset(offset) => {
				let (mut start,mut end) = self.this_line();
				let cursor_line = self.cursor_line_number();
				let target_line = cursor_line.saturating_add_signed(*offset);
				match target_line.cmp(&cursor_line) {
					Ordering::Less => {
						start = self.line_bounds(target_line)?.0;
					}
					Ordering::Greater => {
						end = self.line_bounds(target_line)?.1;
					}
					Ordering::Equal => {}
				}
				return Some((start,end))
			}
			MotionKind::On(pos) => ordered(self.cursor.get(), *pos),
			MotionKind::Onto(pos) => {
				// For motions which include the character at the cursor during operations
				// but exclude the character during movements
				let mut pos = ClampedUsize::new(*pos, self.cursor.max, false);
				let cursor_pos = self.cursor;

				// We are moving forwards, so add one
				if pos.get() > cursor_pos.get() {
					pos.add(1)
				}
				ordered(cursor_pos.get(),pos.get())
			}
			MotionKind::Line(n) => {
				let (start,end) = self.line_bounds(*n)?;
				(start,end)
			}
			MotionKind::LineRange(first, last) => {
				let (start,_) = self.line_bounds(*first)?;
				let (_,end) = self.line_bounds(*last)?;
				(start,end)
			}
			MotionKind::To(pos) => {
				let pos = match pos.cmp(&self.cursor.get()) {
					std::cmp::Ordering::Less => *pos + 1,
					std::cmp::Ordering::Greater => *pos - 1,
					std::cmp::Ordering::Equal => *pos,
				};
				ordered(self.cursor.get(), pos)
			}
			MotionKind::InclusiveWithTargetCol((start,end),_) |
			MotionKind::Exclusive((start,end)) => ordered(*start, *end),
			MotionKind::ExclusiveWithTargetCol((start,end),_) |
			MotionKind::Inclusive((start,end)) => {
				let (start, mut end) = ordered(*start, *end);
				end = ClampedUsize::new(end,self.cursor.max,false).ret_add(1);
				(start,end)
			}
			MotionKind::Lines(_) |
			MotionKind::Null => return None
		};
		Some(range)
	}
	pub fn get_register_content(&mut self, verb: &Verb, motion: &MotionKind) -> RegisterContent {
		let should_drain = verb == &Verb::Delete || verb == &Verb::Change;
		match motion {
			MotionKind::BlockRange(windows) => {
				let content = if should_drain {
					let content = windows.iter()
						.rev() // Reverse the order so that the spans stay valid
						.map(|(start,end)| {
							self.drain(*start,*end)
						})
						.collect::<Vec<_>>();
					self.update_graphemes();
					content
				} else {
					windows.iter()
						.map(|(start,end)| {
							self.slice(*start..*end)
								.map(|s| s.to_string())
								.unwrap_or_default()
						})
						.collect::<Vec<_>>()
				};
				RegisterContent::Block(content)
			}
			MotionKind::Line(line_no) => {
				let Some((start,end)) = self.line_bounds(*line_no) else {
					return RegisterContent::Empty
				};
				let line_content = if should_drain {
					let content = self.drain(start,end);
					self.update_graphemes();
					content
				} else {
					self.slice(start..end)
						.map(|s| s.to_string())
						.unwrap_or_default()
				};
				RegisterContent::Line(line_content)
			}
			MotionKind::LineRange(start,end) => {
				let Some((start,_)) = self.line_bounds(*start) else {
					return RegisterContent::Empty
				};
				let Some((_,end)) = self.line_bounds(*end) else {
					return RegisterContent::Empty
				};
				let line_content = if should_drain {
					let content = self.drain(start,end);
					self.update_graphemes();
					content
				} else {
					self.slice(start..end)
						.map(|s| s.to_string())
						.unwrap_or_default()
				};
				RegisterContent::Line(line_content)
			}
			_ => {
				let Some((start,end)) = self.range_from_motion(motion) else {
					return RegisterContent::Empty
				};
				if should_drain {
					// If we are deleting or changing, we need to drain the content
					// and update the grapheme indices
					let drained = self.drain(start,end);
					self.update_graphemes();
					RegisterContent::Span(drained)
				} else {
					// If we are yanking, we just need to get the content
					let content = self.slice(start..end)
						.map(|s| s.to_string())
						.unwrap_or_default();
					RegisterContent::Span(content)
				}
			}
		}
	}
	#[allow(clippy::unnecessary_to_owned)]
	pub fn exec_verb(&mut self, verb: Verb, motion: MotionKind, register: RegisterName) -> Result<(),String> {
		match verb {
			Verb::Delete |
			Verb::Yank |
			Verb::Change => {
				let content = self.get_register_content(&verb, &motion);
				register.write_to_register(content);
				if let Some(SelectRange::TwoDim(sel)) = self.select_range.as_ref() {
					// If we are in visual block, the cursor is set to the start of the first window
					let new_pos = sel.first().map_or(0, |(start,_)| *start);
					if self.grapheme_at(new_pos) == Some("\n") && self.grapheme_before(new_pos) != Some("\n") {
						self.cursor.set(new_pos.saturating_sub(1));
					} else {
						self.cursor.set(new_pos);
					}
				} else {
					match motion {
						MotionKind::ExclusiveWithTargetCol((_,_),pos) |
							MotionKind::InclusiveWithTargetCol((_,_),pos) => {
								let (start,end) = self.this_line();
								self.cursor.set(start);
								self.cursor.add(end.min(pos));
							}
						_ => {
							let Some((start,_)) = self.range_from_motion(&motion) else {
								self.move_cursor(motion);
								return Ok(())
							};
							self.cursor.set(start);
						}
					}
				}
			}
			Verb::Rot13 => {
				let Some((start,end)) = self.range_from_motion(&motion) else {
					return Ok(())
				};
				let slice = self.slice(start..end)
					.unwrap_or_default();
				let rot13 = rot13(slice);
				self.buffer.replace_range(start..end, &rot13);
				self.cursor.set(start);
			}
			Verb::ReplaceChar(ch) => {
				let mut buf = [0u8;4];
				let new = ch.encode_utf8(&mut buf);
				self.replace_at_cursor(new);
				self.apply_motion(motion);
			}
			Verb::ReplaceCharInplace(ch,count) => {
				for i in 0..count {
					let mut buf = [0u8;4];
					let new = ch.encode_utf8(&mut buf);
					self.replace_at_cursor(new);

					// try to increment the cursor until we are on the last iteration
					// or until we hit the end of the buffer
					if i != count.saturating_sub(1) && !self.cursor.inc() {
						break
					}
				}
			}
			Verb::ToggleCaseInplace(count) => {
				for i in 0..count {
					let Some(gr) = self.grapheme_at_cursor() else {
						return Ok(())
					};
					if gr.len() > 1 || gr.is_empty() {
						return Ok(())
					}
					let ch = gr.chars().next().unwrap();
					if !ch.is_alphabetic() {
						return Ok(())
					}
					let mut buf = [0u8;4];
					let new = if ch.is_ascii_lowercase() {
						ch.to_ascii_uppercase().encode_utf8(&mut buf)
					} else {
						ch.to_ascii_lowercase().encode_utf8(&mut buf)
					};
					self.replace_at_cursor(new);

					// try to increment the cursor until we are on the last iteration
					// or until we hit the end of the buffer
					if i != count.saturating_sub(1) && !self.cursor.inc() {
						break
					}
				}
			}
			Verb::ToggleCaseRange => {
				let Some((start,end)) = self.range_from_motion(&motion) else {
					return Ok(())
				};
				for i in start..end {
					let Some(gr) = self.grapheme_at(i) else {
						continue
					};
					if gr.len() > 1 || gr.is_empty() {
						continue
					}
					let ch = gr.chars().next().unwrap();
					if !ch.is_alphabetic() {
						continue
					}
					let mut buf = [0u8;4];
					let new = if ch.is_ascii_lowercase() {
						ch.to_ascii_uppercase().encode_utf8(&mut buf)
					} else {
						ch.to_ascii_lowercase().encode_utf8(&mut buf)
					};
					self.replace_at(i,new);
				}
			}
			Verb::ToLower => {
				let Some((start,end)) = self.range_from_motion(&motion) else {
					return Ok(())
				};
				for i in start..end {
					let Some(gr) = self.grapheme_at(i) else {
						continue
					};
					if gr.len() > 1 || gr.is_empty() {
						continue
					}
					let ch = gr.chars().next().unwrap();
					if !ch.is_alphabetic() {
						continue
					}
					let mut buf = [0u8;4];
					let new = if ch.is_ascii_uppercase() {
						ch.to_ascii_lowercase().encode_utf8(&mut buf)
					} else {
						ch.encode_utf8(&mut buf)
					};
					self.replace_at(i,new);
				}
			}
			Verb::ToUpper => {
				let Some((start,end)) = self.range_from_motion(&motion) else {
					return Ok(())
				};
				for i in start..end {
					let Some(gr) = self.grapheme_at(i) else {
						continue
					};
					if gr.len() > 1 || gr.is_empty() {
						continue
					}
					let ch = gr.chars().next().unwrap();
					if !ch.is_alphabetic() {
						continue
					}
					let mut buf = [0u8;4];
					let new = if ch.is_ascii_lowercase() {
						ch.to_ascii_uppercase().encode_utf8(&mut buf)
					} else {
						ch.encode_utf8(&mut buf)
					};
					self.replace_at(i,new);
				}
			}
			Verb::Redo |
				Verb::Undo => {
					let (edit_provider,edit_receiver) = match verb {
						// Redo = pop from redo stack, push to undo stack
						Verb::Redo => (&mut self.redo_stack, &mut self.undo_stack),
						// Undo = pop from undo stack, push to redo stack
						Verb::Undo => (&mut self.undo_stack, &mut self.redo_stack),
						_ => unreachable!()
					};
					let Some(edit) = edit_provider.pop() else { return Ok(()) };
					let Edit { pos, cursor_pos, old, old_diff, new, new_diff, merging: _, .. } = edit;

					self.buffer.replace_range(pos..pos + new.len(), &old);
					let new_cursor_pos = self.cursor.get();
					let in_insert_mode = !self.cursor.exclusive;

					if in_insert_mode {
						self.cursor.set(cursor_pos);
					}
					let new_edit = Edit {
						pos,
						merge_pos: 0,
						cursor_pos: new_cursor_pos,
						old: new,
						new: old,
						old_diff: new_diff,
						new_diff: old_diff,
						merging: false
					};
					edit_receiver.push(new_edit);
					self.update_graphemes();
				}
			Verb::Put(anchor) => {
				let Some(content) = register.read_from_register() else {
					return Ok(())
				};
				match motion {
					MotionKind::Line(n) => {
							let Some((start,end)) = self.line_bounds(n) else { return Ok(()) };
							let insert_idx = match anchor {
								Anchor::After => end,
								Anchor::Before => start
							};
							self.insert_register_content(insert_idx, content, anchor);
							self.cursor.set(insert_idx);
							let first_non_ws = self.eval_motion(None, MotionCmd(1,Motion::FirstGraphicalOnScreenLine));
							self.move_cursor(first_non_ws);
					}
					MotionKind::LineRange(s,e) => {
						let lines = (s..=e).rev();
						for line in lines {
							let Some((start,end)) = self.line_bounds(line) else { return Ok(()) };
							let insert_idx = match &anchor {
								Anchor::After => end,
								Anchor::Before => start
							};
							self.insert_register_content(insert_idx, content.clone(), anchor.clone());
							self.cursor.set(insert_idx);
							let first_non_ws = self.eval_motion(None, MotionCmd(1,Motion::FirstGraphicalOnScreenLine));
							self.move_cursor(first_non_ws);
						}
					}
					_ => {
						if register.is_line() {
							let insert_idx = match anchor {
								Anchor::After => self.end_of_line(),
								Anchor::Before => self.start_of_line()
							};
							self.insert_register_content(insert_idx, content, anchor);
							let down_line = self.eval_motion(None, MotionCmd(1,Motion::LineDownCharwise));
							self.move_cursor(down_line);
							let first_non_ws = self.eval_motion(None, MotionCmd(1,Motion::FirstGraphicalOnScreenLine));
							self.move_cursor(first_non_ws);
						} else {
							let insert_idx = match anchor {
								Anchor::After => self.cursor.ret_add(1),
								Anchor::Before => self.cursor.get()
							};
							let len = content.len();
							self.insert_register_content(insert_idx, content, anchor);
							if register.is_block() {
								self.cursor.set(insert_idx);
							} else {
								self.cursor.add(len.saturating_sub(1));
							}
						}
					}
				}
			}
			Verb::SwapVisualAnchor => {
				if let Some(range) = self.select_range.clone() {
					let Some(mode) = self.select_mode.as_mut() else { return Ok(()) };
					match mode {
						SelectMode::Char(anchor) => {
							let SelectRange::OneDim((start,end)) = range else { unreachable!() };
							let new_cursor_pos = match anchor {
								SelectAnchor::End => start,
								SelectAnchor::Start => end,
							};
							mode.invert_anchor();
							self.cursor.set(new_cursor_pos);
						}
						SelectMode::Line(anchor) => {
							let SelectRange::OneDim((start,end)) = range else { unreachable!() };
							let anchor_pos = match anchor {
								SelectAnchor::End => end,
								SelectAnchor::Start => start,
							};
							mode.invert_anchor();
							let cursor_col = self.cursor_col();
							let new_cursor_pos = {
								let line_no = self.index_line_number(anchor_pos);
								let (start,end) = self.line_bounds(line_no).unwrap_or((0,self.cursor.max));
								let line_len = end.saturating_sub(start);
								(start + cursor_col).min(line_len)
							};
							self.cursor.set(new_cursor_pos);
						}
						SelectMode::Block { anchor: _, anchor_pos } => {
							let mut cursor_pos = self.cursor.get();
							std::mem::swap(&mut cursor_pos, anchor_pos);
							self.cursor.set(cursor_pos);

							mode.invert_anchor();
						}
					}
				}
			}
			Verb::JoinLines => {
				let start = self.start_of_line();
				let Some((_,mut end)) = self.nth_next_line(1) else {
					return Ok(())
				};
				end = end.saturating_sub(1); // exclude the last newline
				let mut last_was_whitespace = false;
				for i in start..end {
					let Some(gr) = self.grapheme_at(i) else {
						continue
					};
					if gr == "\n" {
						if last_was_whitespace {
							self.remove(i);
						} else {
							self.force_replace_at(i, " ");
						}
						last_was_whitespace = false;
						continue
					}
					last_was_whitespace = is_whitespace(gr);
				}
			}
			Verb::InsertChar(ch) => {
				self.insert_at_cursor(ch);
				self.cursor.add(1);
			}
			Verb::Insert(string) => {
				self.push_str(&string);
				let graphemes = string.graphemes(true).count();
				self.cursor.add(graphemes);
			}
			Verb::Indent => {
				let Some((start,end)) = self.range_from_motion(&motion) else {
					return Ok(())
				};
				self.insert_at(start, '\t');
				let mut range_indices = self.grapheme_indices()[start..end].to_vec().into_iter();
				while let Some(idx) = range_indices.next() {
					let gr = self.grapheme_at(idx).unwrap();
					if gr == "\n" {
						let Some(idx) = range_indices.next() else {
							self.push('\t');
							break
						};
						self.insert_at(idx, '\t');
					}
				}

				match motion {
					MotionKind::ExclusiveWithTargetCol((_,_),pos) |
					MotionKind::InclusiveWithTargetCol((_,_),pos) => {
						self.cursor.set(start);
						let end = self.end_of_line();
						self.cursor.add(end.min(pos));
					}
					_ => { self.cursor.set(start); },
				}
			}
			Verb::Dedent => {
				let Some((start,end)) = self.range_from_motion(&motion) else { return Ok(()) };
				let mut indices_to_remove = vec![];
				if self.grapheme_at(start) == Some("\t") {
					indices_to_remove.push(start);
				}
				let mut range_indices = self.grapheme_indices()[start..end].to_vec().into_iter();
				while let Some(idx) = range_indices.next() {
					let Some(gr) = self.grapheme_at(idx) else { break };
					if gr == "\n" {
						let Some(idx) = range_indices.next() else { break };
						if self.grapheme_at(idx) == Some("\t") {
							indices_to_remove.push(idx);
						}
					}
				}

				for idx in indices_to_remove.iter().rev() {
					self.remove(*idx)
				}
			}
			Verb::InsertModeLineBreak(anchor) => {
				let (mut start,end) = self.this_line();
				if start == 0 && end == self.cursor.max {
					match anchor {
						Anchor::After => {
							self.push('\n');
							self.cursor.set(self.cursor_max());
							return Ok(())
						}
						Anchor::Before => {
							self.insert_at(0, '\n');
							self.cursor.set(0);
							return Ok(())
						}
					}
				}
				// We want the position of the newline, or start of buffer
				start = start.saturating_sub(1).min(self.cursor.max);
				match anchor {
					Anchor::After => {
						self.cursor.set(end);
						self.insert_at_cursor('\n');
					}
					Anchor::Before => {
						self.cursor.set(start);
						self.insert_at_cursor('\n');
						self.cursor.add(1);
					}
				}
			}
			Verb::Equalize => {
				let Ok(program) = env::var("EQUALPRG") else {
					eprintln!("vicut: '$EQUALPRG' is not set, ignoring '=' call");
					eprintln!("vicut: The '=' operator requires a path to a formatter program in the '$EQUALPRG' environment variable");
					return Ok(());
				};
				let Some((start, end)) = self.range_from_motion(&motion) else {
					return Ok(());
				};
				let start_ln = self.index_line_number(start);
				let end_ln = self.index_line_number(end);
				let Some((start, _)) = self.line_bounds(start_ln) else {
					return Ok(());
				};
				let Some((_, end)) = self.line_bounds(end_ln) else {
					return Ok(());
				};
				let Some(slice) = self.slice(start..end) else {
					return Ok(());
				};

				let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
				let bytes = Command::new(shell)
					.arg("-c")
					.arg(program)
					.stdin(Stdio::piped())
					.stdout(Stdio::piped())
					.spawn()
					.and_then(|mut child| {
						child.stdin.as_mut().unwrap().write_all(slice.as_bytes())?;
						child.wait_with_output()
					})
				.map_err(|e| format!("Failed to run command: {e}"))?;
				let output = String::from_utf8_lossy(&bytes.stdout).to_string();

				self.replace_range(start, end, &output);
			}
			Verb::ShellCmd(cmd) => {
				let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
				let child = Command::new(shell)
					.arg("-c")
					.arg(cmd)
					.output()
					.map_err(|e| format!("Failed to spawn child process: {e}"))?;

				if child.status.success() {
					return Ok(());
				} else {
					return Err(format!("Shell command exited with status {}", child.status.code().unwrap_or(-1)));
				}
			}
			Verb::Read(src) => {
				let insert_line = match motion {
					MotionKind::Line(n) => n,
					MotionKind::LineRange(_,e) => e,
					_ => self.cursor_line_number()
				};
				let (_,insert_pos) = self.line_bounds(insert_line).unwrap();
				let needs_newline = self.grapheme_at(insert_pos) != Some("\n");

				let data = match src {
					ReadSrc::Cmd(sh_cmd) => {
						let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
						let child = Command::new(shell)
							.arg("-c")
							.arg(sh_cmd)
							.output()
							.map_err(|e| format!("Failed to spawn child process for write: {e}"))?;

						if child.status.success() {
							String::from_utf8(child.stdout)
								.map_err(|e| format!("Command output was not valid UTF-8: {e}"))?
						} else {
							return Err(format!("Shell command exited with status {}", child.status.code().unwrap_or(-1)));
						}
					}
					ReadSrc::File(path) => {
						std::fs::read_to_string(path)
							.map_err(|e| format!("Failed to write to file: {e}"))?
					}
				};
				let needs_trailing_newline = !data.ends_with("\n") && insert_pos != self.cursor.max;
				let mut output = String::new();
				if needs_newline { writeln!(output).ok(); }
				write!(output,"{data}").ok();
				if needs_trailing_newline { writeln!(output).ok(); }

				self.insert_str_at(insert_pos, &output);
			}
			Verb::Write(dest) => {
				let (start_line,end_line) = match motion {
					MotionKind::Line(n) => (n,n),
					MotionKind::LineRange(s,e) => (s,e),
					_ => (0,self.total_lines())
				};
				let Some((start,_)) = self.line_bounds(start_line) else { return Ok(()) };
				let Some((_,end)) = self.line_bounds(end_line) else { return Ok(()) };

				let Some(write_span) = self.slice(start..end) else { return Ok(()) };

				match dest {
					WriteDest::File(path_buf) => {
						std::fs::write(path_buf, write_span)
							.map_err(|e| format!("Failed to write to file: {e}"))?;
					}
					WriteDest::FileAppend(path_buf) => {
						use std::fs::OpenOptions;

						let mut file = OpenOptions::new()
							.create(true)
							.append(true)
							.open(path_buf)
							.map_err(|e| format!("Failed to open file: {e}"))?;

						file.write_all(write_span.as_bytes())
							.map_err(|e| format!("Failed to write to file: {e}"))?;
					}
					WriteDest::Cmd(sh_cmd) => {
						let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
						let mut child = Command::new(shell)
							.arg("-c")
							.arg(sh_cmd)
							.stdin(Stdio::piped())
							.spawn()
							.map_err(|e| format!("Failed to spawn child process for write: {e}"))?;

						child.stdin.as_mut().unwrap().write_all(write_span.as_bytes())
							.map_err(|e| format!("Failed to pipe input to child process: {e}"))?;
						let status = child.wait()
							.map_err(|e| format!("Failed to wait for child process: {e}"))?;
						if !status.success() {
							eprintln!("Command exited with non-zero status");
						}
					}
				}
			}
			Verb::RepeatGlobal => {
				if let Some(global) = self.last_global.clone() {
					self.exec_verb(global, motion, register)?
				}
			}
			Verb::RepeatSubstitute => {
				let (start_line,end_line) = match motion {
					MotionKind::Line(n) => (n,n),
					MotionKind::LineRange(s,e) => (s,e),
					_ => (0,self.total_lines()),
				};
				// Have to temporarily move sub out of last_substitution
				// Because of mutable borrowing stuff
				if let Some(sub) = self.last_substitution.take() {
					let (ref regex,ref new,flags) = sub;
					let lines = (start_line..=end_line).rev();
					for line_no in lines {
						let Some((start,end)) = self.line_bounds(line_no) else { continue };
						let line = self.slice(start..end).unwrap();
						let global = flags.contains(SubFlags::GLOBAL);
						if global {
							let line_matches = regex
								.find_iter(line)
								.map(|mat| (mat.start(),mat.end()))
								.collect::<Vec<_>>()
								.into_iter()
								.rev();
							for (mat_start,mat_end) in line_matches {
								let mat_start = self.find_index_for_byte_pos(mat_start).unwrap();
								let mat_end = self.find_index_for_byte_pos(mat_end).unwrap();
								let real_start = start + mat_start;
								let real_end = start + mat_end;
								self.replace_range(real_start,real_end, new);
							}
						} else {
							let Some((mat_start,mat_end)) = regex.find(line).map(|mat| (mat.start(),mat.end())) else { continue };
							let mat_start = self.find_index_for_byte_pos(mat_start).unwrap();
							let mat_end = self.find_index_for_byte_pos(mat_end).unwrap();
							let real_start = start + mat_start;
							let real_end = start + mat_end;
							self.replace_range(real_start,real_end, new);
						}
					}
					// Now we put it back
					self.last_substitution = Some(sub);
				}
			}
			Verb::Substitute(old, new, flags) => {
				let (start_line,end_line) = match motion {
					MotionKind::Line(n) => (n,n),
					MotionKind::LineRange(s,e) => (s,e),
					_ => (0,self.total_lines()),
				};
				match Regex::new(&old) {
					Ok(regex) => {
						// We go in reverse here
						let lines = (start_line..=end_line).rev();
						for line_no in lines {
							let Some((start,end)) = self.line_bounds(line_no) else { continue };
							let line = self.slice(start..end).unwrap_or_default();
							let global = flags.contains(SubFlags::GLOBAL);
							if global {
								let line_matches = regex
									.find_iter(line)
									.map(|mat| (mat.start(),mat.end()))
									.collect::<Vec<_>>()
									.into_iter()
									.rev();
								for (mat_start,mat_end) in line_matches {
									let mat_start = self.find_index_for_byte_pos(mat_start).unwrap();
									let mat_end = self.find_index_for_byte_pos(mat_end).unwrap();
									let real_start = start + mat_start;
									let real_end = start + mat_end;
									self.replace_range(real_start,real_end, &new);
								}
							} else {
								let Some((mat_start,mat_end)) = regex.find(line).map(|mat| (mat.start(),mat.end())) else { continue };
								let mat_start = self.find_index_for_byte_pos(mat_start).unwrap();
								let mat_end = self.find_index_for_byte_pos(mat_end).unwrap();
								let real_start = start + mat_start;
								let real_end = start + mat_end;
								self.replace_range(real_start,real_end, &new);
							}
						}
						self.last_substitution = Some((regex,new,flags));
					}
					Err(e) => {
						eprintln!("vicut: {e}");
						std::process::exit(1);
					}
				}
			}
			Verb::ExMode |
			Verb::Complete |
			Verb::Normal(_) |
			Verb::EndOfFile |
			Verb::InsertMode |
			Verb::NormalMode |
			Verb::VisualMode |
			Verb::RepeatLast |
			Verb::ReplaceMode |
			Verb::VisualModeLine |
			Verb::VisualModeBlock |
			Verb::CompleteBackward |
			Verb::SearchMode(_, _) |
			Verb::AcceptLineOrNewline |
			Verb::VisualModeSelectLast => self.apply_motion(motion), // Already handled logic for these in exec.rs
		}
		Ok(())
	}
	pub fn exec_cmd(&mut self, cmd: ViCmd) -> Result<(),String> {
		let clear_redos = !cmd.is_undo_op() || cmd.verb.as_ref().is_some_and(|v| v.1.is_edit());
		let is_char_insert = cmd.verb.as_ref().is_some_and(|v| v.1.is_char_insert());
		let is_line_motion = cmd.is_line_motion();
		let is_undo_op = cmd.is_undo_op();
		let edit_is_merging = self.undo_stack.last().is_some_and(|edit| edit.merging);

		// Merge character inserts into one edit
		if edit_is_merging
			&& cmd.verb.as_ref().is_none_or(|v| !v.1.is_char_insert()) {
				if let Some(edit) = self.undo_stack.last_mut() {
					edit.stop_merge();
				}
		}

		let ViCmd { register, verb, motion, flags, raw_seq: _ } = cmd;

		let verb_cmd_ref = verb.as_ref();
		let verb_ref = verb_cmd_ref.map(|v| v.1.clone());

		let before = self.buffer.clone();
		let cursor_pos = self.cursor.get();

		/*
		 * Let's evaluate the motion now
		 * If we got some weird command like 'dvw' we will have to simulate a visual selection to get the range
		 * If motion is None, we will try to use self.select_range
		 * If self.select_range is None, we will use MotionKind::Null
		 */
		let motion_eval = if flags.intersects(CmdFlags::VISUAL | CmdFlags::VISUAL_LINE | CmdFlags::VISUAL_BLOCK) {
			let motion = motion
				.clone()
				.map(|m| self.eval_motion(verb_ref.as_ref(), m))
				.unwrap_or(MotionKind::Null);
			let flag_intersection = flags.intersection(CmdFlags::VISUAL | CmdFlags::VISUAL_LINE | CmdFlags::VISUAL_BLOCK);
			let mode = match flag_intersection {
				CmdFlags::VISUAL => SelectMode::Char(SelectAnchor::Start),
				CmdFlags::VISUAL_LINE => SelectMode::Line(SelectAnchor::Start),
				CmdFlags::VISUAL_BLOCK => self.get_block_select(),
				_ => unreachable!()
			};
			// Start a selection
			self.start_selecting(mode);
			// Apply the cursor motion
			self.apply_motion(motion);

			// Use the selection range created by the motion
			self.select_range
				.clone()
				.map(MotionKind::from_select_range)
				.unwrap_or(MotionKind::Null)
		} else {
			motion
				.clone()
				.map(|m| self.eval_motion(verb_ref.as_ref(), m))
				.unwrap_or({
					self.select_range
						.clone()
						.map(MotionKind::from_select_range)
						.unwrap_or(MotionKind::Null)
				})
		};

		if let Some(verb) = verb.clone() {
			self.exec_verb(verb.1, motion_eval, register)?;
		} else {
			self.apply_motion(motion_eval);
		}

		/* Done executing, do some cleanup */

		let after = self.buffer.clone();
		if clear_redos {
			self.redo_stack.clear();
		}

		if before != after {
			if !is_undo_op {
				self.handle_edit(before, after, cursor_pos);
			}
			/*
			 * The buffer has been edited,
			 * which invalidates the grapheme_indices vector
			 * We set it to None now, so that self.update_graphemes_lazy()
			 * will update it when it is needed again
			 */
			self.update_graphemes();
		}

		if !is_line_motion {
			self.saved_col = None;
		}

		if is_char_insert {
			if let Some(edit) = self.undo_stack.last_mut() {
				edit.start_merge();
			}
		}

		if self.grapheme_at_cursor().is_some_and(|gr| gr == "\n")
			&& self.grapheme_before_cursor().is_some_and(|gr| gr != "\n")
			&& self.cursor.exclusive {
				self.cursor.sub(1); // push it off the newline
		}

		Ok(())
	}
	pub fn as_str(&self) -> &str {
		&self.buffer // FIXME: this will have to be fixed up later
	}
}

/// Rotate alphabetic characters by 13 alphabetic positions
pub fn rot13(input: &str) -> String {
	input.chars()
		.map(|c| {
			if c.is_ascii_lowercase() {
				let offset = b'a';
				(((c as u8 - offset + 13) % 26) + offset) as char
			} else if c.is_ascii_uppercase() {
				let offset = b'A';
				(((c as u8 - offset + 13) % 26) + offset) as char
			} else {
				c
			}
		}).collect()
}

/// Ensure that the start is always less than or equal to the end
///
/// This is useful for creating ranges where the start and end positions
/// might be in any order, such as when dealing with cursor movements
pub fn ordered(start: usize, end: usize) -> (usize,usize) {
	if start > end {
		(end,start)
	} else {
		(start,end)
	}
}

pub fn ordered_signed(start: isize, end: isize) -> (isize,isize) {
	if start > end {
		(end,start)
	} else {
		(start,end)
	}
}
