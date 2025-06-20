# vicut

`vicut` is a Vim-based, scriptable, headless text editor for the command line.  

![vicut](https://github.com/user-attachments/assets/3e0e9e10-cc41-4203-9302-9965a0e42893)


It combines the power of Vim motions/operators with the general-use applicability of command line tools like `sed`, `awk`, and `cut`. `vicut` can be used to extract fields, edit text files in-place, apply global substitutions, and more.

## Why vicut?
I'm fluent with Vim and often find myself wishing I could use its expressive editing features **outside** the interactive editor — especially when writing shell scripts. Tools like `awk`, `sed`, and `cut` are powerful for formatting command output and extracting fields, but I’ve lost count of how many times I’ve thought:
> *"This would be way easier if I could just ask Vim to do it."*

So I decided to repurpose [my shell](https://github.com/km-clay/fern)'s line editor into a CLI tool that can process files and input streams using Vim commands.

## Features

### 🔍 Core Editing

* Apply Vim-style editing commands (e.g. `:%s/foo/bar/g`, `d2W`, `ci}`) to one or more files, or stdin.
* Use Vim motions to extract or modify structured data.
* Perform in-place file edits with `-i`, or print to stdout by default.

### 📦 Flexible Output

* Output in plain text, JSON (`--json`), or interpolate fields using a format string (`--template`).
* Chain multiple editing/extraction commands.
* Capture multiple fields using `-c`, and structure them using `-n` and `--delimiter`.

### ⚡ High Performance

* Use `--linewise` for stream-style processing like `sed`, but multi-threaded.
* Combine regex pattern matching with Vim-style editing in a single tool.

## ⚙️ Usage

`vicut` uses an internal text editing engine based on Vim. File names can be given as arguments, or text can be given using stdin. There are four command flags you can use to issue commands to the internal editor.

* `-c`/`--cut <VIM_CMD>` executes a Vim command (something like `5w`, `vi)`, `:%s/foo/bar/g`, etc) and returns the span of text covered by the cursor's motion as a field. Any arbitrary number of fields can be extracted using `-c`. If no `-c` commands are given, `vicut` will print the entire buffer as a single field.
* `-m`/`--move <VIM_CMD>` silently executes a Vim command. `-m` does not extract a field from the buffer like `-c` does, making it ideal for positioning the cursor before `-c` calls, or making edits to the buffer.
* `-g`/`--global <PATTERN> <COMMANDS>` allows for conditional execution of command flags. Any command flags following `-g` will only execute on lines that match the pattern given after `-g`. The `-g` scope can be exited using `--end`, which will allow you to continue writing unconditional commands. For the purpose of repetition with `-r`, the entire `-g` block counts as a single command to be repeated.
* `-v`/`--not-global <PATTERN> <COMMANDS>` same behavior as `-g`, except it executes the contained command flags on lines that *don't* match the given input.
* `-r`/`--repeat <N> <R>` repeats `N` previous commands `R` times. Repeats can be logically nested.
* `-n`/`--next` concludes the current 'field group' and starts a new one. Each field group is printed as a separate record in the output, or as a separate JSON object if using `--json`

Command flags can be given any number of times, and the commands are executed in order of appearance.

### Output Format Options

Output can be structured in three different ways using these options:
* `-j`/`--json` emits the extracted field data as a json object, ready to be piped into other programs, such as `jq`
* `-d`/`--delimiter <STR>` lets you give a field separator as an argument to the flag. The separator is placed inbetween each field in each record.
* `-t`/`--template <STR>` lets you define a custom output format using a format string. Fields are interpolated on placeholders that look like `{{1}}` or `{{field_name}}`.

### Execution Behavior Options

* `-i` If you have given files as arguments to read from, the `-i` flag will make `vicut` edit the contents of those files in-place. This is an atomic operation, meaning changes will only be written to the files if all operations succeed.
* `--backup` If the `-i` option has been set, this will create a backup of the files to be edited.
* `--backup-extension` Allows you to set an arbitrary file extension to use for the backups. Default is `.bak`
* `--keep-mode` The internal editor always returns to Normal mode after each call to `-m` or `-c`. This flag prevents that behavior, and causes the internal editor's mode to persist between calls.
* `--linewise` Makes `vicut` treat each line of text in the input as a separate buffer. The sequence of commands you give to `vicut` will be applied to every line. This operation utilizes multi-threading to operate on lines in parallel, making it far faster than full buffer editing.
* `--serial` Makes `--linewise` mode operate on each line sequentially instead of using multi-threading.
* `--jobs` Restricts the number of threads `--linewise` can create for operating on lines.
* `--trim-fields` Trims leading and trailing whitespace from fields extracted by `-c`.

#### ℹ️ Examples and in-depth usage ideas can be found on the [wiki](https://github.com/km-clay/vicut/wiki)

## 🚀 Performance
While tools like `awk` and `sed` do beat `vicut` in speed for full-buffer processing, `vicut`'s `--linewise` mode emulates the stream processing behaviors of `awk` and `sed` by treating each line of input as an independent buffer, and processing each in parallel. This allows `vicut`'s performance to scale horizontally across CPU cores, giving it an edge over traditional Unix text processors for non-trivial inputs— even while executing more semantically rich operations like Vim motions.

On structured input, execution speed of `vicut` in `--linewise` mode is comparable to or faster than the speeds of `sed` and `awk` on datasets up to 1 million lines.  
Here's a benchmark using a generated data set that looks like this:
```
00001) Provider-1 (City-1, State-1) [924.05 km]
00002) Provider-2 (City-2, State-2) [593.91 km]
00003) Provider-3 (City-3, State-3) [306.39 km]
00004) Provider-4 (City-4, State-4) [578.94 km]
00005) Provider-5 (City-5, State-5) [740.13 km]
...
```
With the target output being:
```
00001 ---- Provider-1 ---- City-1, State-1 ---- 924.05 km
00002 ---- Provider-2 ---- City-2, State-2 ---- 593.91 km
00003 ---- Provider-3 ---- City-3, State-3 ---- 306.39 km
00004 ---- Provider-4 ---- City-4, State-4 ---- 578.94 km
00005 ---- Provider-5 ---- City-5, State-5 ---- 740.13 km
...
```
### 25,000 lines
| Tool    | Command                                                                                | Wall-Clock Time | 
| ------- | -------------------------------------------------------------------------------------- | --------------- | 
| `sed`   | `sed -E -e 's/[][]//g' -e 's/(\) \| \()/ ---- /g'`                                   | 20.0ms          |
| `awk`   |  `awk -vOFS=" --- " -F'[][()]' '{ print $1, $2, $3, " " $5 }'`                       | 13.7ms          |
| `vicut` | `vicut --linewise --delimiter ' ---- ' -c 'e' -m '2w' -c 't(h' -c 'vi)' -c 'vi]'`       | 11.9ms          |


### 100,000 lines
| Tool    | Command                                                                                | Wall-Clock Time |
| ------- | -------------------------------------------------------------------------------------- | --------------- |
| `sed`   | `sed -E -e 's/[][]//g' -e 's/(\) \| \()/ ---- /g'`                                  | 76.0ms          |
| `awk`   |  `awk -vOFS=" ---- " -F'[][()]' '{ print $1, $2, $3, " " $5 }'`                       | 51.6ms          |
| `vicut` | `vicut --linewise --delimiter ' ---- ' -c 'e' -m '2w' -c 't(h' -c 'vi)' -c 'vi]'`       | 35.2ms          |

### 1,000,000 lines
| Tool    | Command                                                                                | Wall-Clock Time |
| ------- | -------------------------------------------------------------------------------------- | --------------- |
| `sed`   | `sed -E -e 's/[][]//g' -e 's/(\) \| \()/ ---- /g'`                                    | 756.4ms          |
| `awk`   |  `awk -vOFS=" ---- " -F'[][()]' '{ print $1, $2, $3, " " $5 }'`                       | 499.1ms          |
| `vicut` | `vicut --linewise --delimiter ' ---- ' -c 'e' -m '2w' -c 't(h' -c 'vi)' -c 'vi]'`       | 296.0ms          |

*Benchmark recorded using an AMD Ryzen 9 9950X (16-Core) running Arch Linux*

The command used to generate the datasets was this, if you want to reproduce these benchmarks at home:
`seq -w 1 1000000 | awk 'BEGIN { OFMT="%.2f" } { printf "%05d) Provider-%d (City-%d, State-%d) [%.2f km]\n", $1, $1, $1, $1, rand()*1000 }' > providers.txt`

## 📦 Installation

**NOTE:** Building requires the `rustc` compiler and the `cargo` package manager. Both can be installed using `rustup`.

#### Cargo Installation
You can have `cargo` download and build the source code using this command:
```
cargo install --git https://github.com/km-clay/vicut
```
Note that this will install the `vicut` binary to `~/.cargo/bin` so make sure that is in your PATH.  

#### Building from Source
Alternatively, you can clone the repo and build it manually:

1. Clone the repository, navigate to it
```bash
git clone https://github.com/km-clay/vicut
cd vicut
```
2. Build the binary from source
```bash
cargo build --release
```
3. Install the binary to some place in your PATH
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

## Notes

`vicut` is experimental and still in early development. The core functionality is stable and usable, but many of Vim's more obscure motions and operators are not yet supported. The logic for executing the Vim commands is entirely home-grown, so there may be some small inconsistencies between Vim and vicut. The internal editor logic is adapted from the line editor I wrote for [`fern`](https://github.com/km-clay/fern), so some remnants of that may still appear in the codebase. Any and all contributions are welcome.
