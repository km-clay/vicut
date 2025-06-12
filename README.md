# vicut

A command line utility for using Vim commands to process text and extract fields from stdin.

## Why vicut?
Extracting fields with standard Unix tools like `cut`, `sed`, or `awk` has always felt messy to me.  
`cut` folds like a lawn chair at the first sign of any resistance.  
`sed` often demands verbose, brittle regexes that are time consuming to write and are never reused.  
And while `awk` is powerful, it quickly becomes verbose when dealing with anything non-trivial.  

I wanted a tool that makes field extraction intuitive and precise, even with messy or irregular input.

## Overview
`vicut` is a tool meant to be used in pipelines. Internally, it uses a stateful text editing engine based on Vim. It reads data from stdin, and then uses the command flags given by the user to operate on the text and extract fields. Fields are extracted based on cursor movements. There are four command flags. 

* `-c`/`--cut <CMD>` executes a Vim command, and returns the span covered by the cursor's motion as a field. 
* `-m`/`--move <CMD>` does the same thing, but does not return a span, so it can be used to position the cursor or edit the buffer pre-emptively.
* `-r`/`--repeat <N> <R>` repeats `N` previous commands `R` times. Repeats can be logically nested.
* `-n`/`--next` concludes the current field group and starts a new one. Each field group is a separate record in the output.

This method allows for very powerful text extraction, even from loosely structured inputs.

## Usage

For advanced usage and some examples/comparisons with other tools, you can check out the [wiki](https://github.com/km-clay/vicut/wiki)

```
vicut [OPTIONS] [COMMANDS]...


OPTIONS:
        -t, --template <STR>
                Provide a format template to use for custom output formats. Example:
                --template "< {{1}} > ( {{2}} ) { {{3}} }"
                Names given to fields explicitly using '-c name=<name>' should be used instead of field numbers.

        -d, --delimiter <STR>
                Provide a delimiter to place between fields in the output. No effect when used with --json.

        --json
                Output the result as structured JSON.

        --keep-mode
                The internal editor will not return to normal mode after each command.

        --linewise
                Apply given commands to each line in the given input.

        --trim-fields
                Trim leading and trailing whitespace from captured fields.

        --trace
                Print debug trace of command execution


COMMANDS:
        -c, --cut [name=<NAME>] <VIM_COMMAND>
                Execute a Vim command on the buffer, and capture the text between the cursor's
                start and end positions as a field.
                Fields can be optionally given a name, which will be used as the key
                for that field in formatted JSON output.

        -m, --move <VIM_COMMAND>
                Logically identical to -c/--cut, except it does not capture a field.

        -r, --repeat <N> <R>
                Repeat the last N commands R times. Repeats can be nested.

        -n, --next
                Start a new field group. Each field group becomes one output record.


NOTES:
        * Commands are executed left to right.
        * Cursor state is maintained between commands, but the editor returns to normal mode between each command.
        * Commands are not limited to only motions. Commands which edit the buffer can be executed as well.


EXAMPLE:
        $ echo 'foo bar (boo far) [bar foo]' | vicut --delimiter ' -- ' \
        -c 'e' -m 'w' -r 2 1 -c 'va)' -c 'va]'
        outputs:
        foo -- bar -- (boo far) -- [bar foo]
```

## Installation

**NOTE:** You will need to have `cargo` installed in order to build `vicut`

1. Clone the repository, navigate to it
```bash
git clone https://github.com/km-clay/vicut
cd vicut
```
2. Build the binary from source
```bash
cargo build --release
```
3. Install the binary to some place in your path
```bash
install -Dm755 target/release/vicut ~/.local/bin # or wherever
```

Here's a one liner for all of that:
```bash
(git clone https://github.com/km-clay/vicut && cd vicut && cargo build --release && mkdir -p ~/.local/bin && install -Dm755 target/release/vicut ~/.local/bin && echo "Installed the binary to ~/.local/bin, make sure that is in your \$PATH")
```

## Notes

`vicut` is experimental and still in early development. The core functionality is stable (probably) and usable, but many of Vim's more obscure motions and operators are not yet supported. The logic for executing the Vim commands is entirely home-grown, so there may be some small inconsistencies between Vim and vicut. The internal editor logic is adapted from the line editor I wrote for [`fern`](https://github.com/km-clay/fern), so some remnants of that may still appear in the codebase. Any and all contributions are welcome.
