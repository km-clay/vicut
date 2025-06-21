use crate::linebuf::LineBuf;

#[test]
fn linebuf_empty_linebuf() {
	let mut buf = LineBuf::new();
	assert_eq!(buf.as_str(), "");
	buf.update_graphemes_lazy();
	assert!(buf.grapheme_indices().is_empty());
	assert!(buf.slice(0..0).is_none());
}

#[test]
fn linebuf_ascii_content() {
	let mut buf = LineBuf::new().with_initial("hello".into(), 0);

	buf.update_graphemes_lazy();
	assert_eq!(buf.grapheme_indices(), &[0, 1, 2, 3, 4]);

	assert_eq!(buf.grapheme_at(0), Some("h"));
	assert_eq!(buf.grapheme_at(4), Some("o"));
	assert_eq!(buf.slice(1..4), Some("ell"));
	assert_eq!(buf.slice_to(2), Some("he"));
	assert_eq!(buf.slice_from(2), Some("llo"));
}

#[test]
fn linebuf_unicode_graphemes() {
	let mut buf = LineBuf::new().with_initial("a🇺🇸b́c".into(), 0);

	buf.update_graphemes_lazy();
	let indices = buf.grapheme_indices();
	assert_eq!(indices.len(), 4); // 4 graphemes + 1 end marker

	assert_eq!(buf.grapheme_at(0), Some("a"));
	assert_eq!(buf.grapheme_at(1), Some("🇺🇸"));
	assert_eq!(buf.grapheme_at(2), Some("b́")); // b + combining accent
	assert_eq!(buf.grapheme_at(3), Some("c"));
	assert_eq!(buf.grapheme_at(4), None); // out of bounds

	assert_eq!(buf.slice(0..2), Some("a🇺🇸"));
	assert_eq!(buf.slice(1..3), Some("🇺🇸b́"));
	assert_eq!(buf.slice(2..4), Some("b́c"));
}

#[test]
fn linebuf_slice_to_from_cursor() {
	let mut buf = LineBuf::new().with_initial("abçd".into(), 2);

	buf.update_graphemes_lazy();
	assert_eq!(buf.slice_to_cursor(), Some("ab"));
	assert_eq!(buf.slice_from_cursor(), Some("çd"));
}

#[test]
fn linebuf_out_of_bounds_slices() {
	let mut buf = LineBuf::new().with_initial("test".into(), 0);

	buf.update_graphemes_lazy();

	assert_eq!(buf.grapheme_at(5), None); // out of bounds
	assert_eq!(buf.slice(2..5), None); // end out of bounds
	assert_eq!(buf.slice(4..4), None); // valid but empty
}

#[test]
fn linebuf_this_line() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial.into(), 57);
	let (start,end) = buf.this_line();
	assert_eq!(buf.slice(start..end), Some("This is the third line\n"))
}

#[test]
fn linebuf_prev_line() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial.into(), 57);
	let (start,end) = buf.nth_prev_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the second line\n"))
}

#[test]
fn linebuf_prev_line_first_line_is_empty() {
	let initial = "\nThis is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial.into(), 36);
	let (start,end) = buf.nth_prev_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the first line\n"))
}

#[test]
fn linebuf_next_line() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line";
	let mut buf = LineBuf::new().with_initial(initial.into(), 57);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the fourth line"))
}

#[test]
fn linebuf_next_line_last_line_is_empty() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line\n";
	let mut buf = LineBuf::new().with_initial(initial.into(), 57);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("This is the fourth line\n"))
}

#[test]
fn linebuf_next_line_several_trailing_newlines() {
	let initial = "This is the first line\nThis is the second line\nThis is the third line\nThis is the fourth line\n\n\n\n";
	let mut buf = LineBuf::new().with_initial(initial.into(), 81);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("\n"))
}

#[test]
fn linebuf_next_line_only_newlines() {
	let initial = "\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n";
	let mut buf = LineBuf::new().with_initial(initial.into(), 7);
	let (start,end) = buf.nth_next_line(1).unwrap();
	assert_eq!(start, 8);
	assert_eq!(buf.slice(start..end), Some("\n"))
}

#[test]
fn linebuf_prev_line_only_newlines() {
	let initial = "\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n";
	let mut buf = LineBuf::new().with_initial(initial.into(), 7);
	let (start,end) = buf.nth_prev_line(1).unwrap();
	assert_eq!(buf.slice(start..end), Some("\n"));
	assert_eq!(start, 6);
}

#[test]
fn linebuf_cursor_motion() {
	let mut buf = LineBuf::new().with_initial("Thé quíck 🦊 bröwn fóx jumpś óver the 💤 lázy dóg 🐶".into(), 0);

	buf.update_graphemes_lazy();
	let total = buf.grapheme_indices.as_ref().unwrap().len();

	for i in 0..total {
		buf.cursor.set(i);

		let expected_to = buf.buffer.get(..buf.grapheme_indices_owned()[i]).unwrap_or("").to_string();
		let expected_from = if i + 1 < total {
			buf.buffer.get(buf.grapheme_indices_owned()[i]..).unwrap_or("").to_string()
		} else {
			// last grapheme, ends at buffer end
			buf.buffer.get(buf.grapheme_indices_owned()[i]..).unwrap_or("").to_string()
		};

		let expected_at = {
			let start = buf.grapheme_indices_owned()[i];
			let end = buf.grapheme_indices_owned().get(i + 1).copied().unwrap_or(buf.buffer.len());
			buf.buffer.get(start..end).map(|slice| slice.to_string())
		};

		assert_eq!(
			buf.slice_to_cursor(),
			Some(expected_to.as_str()),
			"Failed at cursor position {i}: slice_to_cursor"
		);
		assert_eq!(
			buf.slice_from_cursor(),
			Some(expected_from.as_str()),
			"Failed at cursor position {i}: slice_from_cursor"
		);
		assert_eq!(
			buf.grapheme_at(i).map(|slice| slice.to_string()),
			expected_at,
			"Failed at cursor position {i}: grapheme_at"
		);
	}
}
