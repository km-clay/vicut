// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
// Used to call the main logic internally
// Testing fixture
// Print help or version info and exit early if `--help` or `--version` are found
// Testing
// Testing fixture for the debug profile
// Simplest of the three routes.
// Default execution pathway. Operates on `stdin`.
// Operates on the content of the files, and either prints to stdout, or edits the files in-place
// Execution pathway for handling filenames given as arguments
// Output has already been handled
// Output has already been handled
// So using it in pool.install() doesn't work. We have to initialize it in the closure there.
// We need to initialize stream in each branch, since Box<dyn BufReader> does not implement send/sync
// Each route in this function operates on individual lines from the input
// The pathway for when the `--linewise` flag is set
// Pair each line with its original index
// Reads the complete input from stdin and then splits it into its lines for execution.
// This function is used for `--linewise` execution on stdin.
// Executes commands on lines from stdin, using multi-threaded processing
// Sort lines
// Write back to file
// Separate content by file
// Process each line's content
// Backup files are created if `--backup-files` is enabled.
// Errors during reading, transformation, or writing will abort the program with a diagnostic.
//     - Print to `stdout`, optionally prefixed by filename (`if multiple input files`)
//     - Write the result back to the original file (`-i is set`)
// 7. Reconstruct each file's contents and either:
// 6. Sort each file’s lines by line number to restore the original order.
// 5. Group the transformed lines by filename in a `BTreeMap`.
// 4. Use a parallel iterator to transform each line using `execute()`.
// 3. Tag each line with its originating filename and line number.
// 2. Combine all lines from all files into a single work pool.
// 1. Split each file into its lines.
// Steps:
// the full outputs in order.
// transforming each line independently using the `execute()` function and then reconstructing
// This function is used for `--linewise` execution. It processes all lines in parallel,
// Executes all input files line-by-line using multi-threaded processing.
// Write back to file
// Process each file's content
// 3. Decide how to handle output depending on whether args.edit_inplace is set.
// 2. Call `execute()` on each file's contents
// 1. Create a `work` vector containing a tuple of the file's path, and it's contents.
// The steps this function walks through are as follows:
// Multi-thread the execution of file input.
// -n
// -c name=<NAME> <VIM_CMDS>
// -c <VIM_CMDS>
// -m <VIM_CMDS>
// Negative branch
// Execute our commands
// Set the cursor on the start of the line
// Positive branch
// LineBuf::eval_motion() *always* returns MotionKind::Lines() for Motion::Global/NotGlobal.
// Here we ask ViCut's editor directly to evaluate the Global motion for us.
// -g/-v <PATTERN> <COMMANDS> [--else <COMMANDS>]
// We use recursion so that we can nest repeats easily
// -r <N> <R>
// Execute a single `Cmd`
// in each line. The newline characters are vital to `LineBuf`'s navigation logic.
// We use this instead of `String::lines()` because that method does not include the newline itself
// Split a string slice into it's lines.
// Trim the fields 🧑‍🌾
// want to see that output, with or without globals.
// But if the files vector is empty, the user is working on stdin, so they will probably
// We don't want to spam the output with entire files with no matches in that case,
// that the user is probably searching for something, potentially in a group of files.
// if args has files it is working on, and the command list has a global, that means
// fmt_lines is empty, so the user didn't write any -c commands
// Let's figure out if we want to print the whole buffer
// Next we loop over `args.cmds` and execute each one in sequence.
// Here we are going to initialize a new instance of `ViCut` to manage state for editing this input
// Execute the user's commands.
// The loop looks for patterns like {{1}} or {{foo}} to interpolate on
// We use a state machine here to interpolate the fields
// Format the output according to the given format string
// Push the new string
// Also clear fields for the next line
// Join the fields by the delimiter
// So let's double pop the 2d vector and grab the value of our only field
// We performed len checks in no_fields_extracted(), so unwrap is safe
// Let's check to see if we are outputting the whole buffer
// If we did extract some fields, we print each record one at a time, and each field will be separated by `delimiter`
// If we didn't extract any fields, we do our best to preserve the formatting of the original input
// Perform standard output formatting.
// This can be depended on, since `"0"` is a reserved field name that cannot be set by user input.
// Checks for the `"0"` field name, which is a sentinel value that says "We didn't get any `-c` commands"
// Check to see if we didn't explicitly extract any fields
// Format the output as JSON
// `lines` is a two-dimensional vector of tuples, each representing a key/value pair for extract fields.
// Format the stuff we extracted according to user specification
// If `trace` is true, then trace!() calls always activate, with our custom formatting.
// This interacts with the `--trace` flag that can be passed in the arguments.
// Initialize the logger
// Prints out the help info for `vicut`
// "Get some help" - Michael Jordan
/// We check all three separately instead of just the last one, so that we can give better error messages
/// 3. The path given refers to a file that we are allowed to read.
/// 2. The path given refers to a file.
/// 1. The path given exists.
/// Checks to make sure the following invariants are met:
/// Handle a filename passed as an argument.
// no need to be pressed about a missing '--end' when nothing would come after it
// Let's just submit the current -g commands.
// If we got here, we have run out of arguments
// We're done here
// Now we start working on this
/// ```
/// vicut -g 'foo' -g 'bar' -c 'd' --else -v 'baz' -c 'y' --end --end
/// ```bash
/// deep combinations of conditionals and scopes, like:
/// build a nested command execution tree from the input. This allows arbitrarily
/// Because of this recursive structure, we use a recursive descent parser to
/// and `-r` repeats.
/// can contain other nested `-g` or `-v` invocations, as well as `--else` branches
/// that will only execute if a pattern match (or non-match) succeeds. These blocks
/// `-g` and `-v` are special cases: each introduces a scoped block of commands
/// Handles `-g` and `-v` global conditionals.
// So we can't let people use it arbitrarily, or weird shit starts happening
// We use '0' as a sentinel value to say "We didn't slice any fields, so this field is the entire buffer"
/// Parse the user's arguments
// The arguments passed to the program by the user
// Whether to execute on a match, or on no match
// The field name used in `Cmd::NamedField`
// For linux we use Jemalloc. It is ***significantly*** faster than the default allocator in this case, for some reason.
