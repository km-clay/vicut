impl Argv {
	/// Parse the user's arguments
	pub fn parse() -> Result<Self,String> {
		let mut new = Self::default();
		let mut args = std::env::args().skip(1).peekable();
		while let Some(arg) = args.next() {
			match arg.as_str() {
				"--json" | "-j" => {
					new.json = true;
				}
				"--trace" => {
					new.trace = true;
				}
				"--linewise" => {
					new.linewise = true;
				}
				"--serial" => {
					new.single_thread = true;
				}
				"--trim-fields" => {
					new.trim_fields = true;
				}
				"--keep-mode" => {
					new.keep_mode = true;
				}
				"--backup" => {
					new.backup_files = true;
				}
				"-i" => {
					new.edit_inplace = true;
				}
				"--template" | "-t" => {
					let Some(next_arg) = args.next() else {
						return Err(format!("Expected a format string after '{arg}'"))
					};
					if next_arg.starts_with('-') {
						return Err(format!("Expected a format string after '{arg}', found {next_arg}"))
					}
					new.template = Some(next_arg)
				}
				"--delimiter" | "-d" => {
					let Some(next_arg) = args.next() else { continue };
					if next_arg.starts_with('-') {
						return Err(format!("Expected a delimiter after '{arg}', found {next_arg}"))
					}
					new.delimiter = Some(next_arg)
				}
				"-n" | "--next" => new.cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.map_err(|_| format!("Expected a number after '{arg}'"))?;
					let repeat_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.map_err(|_| format!("Expected a number after '{arg}'"))?;

					new.cmds.push(Cmd::Repeat(cmd_count, repeat_count));
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						return Err(format!("Expected a motion command after '-m', found {arg}"))
					}
					new.cmds.push(Cmd::Motion(arg))
				}
				"-c" | "--cut" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with("name=") {
						let name = arg.strip_prefix("name=").unwrap().to_string();
						if name == "0" {
							// We use '0' as a sentinel value to say "We didn't slice any fields, so this field is the entire buffer"
							// So we can't let people use it arbitrarily, or weird shit starts happening
							return Err("Field name '0' is a reserved field name.".into())
						}
						let Some(arg) = args.next() else { continue };
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::NamedField(name,arg));
					} else {
						if arg.starts_with('-') {
							return Err(format!("Expected a selection command after '-c', found {arg}"))
						}
						new.cmds.push(Cmd::Field(arg));
					}
				}
				"-v" | "--not-global" |
				"-g" | "--global" => {
					let global = new.handle_global_arg(arg.as_str(), &mut args);
					new.cmds.push(global);
				}
				_ => new.handle_filename(arg)
			}
		}
		Ok(new)
	}
	/// Handles `-g` and `-v` global conditionals.
	///
	/// `-g` and `-v` are special cases: each introduces a scoped block of commands
	/// that will only execute if a pattern match (or non-match) succeeds. These blocks
	/// can contain other nested `-g` or `-v` invocations, as well as `--else` branches
	/// and `-r` repeats.
	///
	/// Because of this recursive structure, we use a recursive descent parser to
	/// build a nested command execution tree from the input. This allows arbitrarily
	/// deep combinations of conditionals and scopes, like:
	///
	/// ```bash
	/// vicut -g 'foo' -g 'bar' -c 'd' --else -v 'baz' -c 'y' --end --end
	/// ```
	fn handle_global_arg(&mut self,arg: &str, args: &mut Peekable<Skip<Args>>) -> Cmd {
		let polarity = match arg {
			"-v" | "--not-global" => false,
			"-g" | "--global" => true,
			_ => unreachable!("found arg: {arg}")
		};
		let mut then_cmds = vec![];
		let mut else_cmds = None;
		let Some(arg) = args.next() else {
			return Cmd::Global {
				pattern: arg.into(),
				then_cmds,
				else_cmds,
				polarity
			};
		};
		if arg.starts_with('-') {
			eprintln!("Expected a selection command after '-c', found {arg}");
			std::process::exit(1)
		}
		while let Some(global_arg) = args.next() {
			match global_arg.as_str() {
				"-n" | "--next" => self.cmds.push(Cmd::BreakGroup),
				"-r" | "--repeat" => {
					let cmd_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or_else(|_| {
							eprintln!("Expected a number after '{global_arg}'");
							std::process::exit(1)
						});
					let repeat_count = args
						.next()
						.unwrap_or("1".into())
						.parse::<usize>()
						.unwrap_or_else(|_| {
							eprintln!("Expected a number after '{global_arg}'");
							std::process::exit(1)
						});

					if let Some(else_cmds) = else_cmds.as_mut() {
						else_cmds.push(Cmd::Repeat(cmd_count, repeat_count));
					} else {
						then_cmds.push(Cmd::Repeat(cmd_count, repeat_count));
					}
				}
				"-m" | "--move" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with('-') {
						eprintln!("Expected a motion command after '-m', found {arg}");
						std::process::exit(1);
					}
					if let Some(else_cmds) = else_cmds.as_mut() {
						else_cmds.push(Cmd::Motion(arg))
					} else {
						then_cmds.push(Cmd::Motion(arg))
					}
				}
				"-c" | "--cut" => {
					let Some(arg) = args.next() else { continue };
					if arg.starts_with("name=") {
						let name = arg.strip_prefix("name=").unwrap().to_string();
						let Some(arg) = args.next() else { continue };
						if arg.starts_with('-') {
							eprintln!("Expected a selection command after '-c', found {arg}");
							std::process::exit(1);
						}
						if let Some(cmds) = else_cmds.as_mut() {
							cmds.push(Cmd::NamedField(name,arg));
						} else {
							then_cmds.push(Cmd::NamedField(name,arg));
						}
					} else {
						if arg.starts_with('-') {
							eprintln!("Expected a selection command after '-c', found {arg}");
							std::process::exit(1);
						}
						if let Some(cmds) = else_cmds.as_mut() {
							cmds.push(Cmd::Field(arg));
						} else {
							then_cmds.push(Cmd::Field(arg));
						}
					}
				}
				"-g" | "--global" |
				"-v" | "--not-global" => {
					let nested = self.handle_global_arg(&global_arg, args);
					if let Some(cmds) = else_cmds.as_mut() {
						cmds.push(nested);
					} else {
						then_cmds.push(nested);
					}
				}
				"--else" => {
					// Now we start working on this
					else_cmds = Some(vec![]);
				}
				"--end" => {
					// We're done here
					return Cmd::Global {
						pattern: arg,
						then_cmds,
						else_cmds,
						polarity
					};
				}
				_ => {
					eprintln!("Expected command flag in '-g' scope\nDid you forget to close '-g' with '--end'?");
					std::process::exit(1);
				}
			}
			if args.peek().is_some_and(|arg| !arg.starts_with('-')) { break }
		}

		// If we got here, we have run out of arguments
		// Let's just submit the current -g commands.
		// no need to be pressed about a missing '--end' when nothing would come after it
		Cmd::Global {
			pattern: arg,
			then_cmds,
			else_cmds,
			polarity
		}
	}
	/// Handle a filename passed as an argument.
	///
	/// Checks to make sure the following invariants are met:
	/// 1. The path given exists.
	/// 2. The path given refers to a file.
	/// 3. The path given refers to a file that we are allowed to read.
	///
	/// We check all three separately instead of just the last one, so that we can give better error messages
	fn handle_filename(&mut self, filename: String) {
		let path = PathBuf::from(filename.trim().to_string());
		if !path.exists() {
			eprintln!("vicut: file not found '{}'",path.display());
			std::process::exit(1);
		}
		if !path.is_file() {
			eprintln!("vicut: '{}' is not a file",path.display());
			std::process::exit(1);
		}
		if fs::File::open(&path).is_err() {
			eprintln!("vicut: failed to read file '{}'",path.display());
			std::process::exit(1);
		}
		if !self.files.contains(&path) {
			self.files.push(path)
		}
	}
}

