use std::{collections::HashMap, fmt::Display, rc::Rc, str::FromStr};

use crate::{exec::ViCut, linebuf::ClampedUsize, register::{read_register, write_register, RegisterContent}, vic::{error::VicErr, parse::{RcVal, Val}}, vicmd::{Bound, Word}};

#[derive(Debug, Clone)]
pub enum Func {
	Method(String),
	Print,
	Json,
	TypeOf,
	Env,
	Format
}

#[derive(Debug, Clone)]
pub enum Var {
	Col,
	Line,
	Lines,
	Pos,
	Byte,
	BufLen,
	Selection,
	Buffer,
	Word,
	BigWord,
	IsEndofFile,
	IsEndofLine,
	IsStartofFile,
	IsStartofLine,
	Char,
}

#[derive(Debug, Clone)]
pub enum Builtin {
	Fn(Func),
	Var(Var)
}

impl FromStr for Builtin {
	type Err = VicErr;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"type_of" => Ok(Builtin::Fn(Func::TypeOf)),
			"json" => Ok(Builtin::Fn(Func::Json)),
			"env" => Ok(Builtin::Fn(Func::Env)),
			"print" => Ok(Builtin::Fn(Func::Print)),
			"format" => Ok(Builtin::Fn(Func::Format)),
			"_col" => Ok(Builtin::Var(Var::Col)),
			"_line" => Ok(Builtin::Var(Var::Line)),
			"_lines" => Ok(Builtin::Var(Var::Lines)),
			"_pos" => Ok(Builtin::Var(Var::Pos)),
			"_byte" => Ok(Builtin::Var(Var::Byte)),
			"_buf_len" => Ok(Builtin::Var(Var::BufLen)),
			"_selection" => Ok(Builtin::Var(Var::Selection)),
			"_buffer" => Ok(Builtin::Var(Var::Buffer)),
			"_word" => Ok(Builtin::Var(Var::Word)),
			"_WORD" => Ok(Builtin::Var(Var::BigWord)),
			"_is_eof" => Ok(Builtin::Var(Var::IsEndofFile)),
			"_is_eol" => Ok(Builtin::Var(Var::IsEndofLine)),
			"_is_sof" => Ok(Builtin::Var(Var::IsStartofFile)),
			"_is_sol" => Ok(Builtin::Var(Var::IsStartofLine)),
			"_char" => Ok(Builtin::Var(Var::Char)),
			_ => Err(VicErr::Simple(format!("Unknown built-in: {s}")))
		}
	}
}

impl Display for Builtin {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Builtin::Fn(func) => write!(f, "{func}"),
			Builtin::Var(var) => write!(f, "{var}"),
		}
	}
}

impl Display for Func {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Func::Method(_) => write!(f, "method"),
			Func::TypeOf => write!(f, "type_of"),
			Func::Json => write!(f, "json"),
			Func::Env => write!(f, "env"),
			Func::Print => write!(f, "print"),
			Func::Format => write!(f, "format"),
		}
	}
}
impl Display for Var {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Var::Col => write!(f, "_col"),
			Var::Line => write!(f, "_line"),
			Var::Lines => write!(f, "_lines"),
			Var::Pos => write!(f, "_pos"),
			Var::Byte => write!(f, "_byte"),
			Var::BufLen => write!(f, "_buf_len"),
			Var::Selection => write!(f, "_selection"),
			Var::Buffer => write!(f, "_buffer"),
			Var::Word => write!(f, "_word"),
			Var::BigWord => write!(f, "_WORD"),
			Var::IsEndofFile => write!(f, "_is_eof"),
			Var::IsEndofLine => write!(f, "_is_eol"),
			Var::IsStartofFile => write!(f, "_is_sof"),
			Var::IsStartofLine => write!(f, "_is_sol"),
			Var::Char => write!(f, "_char"),
		}
	}
}

impl ViCut {
	pub fn init_builtins() -> HashMap<String, Val> {
		let mut builtins = HashMap::new();
		builtins.insert("type_of".to_string(), Val::BuiltinHandle(Builtin::Fn(Func::TypeOf)));
		builtins.insert("json".to_string(), Val::BuiltinHandle(Builtin::Fn(Func::Json)));
		builtins.insert("env".to_string(), Val::BuiltinHandle(Builtin::Fn(Func::Env)));
		builtins.insert("print".to_string(), Val::BuiltinHandle(Builtin::Fn(Func::Print)));
		builtins.insert("format".to_string(), Val::BuiltinHandle(Builtin::Fn(Func::Format)));

		builtins.insert("_col".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Col)));
		builtins.insert("_line".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Line)));
		builtins.insert("_lines".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Lines)));
		builtins.insert("_pos".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Pos)));
		builtins.insert("_byte".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Byte)));
		builtins.insert("_buf_len".to_string(), Val::BuiltinHandle(Builtin::Var(Var::BufLen)));
		builtins.insert("_selection".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Selection)));
		builtins.insert("_buffer".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Buffer)));
		builtins.insert("_word".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Word)));
		builtins.insert("_WORD".to_string(), Val::BuiltinHandle(Builtin::Var(Var::BigWord)));
		builtins.insert("_is_eof".to_string(), Val::BuiltinHandle(Builtin::Var(Var::IsEndofFile)));
		builtins.insert("_is_eol".to_string(), Val::BuiltinHandle(Builtin::Var(Var::IsEndofLine)));
		builtins.insert("_is_sof".to_string(), Val::BuiltinHandle(Builtin::Var(Var::IsStartofFile)));
		builtins.insert("_is_sol".to_string(), Val::BuiltinHandle(Builtin::Var(Var::IsStartofLine)));
		builtins.insert("_char".to_string(), Val::BuiltinHandle(Builtin::Var(Var::Char)));

		builtins
	}
	pub fn try_builtin_function(&mut self, name: Func, args: Rc<[Val]>) -> Result<Val,VicErr> {
		match name {
			Func::Method(_) => unreachable!(),
			Func::TypeOf => {
				if args.len() != 1 {
					return Err(VicErr::Simple("type_of expects exactly one argument".to_string()))
				}
				let arg = &args[0];
				Ok(Val::new_str(arg.display_type()))
			}
			Func::Json => {
				if args.len() != 1 {
					return Err(VicErr::Simple("json expects exactly one argument".to_string()))
				}
				let arg = &args[0];
				match *arg {
					Val::Arr(_) | Val::Dict(_) => {
						// Correct
					}
					_ => {
						return Err(VicErr::Simple(format!("Expected array or dictionary in json(), got {}", arg.display_type())))
					}
				}
				let raw = arg.to_string();
				let json_str = serde_json::to_string(&raw).map_err(|e| VicErr::Simple(format!("Failed to serialize to JSON: {e}")))?;
				Ok(Val::new_str(json_str))
			}
			Func::Env => {
				if args.len() != 1 {
					return Err(VicErr::Simple("env expects exactly one argument".to_string()))
				}
				let arg = &args[0];
				let Val::Str(var_name) = arg else {
					return Err(VicErr::Simple(format!("Expected string in env(), got {}", arg.display_type())))
				};
				let var_name = var_name.borrow();
				let env_value = std::env::var(var_name.trim()).unwrap_or_default();
				Ok(Val::new_str(env_value).into())
			}
			Func::Print => {
				let mut output = String::new();
				for arg in args.iter() {
					let mut arg_clone: Val = arg.deep_clone().into();
					self.deep_eval_value(&mut arg_clone)?;
					let arg_str = arg_clone.to_string();
					output.push_str(&arg_str);
				}
				println!("{output}");
				Ok(Val::Null.into())
			}
			Func::Format => {
				let mut format = String::new();
				for arg in args.iter() {
					let arg_str = arg.to_string();
					format.push_str(&arg_str);
				}
				Ok(Val::new_str(format))
			}
		}
	}
	pub fn get_builtin_var(&mut self, name: Var) -> Option<Val> {
		let cur_buf = self.current_buffer_mut();

		Some(match name {
			Var::Col => Val::Num((cur_buf.cursor_col() + 1) as isize),
			Var::Line => Val::Num((cur_buf.cursor_line_number() + 1) as isize),
			Var::Lines => Val::Num(cur_buf.total_lines() as isize),
			Var::Pos => Val::Num(cur_buf.cursor.get() as isize),
			Var::Byte => Val::Num(cur_buf.cursor_byte_pos() as isize),
			Var::BufLen => Val::Num(cur_buf.buffer.len() as isize),
			Var::Selection => Val::new_str(cur_buf.selected_content().unwrap_or_default()),
			Var::Buffer => Val::new_str(cur_buf.buffer.clone()),
			Var::Word => {
				let (word_start,word_end) = cur_buf.text_obj_word(1, Bound::Inside, Word::Normal).unwrap_or_default();
				let word_end = ClampedUsize::new(word_end, cur_buf.cursor.cap(), false).ret_add(1);
				cur_buf.slice_inclusive(word_start..=word_end)
					.map(|slice| Val::new_str(slice.to_string()))
					.unwrap_or(Val::new_str(String::new()))
			}
			Var::BigWord => {
				let (big_word_start,big_word_end) = cur_buf.text_obj_word(1, Bound::Inside, Word::Big).unwrap_or_default();
				let big_word_end = ClampedUsize::new(big_word_end, cur_buf.cursor.cap(), false).ret_add(1);
				cur_buf.slice_inclusive(big_word_start..=big_word_end)
					.map(|slice| Val::new_str(slice.to_string()))
					.unwrap_or(Val::new_str(String::new()))
			}
			Var::IsEndofFile => Val::Bool(cur_buf.cursor_at_max()).into(),
			Var::IsEndofLine => Val::Bool(cur_buf.cursor_at_eol()).into(),
			Var::IsStartofFile => Val::Bool(cur_buf.cursor.get() == 0).into(),
			Var::IsStartofLine => Val::Bool(cur_buf.cursor.get() == 0).into(),
			Var::Char => {
				cur_buf.grapheme_at_cursor()
					.map(|gr| Val::new_str(gr.to_string()))
					.unwrap_or(Val::new_str(String::new()))
			}
		})
	}
	pub fn set_builtin_var(&mut self, name: Var, value: Val) -> Result<(), VicErr> {
		let cur_buf = self.current_buffer_mut();

		match name {
			Var::Col => {
				let Ok(col) = value.to_string().parse::<usize>() else {
					return Err(VicErr::Simple("Column must be a non-negative integer".to_string()));
				};
				let line_no = cur_buf.cursor_line_number();
				let Some((start,_)) = cur_buf.line_bounds(line_no) else {
					return Err(VicErr::Simple(format!("Line {} does not exist", cur_buf.cursor_line_number())));
				};
				cur_buf.cursor.set(start + col);
			},
			Var::Line => {
				let Ok(line) = value.to_string().parse::<usize>() else {
					return Err(VicErr::Simple("Line must be a non-negative integer".to_string()));
				};
				let Some((start,_)) = cur_buf.line_bounds(line) else {
					return Err(VicErr::Simple(format!("Line {} does not exist", line)));
				};
				cur_buf.cursor.set(start);
			},
			Var::Pos => {
				let Ok(pos) = value.to_string().parse::<usize>() else {
					return Err(VicErr::Simple("Position must be a non-negative integer".to_string()));
				};
				cur_buf.cursor.set(pos);
			},
			_ => return Err(VicErr::Simple(format!("Cannot set built-in variable {}", name))),
		}
		Ok(())
	}
	pub fn dispatch_builtin_handle(&mut self, handle: Builtin, args: Rc<[Val]>) -> Result<Val,VicErr> {
		match handle {
			Builtin::Fn(func) => self.try_builtin_function(func, args),
			Builtin::Var(var) => {
				Ok(self.get_builtin_var(var).unwrap())
			}
		}
	}
	pub fn dispatch_builtin_method(&mut self, self_val: &mut Val, method_name: &str, args: Rc<[Val]>) -> Result<Val,VicErr> {
		match self_val {
			Val::Str(_) => self.string_builtins(self_val, method_name, args),
			Val::Arr(_) => self.arr_builtins(self_val, method_name, args),
			Val::Num(_) => self.num_builtins(self_val, method_name, args),
			Val::Register(_) => self.register_builtins(self_val, method_name, args),
			Val::Err(_, _) => self.error_builtins(self_val, method_name, args),
			Val::Closure(_, _) => todo!(),
			Val::Dict(_) => todo!(),
			Val::Bool(_) => todo!(),
			Val::Regex(_) => todo!(),
			Val::Expr(_) => todo!(),
			Val::Ref(val) => {
				unsafe {
					let val_ptr = &mut (***val);
					self.dispatch_builtin_method(val_ptr, method_name, args)
				}
			}
			Val::BuiltinHandle(Builtin::Var(Var::Buffer)) => self.buffer_builtins(method_name, args),
			_ => Err(VicErr::Simple(format!("Cannot call method '{}' on value of type {}", method_name, self_val.display_type()))),
		}
	}
	fn error_builtins(&mut self, self_val: &mut Val, method_name: &str, args: Rc<[Val]>) -> Result<Val, VicErr> {
		match method_name {
			"unpack" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("unpack does not take any arguments".to_string()));
				}
				let Val::Err(_, msg) = self_val else { unreachable!() };

				Ok(msg.borrow().deep_clone().into())
			}
			_ => Err(VicErr::Simple(format!("Unknown error method: {method_name}"))),
		}
	}
	fn buffer_builtins(&mut self, method_name: &str, args: Rc<[Val]>) -> Result<Val, VicErr> {
		match method_name {
			_ => Err(VicErr::Simple(format!("Unknown buffer method: {method_name}"))),
		}
	}
	fn register_builtins(&mut self, self_val: &mut Val, method_name: &str, args: Rc<[Val]>) -> Result<Val, VicErr> {
		match method_name {
			"yank" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("yank takes exactly one argument".to_string()));
				}
				let content = RegisterContent::Span(args[0].to_string());
				let Val::Register(reg) = self_val else { unreachable!() };
				write_register(Some(*reg), content);
				Ok(Val::Null.into())
			}
			"put" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("put does not take any arguments".to_string()));
				}
				let Val::Register(reg) = self_val else { unreachable!() };
				let content = read_register(Some(*reg)).unwrap_or_default().to_string();
				Ok(Val::new_str(content).into())
			}
			_ => Err(VicErr::Simple(format!("Unknown register method: {}", method_name))),
		}
	}
	fn num_builtins(&mut self, self_val: &mut Val, method_name: &str, args: Rc<[Val]>) -> Result<Val, VicErr> {
		match method_name {
			"abs" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("abs does not take any arguments".to_string()));
				}
				if let Val::Num(num) = self_val {
					Ok(Val::Num(num.abs()).into())
				} else {
					Err(VicErr::Simple("abs can only be called on numbers".to_string()))
				}
			},
			"sqrt" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("sqrt does not take any arguments".to_string()));
				}
				if let Val::Num(num) = self_val {
					if *num < 0 {
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
	fn arr_builtins(&mut self, self_val: &mut Val, method_name: &str, args: Rc<[Val]>) -> Result<Val, VicErr> {
		match method_name {
			"len" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("len does not take any arguments".to_string()));
				}
				let len = match self_val {
					Val::Arr(arr) => arr.borrow().len(),
					_ => return Err(VicErr::Simple("len can only be called on arrays".to_string())),
				};
				Ok(Val::Num(len as isize).into())
			},
			"is_empty" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("is_empty does not take any arguments".to_string()));
				}
				let is_empty = match self_val {
					Val::Arr(arr) => arr.borrow().is_empty(),
					_ => return Err(VicErr::Simple("is_empty can only be called on arrays".to_string())),
				};
				Ok(Val::Bool(is_empty).into())
			},
			"push" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("push takes exactly one argument".to_string()));
				}
				if let Val::Arr(arr) = self_val {
					let mut arr = arr.borrow_mut();
					let val_copy = args[0].clone();
					arr.push_back(val_copy.into());
					Ok(Val::Null.into())
				} else {
					Err(VicErr::Simple("push can only be called on arrays".to_string()))
				}
			},
			"fpush" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("fpush takes exactly one argument".to_string()));
				}
				if let Val::Arr(arr) = self_val {
					let mut arr = arr.borrow_mut();
					let val_copy = args[0].clone();
					arr.push_front(val_copy.into());
					Ok(Val::Null.into())
				} else {
					Err(VicErr::Simple("fpush can only be called on arrays".to_string()))
				}
			}
			"fpop" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("fpop does not take any arguments".to_string()));
				}
				if let Val::Arr(arr) = self_val {
					let mut arr = arr.borrow_mut();
					if let Some(val) = arr.pop_front() {
						Ok(val)
					} else {
						Ok(Val::Null.into())
					}
				} else {
					Err(VicErr::Simple("fpop can only be called on arrays".to_string()))
				}
			},
			"pop" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("pop does not take any arguments".to_string()));
				}
				if let Val::Arr(arr) = self_val {
					let mut arr = arr.borrow_mut();
					if let Some(val) = arr.pop_back() {
						Ok(val)
					} else {
						Ok(Val::Null.into())
					}
				} else {
					Err(VicErr::Simple("pop can only be called on arrays".to_string()))
				}
			},
			"peek" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("peek does not take any arguments".to_string()));
				}
				if let Val::Arr(arr) = self_val {
					let mut arr = arr.borrow();
					if let Some(val) = arr.back() {
						Ok(val.clone())
					} else {
						Err(VicErr::Simple("peek called on an empty array".to_string()))
					}
				} else {
					Err(VicErr::Simple("peek can only be called on arrays".to_string()))
				}
			}
			"fpeek" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("fpeek does not take any arguments".to_string()));
				}
				if let Val::Arr(arr) = self_val {
					let mut arr = arr.borrow();
					if let Some(val) = arr.front() {
						Ok(val.clone())
					} else {
						Err(VicErr::Simple("fpeek called on an empty array".to_string()))
					}
				} else {
					Err(VicErr::Simple("fpeek can only be called on arrays".to_string()))
				}
			}
			_ => Err(VicErr::Simple(format!("Unknown method: {}", method_name))),
		}
	}
	fn string_builtins(&mut self, self_val: &mut Val, method_name: &str, args: Rc<[Val]>) -> Result<Val, VicErr> {
		match method_name {
			"len" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("len does not take any arguments".to_string()));
				}
				let len = match self_val {
					Val::Str(s) => s.borrow().len(),
					Val::Arr(arr) => arr.borrow().len(),
					_ => return Err(VicErr::Simple("len can only be called on strings or arrays".to_string())),
				};
				Ok(Val::Num(len as isize).into())
			},
			"push" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("push takes exactly one argument".to_string()));
				}
				if let Val::Str(s) = self_val {
					let mut s = s.borrow_mut();
					s.push_str(&args[0].to_string());
					Ok(Val::Null.into())
				} else {
					Err(VicErr::Simple("push can only be called on strings".to_string()))
				}
			},
			"pop" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("pop does not take any arguments".to_string()));
				}
				if let Val::Str(s) = self_val {
					let mut s = s.borrow_mut();
					if !s.is_empty() {
						let last_char = s.pop().unwrap();
						Ok(Val::new_str(last_char.to_string()).into())
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
				if let Val::Str(s) = self_val {
					let s = s.borrow();
					Ok(Val::new_str(s.to_uppercase()).into())
				} else {
					Err(VicErr::Simple("to_upper can only be called on strings".to_string()))
				}
			},
			"to_lower" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("to_lower does not take any arguments".to_string()));
				}
				if let Val::Str(s) = self_val {
					let s = s.borrow();
					Ok(Val::new_str(s.to_lowercase()).into())
				} else {
					Err(VicErr::Simple("to_lower can only be called on strings".to_string()))
				}
			},
			"trim" => {
				if !args.is_empty() {
					return Err(VicErr::Simple("trim does not take any arguments".to_string()));
				}
				if let Val::Str(s) = self_val {
					let s = s.borrow();
					Ok(Val::new_str(s.trim().to_string()).into())
				} else {
					Err(VicErr::Simple("trim can only be called on strings".to_string()))
				}
			},
			"trim_matches" => {
				if args.len() != 1 {
					return Err(VicErr::Simple("trim_matches takes exactly one argument".to_string()));
				}
				let arg = args[0].clone();
				if let Val::Str(s) = self_val {
					let s = s.borrow().to_string();
					let trimmed = match arg {
						Val::Str(ref pat_str) => s.trim_matches(|c| pat_str.borrow().contains(c)),
						Val::Arr(ref arr) => {
							let arr = arr.borrow();
							let chars: Option<Vec<char>> = arr.iter()
								.map(|v| {
									let s = v.to_string();
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
					Ok(Val::new_str(trimmed.to_string()).into())
				} else {
					Err(VicErr::Simple("trim_matches can only be called on strings".to_string()))
				}
			}
			_ => Err(VicErr::Simple(format!("Unknown string method: {method_name}"))),
		}
	}
}
