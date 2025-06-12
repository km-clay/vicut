# vicut

A command line utility for using Vim commands to process text and extract fields from stdin.

## Why vicut?
Extracting fields with standard Unix tools like `cut`, `sed`, or `awk` has always felt messy to me.  
`cut` folds like a lawn chair at the first sign of any resistance.  
`sed` often demands verbose, brittle regexes that are time consuming to write and are never reused.  
And while `awk` is powerful, it quickly becomes verbose when dealing with anything non-trivial.  

I wanted a tool that makes field extraction and output formatting intuitive and precise, even with messy or irregular input.

## 🧰 Overview
`vicut` is a tool meant to be used in pipelines. Internally, it uses a stateful text editing engine based on Vim. It reads data from stdin, and then uses the command flags given by the user to operate on the text and extract fields. Fields are extracted based on cursor movements. There are four command flags. 

* `-c`/`--cut <CMD>` executes a Vim command, and returns the span covered by the cursor's motion as a field. 
* `-m`/`--move <CMD>` does the same thing, but does not return a span, so it can be used to position the cursor or edit the buffer pre-emptively.
* `-r`/`--repeat <N> <R>` repeats `N` previous commands `R` times. Repeats can be logically nested.
* `-n`/`--next` concludes the current field group and starts a new one. Each field group is a separate record in the output.
  
In this context, `<CMD>` refers to a Normal mode sequence like `di)`, `b2w`, `16vk`, `iInserting some text now<esc>b2w` etc.

This method allows for very powerful text extraction, even from loosely structured inputs.
### Output Format Options
Output can be structured in three different ways using these options:
* `-j`/`--json` emits the extracted field data as a json object, ready to be piped into other programs, such as `jq`
* `-d`/`--delimiter <STR>` lets you give a field separator as an argument to the flag. The separator is placed inbetween each field in each record.
* `-t`/`--template <STR>` lets you define a custom output format using a format string. Fields are interpolated on placeholders that look like `{{1}}` or `{{field_name}}`.

## ⚙️ Usage

For in-depth usage info, and some examples/comparisons with other tools, you can check out the [wiki](https://github.com/km-clay/vicut/wiki)

```
vicut [OPTIONS] [COMMANDS]...


OPTIONS:
        -t, --template <STR>
                Provide a format template to use for custom output formats. Example:
                --template "< {{1}} > ( {{2}} ) { {{3}} }"
                Names given to fields explicitly using '-c name=<name>' should be used instead of field numbers.

        -d, --delimiter <STR>
                Provide a delimiter to place between fields in the output. No effect when used with --json.

        -j, --json
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

## 🧪 Performance
While `vicut` is not yet fully optimized, early comparisons show that its performance is competitive given its more stateful execution model. 
Operating on a sample dataset of lines that look like this:
```
00001) Provider-1 (City-1, State-49) [924.05 km]
00002) Provider-2 (City-2, State-48) [593.91 km]
00003) Provider-3 (City-3, State-47) [306.39 km]
00004) Provider-4 (City-4, State-46) [578.94 km]
00005) Provider-5 (City-5, State-45) [740.13 km]
...
```
### 25,000 lines
| Tool    | Command                                                                                | Time (real) |
| ------- | -------------------------------------------------------------------------------------- | ----------- |
| `sed`   | `sed -E -e 's/\[]\[]//g' -e 's/\\) \\(\\)/ ---- /g'`                                   | 0.045s      |
| `awk`   | `awk -F'[()]' '{ gsub(/\[\|\]/, "", $4); print $1, "---", $2, "---", $3, "---", $4 }'` | 0.040s      |
| `vicut` | `vicut --linewise --delimiter ' --- ' -c 'e' -m '2w' -c 't(h' -c 'vi)' -c 'vi]'`       | 0.128s      |


### 100,000 lines
| Tool    | Command                                                                                | Time (real) |
| ------- | -------------------------------------------------------------------------------------- | ----------- |
| `sed`   | `sed -E -e 's/\[]\[]//g' -e 's/\\) \\(\\)/ ---- /g'`                                   | 0.164s      |
| `awk`   | `awk -F'[()]' '{ gsub(/\[\|\]/, "", $4); print $1, "---", $2, "---", $3, "---", $4 }'` | 0.148s      |
| `vicut` | `vicut --linewise --delimiter ' --- ' -c 'e' -m '2w' -c 't(h' -c 'vi)' -c 'vi]'`       | 0.383s      |

### 1,000,000 lines
| Tool    | Command                                                                                | Time (real) |
| ------- | -------------------------------------------------------------------------------------- | ----------- |
| `sed`   | `sed -E -e 's/\[]\[]//g' -e 's/\\) \\(\\)/ ---- /g'`                                   | 0.891s      |
| `awk`   | `awk -F'[()]' '{ gsub(/\[\|\]/, "", $4); print $1, "---", $2, "---", $3, "---", $4 }'` | 0.546s      |
| `vicut` | `vicut --linewise --delimiter ' --- ' -c 'e' -m '2w' -c 't(h' -c 'vi)' -c 'vi]'`       | 3.361s      |

This data suggests that while the performance of `vicut` *is* roughly 3x slower in its current state, performance scales linearly and predictably with input size.  
Given that `vicut` performs more semantically aware, stateful operations compared to stateless regex pattern matching, a performance cost is to be expected, but current profiling does show room for significant optimizations.

## 📦 Installation

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
(git clone https://github.com/km-clay/vicut && \
 cd vicut && \
 cargo build --release && \
 mkdir -p ~/.local/bin && \
 install -Dm755 target/release/vicut ~/.local/bin && \
 echo -e "\nInstalled the binary to ~/.local/bin, make sure that is in your \$PATH")
```

## 📝 Notes

`vicut` is experimental and still in early development. The core functionality is stable (probably) and usable, but many of Vim's more obscure motions and operators are not yet supported. The logic for executing the Vim commands is entirely home-grown, so there may be some small inconsistencies between Vim and vicut. The internal editor logic is adapted from the line editor I wrote for [`fern`](https://github.com/km-clay/fern), so some remnants of that may still appear in the codebase. Any and all contributions are welcome.
