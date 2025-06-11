# vicut

Command line utility for using Vim commands to process text and extract fields from stdin.

## Why vicut?
I know how to use vim fluently and want to be able to use it to process text on the command line easily. The only thing that currently exists that allows something like this is `nvim --headless`, but it's usually overkill.

## Overview
`vicut` is a tool meant to be used in pipelines. It reads data from stdin, and then uses the command flags given by the user to operate on the text and extract fields. There are four command flags. 

* `-c`/`--cut <CMD>` executes a Vim command, and returns the span covered by the cursor's motion as a field. 
* `-m`/`--move <CMD>` does the same thing, but does not return a span, so it can be used to position the cursor or edit the buffer pre-emptively.
* `-r`/`--repeat <N> <R>` repeats `N` previous commands `R` times. Repeats can be logically nested.
* `-n`/`--next` concludes the current field group and starts a new one. Each field group is a separate record in the output.

This structure allows powerful text extraction from messy or loosely structured inputs.

## Usage

```
vicut [OPTIONS] [COMMANDS]...


OPTIONS:
        --delimiter <STR>
                Provide a delimiter to place between fields in the output. No effect when used with --json.

        --json
                Output the result as structured JSON.

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
(git clone https://github.com/km-clay/vicut && cd vicut && cargo build --release && install -Dm755 target/release/vicut ~/.local/bin && echo "Installed the binary to ~/.local/bin, make sure that is in your \$PATH")
```

## Notes

`vicut` is experimental and still in early development. The core functionality is stable (probably) and usable, but many of Vim's more obscure motions and operators are not yet supported. The logic for executing the Vim commands is entirely home-grown, so there may be some small inconsistencies between Vim and vicut. The internal editor logic is adapted from the line editor I wrote for [`fern`](https://github.com/km-clay/fern), so some remnants of that may still appear in the codebase. Any and all contributions are welcome.
