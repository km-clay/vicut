use crate::{exec::ViCut, linebuf::ClampedUsize, register::{read_register, write_register, RegisterContent}, vic::{error::VicErr, parse::{RcVal, Val}}, vicmd::{Bound, Word}};


impl ViCut {
	pub fn get_builtin_var(&mut self, name: &str) -> Option<RcVal> {
		let cur_buf = self.current_buffer_mut();

		if !Self::BUILTINS.contains(&name) {
			return None // Not a built-in variable
		}
		Some(match name {
			"_col" => Val::Num((cur_buf.cursor_col() + 1) as isize).into(),
			"_line" => Val::Num((cur_buf.cursor_line_number() + 1) as isize).into(),
			"_lines" => Val::Num(cur_buf.total_lines() as isize).into(),
			"_pos" => Val::Num(cur_buf.cursor.get() as isize).into(),
			"_byte" => Val::Num(cur_buf.cursor_byte_pos() as isize).into(),
			"_buf_len" => Val::Num(cur_buf.buffer.len() as isize).into(),
			"_selection" => Val::Str(cur_buf.selected_content().unwrap_or_default()).into(),
			"_buffer" => Val::Str(cur_buf.buffer.clone()).into(),
			"_word" => {
				let (word_start,word_end) = cur_buf.text_obj_word(1, Bound::Inside, Word::Normal).unwrap_or_default();
				let word_end = ClampedUsize::new(word_end, cur_buf.cursor.cap(), false).ret_add(1);
				cur_buf.slice_inclusive(word_start..=word_end)
					.map(|slice| Val::Str(slice.to_string()).into())
					.unwrap_or(Val::Str(String::new()).into())
			}
			"_WORD" => {
				let (big_word_start,big_word_end) = cur_buf.text_obj_word(1, Bound::Inside, Word::Big).unwrap_or_default();
				let big_word_end = ClampedUsize::new(big_word_end, cur_buf.cursor.cap(), false).ret_add(1);
				cur_buf.slice_inclusive(big_word_start..=big_word_end)
					.map(|slice| Val::Str(slice.to_string()).into())
					.unwrap_or(Val::Str(String::new()).into())
			}
			"_is_eof" => Val::Bool(cur_buf.cursor_at_max()).into(),
			"_is_eol" => Val::Bool(cur_buf.cursor_at_eol()).into(),
			"_is_sof" => Val::Bool(cur_buf.cursor.get() == 0).into(),
			"_is_sol" => Val::Bool(cur_buf.cursor.get() == 0).into(),
			"_char" => {
				cur_buf.grapheme_at_cursor()
					.map(|gr| Val::Str(gr.to_string()).into())
					.unwrap_or(Val::Str(String::new()).into())
			}
			_ => unreachable!()
		})
	}
	pub fn set_builtin_var(&mut self, name: &str, value: RcVal) -> Result<(), VicErr> {
		let cur_buf = self.current_buffer_mut();

		if !Self::BUILTINS.contains(&name) {
			return Err(VicErr::Simple(format!("{} is not a built-in variable", name)));
		}
		match name {
			"_col" => {
				let Ok(col) = value.borrow().to_string().parse::<usize>() else {
					return Err(VicErr::Simple("Column must be a non-negative integer".to_string()));
				};
				let line_no = cur_buf.cursor_line_number();
				let Some((start,_)) = cur_buf.line_bounds(line_no) else {
					return Err(VicErr::Simple(format!("Line {} does not exist", cur_buf.cursor_line_number())));
				};
				cur_buf.cursor.set(start + col);
			},
			"_line" => {
				let Ok(line) = value.borrow().to_string().parse::<usize>() else {
					return Err(VicErr::Simple("Line must be a non-negative integer".to_string()));
				};
				let Some((start,_)) = cur_buf.line_bounds(line) else {
					return Err(VicErr::Simple(format!("Line {} does not exist", line)));
				};
				cur_buf.cursor.set(start);
			},
			"_pos" => {
				let Ok(pos) = value.borrow().to_string().parse::<usize>() else {
					return Err(VicErr::Simple("Position must be a non-negative integer".to_string()));
				};
				cur_buf.cursor.set(pos);
			},
			_ => return Err(VicErr::Simple(format!("Cannot set built-in variable {}", name))),
		}
		Ok(())
	}
	pub fn dispatch_builtin_method(&mut self, self_val: RcVal, method_name: &str, args: Vec<RcVal>) -> Result<RcVal,VicErr> {
		let inner = self_val.borrow().clone();
		match inner {
			Val::Str(_) => self.string_builtins(self_val.clone(), method_name, args),
			Val::Arr(_) => self.arr_builtins(self_val.clone(), method_name, args),
			Val::Num(_) => self.num_builtins(self_val.clone(), method_name, args),
			Val::Register(_) => self.register_builtins(self_val.clone(), method_name, args),
			Val::Closure(_, _) => todo!(),
			Val::Dict(_) => todo!(),
			Val::Bool(_) => todo!(),
			Val::Regex(_) => todo!(),
			Val::Expr(_) => todo!(),
			Val::BufferHandle => self.buffer_builtins(method_name, args),
			_ => Err(VicErr::Simple(format!("Cannot call method '{}' on value of type {}", method_name, inner.display_type()))),
		}
	}
	fn buffer_builtins(&mut self, method_name: &str, args: Vec<RcVal>) -> Result<RcVal, VicErr> {
		match method_name {
			_ => Err(VicErr::Simple(format!("Unknown buffer method: {method_name}"))),
		}
	}
	fn register_builtins(&mut self, self_val: RcVal, method_name: &str, args: Vec<RcVal>) -> Result<RcVal, VicErr> {
		match method_name {
			"yank" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("yank takes exactly one argument".to_string()));
				}
				let content = RegisterContent::Span(args[0].borrow().to_string());
				let Val::Register(reg) = *self_val.borrow() else { unreachable!() };
				write_register(Some(reg), content);
				Ok(Val::Null.into())
			}
			"put" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("put does not take any arguments".to_string()));
				}
				let Val::Register(reg) = *self_val.borrow() else { unreachable!() };
				let content = read_register(Some(reg)).unwrap_or_default().to_string();
				Ok(Val::Str(content).into())
			}
			_ => Err(VicErr::Simple(format!("Unknown register method: {}", method_name))),
		}
	}
	fn num_builtins(&mut self, self_val: RcVal, method_name: &str, args: Vec<RcVal>) -> Result<RcVal, VicErr> {
		match method_name {
			"abs" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("abs does not take any arguments".to_string()));
				}
				if let Val::Num(num) = *self_val.borrow() {
					Ok(Val::Num(num.abs()).into())
				} else {
					Err(VicErr::Simple("abs can only be called on numbers".to_string()))
				}
			},
			"sqrt" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("sqrt does not take any arguments".to_string()));
				}
				if let Val::Num(num) = *self_val.borrow() {
					if num < 0 {
						return Err(VicErr::Simple("Cannot compute square root of a negative number".to_string()));
					}
					Ok(Val::Num(num.isqrt()).into())
				} else {
					Err(VicErr::Simple("sqrt can only be called on numbers".to_string()))
				}
			},
			_ => Err(VicErr::Simple(format!("Unknown method: {}", method_name))),
		}
	}
	fn arr_builtins(&mut self, self_val: RcVal, method_name: &str, args: Vec<RcVal>) -> Result<RcVal, VicErr> {
		match method_name {
			"len" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("len does not take any arguments".to_string()));
				}
				let len = match *self_val.borrow() {
					Val::Arr(ref arr) => arr.len(),
					_ => return Err(VicErr::Simple("len can only be called on arrays".to_string())),
				};
				Ok(Val::Num(len as isize).into())
			},
			"push" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("push takes exactly one argument".to_string()));
				}
				if let Val::Arr(ref mut arr) = *self_val.borrow_mut() {
					let val_copy = args[0].borrow().clone();
					arr.push(val_copy.into());
					Ok(Val::Null.into())
				} else {
					Err(VicErr::Simple("push can only be called on arrays".to_string()))
				}
			},
			"pop" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("pop does not take any arguments".to_string()));
				}
				if let Val::Arr(ref mut arr) = *self_val.borrow_mut() {
					if let Some(val) = arr.pop() {
						Ok(val)
					} else {
						Err(VicErr::Simple("pop called on an empty array".to_string()))
					}
				} else {
					Err(VicErr::Simple("pop can only be called on arrays".to_string()))
				}
			},
			_ => Err(VicErr::Simple(format!("Unknown method: {}", method_name))),
		}
	}
	fn string_builtins(&mut self, self_val: RcVal, method_name: &str, args: Vec<RcVal>) -> Result<RcVal, VicErr> {
		match method_name {
			"len" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("len does not take any arguments".to_string()));
				}
				let len = match *self_val.borrow() {
					Val::Str(ref s) => s.len(),
					Val::Arr(ref arr) => arr.len(),
					_ => return Err(VicErr::Simple("len can only be called on strings or arrays".to_string())),
				};
				Ok(Val::Num(len as isize).into())
			},
			"push" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("push takes exactly one argument".to_string()));
				}
				if let Val::Str(ref mut s) = *self_val.borrow_mut() {
					s.push_str(&args[0].borrow().to_string());
					Ok(Val::Null.into())
				} else {
					Err(VicErr::Simple("push can only be called on strings".to_string()))
				}
			},
			"pop" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("pop does not take any arguments".to_string()));
				}
				if let Val::Str(ref mut s) = *self_val.borrow_mut() {
					if !s.is_empty() {
						let last_char = s.pop().unwrap();
						Ok(Val::Str(last_char.to_string()).into())
					} else {
						Err(VicErr::Simple("pop called on an empty string".to_string()))
					}
				} else {
					Err(VicErr::Simple("pop can only be called on strings".to_string()))
				}
			}
			"to_upper" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("to_upper does not take any arguments".to_string()));
				}
				if let Val::Str(ref s) = *self_val.borrow() {
					Ok(Val::Str(s.to_uppercase()).into())
				} else {
					Err(VicErr::Simple("to_upper can only be called on strings".to_string()))
				}
			},
			"to_lower" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("to_lower does not take any arguments".to_string()));
				}
				if let Val::Str(ref s) = *self_val.borrow() {
					Ok(Val::Str(s.to_lowercase()).into())
				} else {
					Err(VicErr::Simple("to_lower can only be called on strings".to_string()))
				}
			},
			"trim" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("trim does not take any arguments".to_string()));
				}
				if let Val::Str(ref s) = *self_val.borrow() {
					Ok(Val::Str(s.trim().to_string()).into())
				} else {
					Err(VicErr::Simple("trim can only be called on strings".to_string()))
				}
			},
			"trim_matches" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("trim_matches takes exactly one argument".to_string()));
				}
				let arg = args[0].borrow().clone();
				if let Val::Str(ref s) = *self_val.borrow() {
					let s = s.to_string();
					let trimmed = match arg {
						Val::Str(ref pat_str) => s.trim_matches(|c| pat_str.contains(c)),
						Val::Arr(ref arr) => {
							let chars: Option<Vec<char>> = arr.iter()
								.map(|v| {
									let s = v.borrow().to_string();
									let mut chars = s.chars();
									match (chars.next(), chars.next()) {
										(Some(c), None) => Some(c), // single char string
										_ => None
									}
								})
							.collect();
							if let Some(char_vec) = chars {
								s.trim_matches(&char_vec[..])
							} else {
								return Err(VicErr::Simple("All elements in array must be single-character strings".to_string()));
							}
						}
						_ => return Err(VicErr::Simple("Expected string or array of single-character strings".to_string())),
					};
					Ok(Val::Str(trimmed.to_string()).into())
				} else {
					Err(VicErr::Simple("trim_matches can only be called on strings".to_string()))
				}
			}
			_ => Err(VicErr::Simple(format!("Unknown string method: {method_name}"))),
		}
	}
}
