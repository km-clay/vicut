# vicut Usage Examples

Below are some example use-cases for `vicut` along with comparisons to standard field extraction tools.

## Exhibit A: `speedtest-cli --list`

Running `speedtest-cli --list` will show a list of servers that the program can use to run a speed test. The output of `speedtest-cli --list` is very readable for humans, but non-trivial for machines due to the inconsistent use of spaces.
```
31200) FiberFast Networks (Portland, OR, United States) [321.23 km]
18220) MetroLink Broadband (Austin, TX, United States) [121.47 km]
29834) Skyline Internet (Denver, CO, United States) [295.88 km]
40422) BrightWayNet (Minneapolis, MN, United States) [673.21 km]
51510) EverSpeed (Charlotte, NC, United States) [104.36 km]
27171) ClearStream Internet (Boston, MA, United States) [151.92 km]
14402) Skybeam Communications (Chicago, IL, United States) [215.34 km]
63317) LumenX Fiber (Las Vegas, NV, United States) [412.75 km]
18734) NetHarbor (Phoenix, AZ, United States) [520.11 km]
80001) NovaFiber (Seattle, WA, United States) [643.89 km]
```

### Tool Comparison

#### `cut`
Immediately fails due to the presence of spaces within fields like the city name and distance. 

#### `sed`
Can do the job using this somewhat complex regular expression:
```bash
speedtest-cli --list | sed -n 's/^\([0-9]*\)) \(.*\) (\(.*\)) \[\(.*\)\]/\1 --- \2 --- \3 --- \4/p'

31200 --- FiberFast Networks --- Portland, OR, United States --- 321.23 km
18220 --- MetroLink Broadband --- Austin, TX, United States --- 121.47 km
29834 --- Skyline Internet --- Denver, CO, United States --- 295.88 km
. . .
```

#### `awk`
Works with some clever delimiter tricks and extra cleanup:
```bash
awk -F'[()]' '{ gsub(/\[|\]/, "", $4); print $1, "---", $2, "---", $3, "---", $4 }'

31200 ---  FiberFast Networks  --- Portland, OR, United States ---  321.23 km
18220 ---  MetroLink Broadband  --- Austin, TX, United States ---  121.47 km
29834 ---  Skyline Internet  --- Denver, CO, United States ---  295.88 km
. . .
```

#### `vicut`
Is able to parse this output using this command:
```bash
vicut --linewise --delimiter ' --- ' \
    -c 'e' \    # Capture to the end of the first word
    -m '2w' \   # Move past the close parenthesis after the id
    -c 't(h' \  # Capture to the next open parenthesis, and then back one
    -c 'vi)' \  # Capture inside of parenthesis
    -c 'vi]'    # Capture inside of brackets

31200 --- FiberFast Networks --- Portland, OR, United States --- 321.23 km
18220 --- MetroLink Broadband --- Austin, TX, United States --- 121.47 km
29834 --- Skyline Internet --- Denver, CO, United States --- 295.88 km
. . .
```
This showcases the use of **visual mode and text objects** for precise field extraction. If the internal editor is in visual mode at the end of a command, it returns just the selected range—rather than the entire motion span.  
  
This allows for fine-grained control over field boundaries: you can enter visual mode, position the end of the selection, and then use the `o` motion to jump back and refine the start of the selection. This mirrors how Vim itself allows back-and-forth selection editing and makes `vicut` unusually expressive for semi-structured text.

## Exhibit B: `nmcli dev`
Running `nmcli dev` lists all of your network devices, their types, states, and the names of their connections. Once again this output is very readable for humans, but is tricky for machines to handle due to the inconsistent use of spaces.

```
DEVICE          TYPE      STATE                   CONNECTION         
enp10s0         ethernet  connected               Wired connection 1 
wlp9s0          wifi      connected               NETWORK NAME
lo              loopback  connected (externally)  lo                 
p2p-dev-wlp9s0  wifi-p2p  disconnected            --
```

### Tool Comparison

#### `cut`
Once again, fails immediately due to inconsistent use of field separators in the input.

#### `sed`
Requires this messy regex that was pretty time consuming to write and test:
```bash
sed -E 's/^([^ ]+)[[:space:]]+([^ ]+)[[:space:]]+([^ ]+([[:space:]]\([^)]+\))?)[[:space:]]+(.*)$/\1 --- \2 --- \3 --- \5/'

DEVICE --- TYPE --- STATE --- CONNECTION         
enp10s0 --- ethernet --- connected --- Wired connection 1 
wlp9s0 --- wifi --- connected --- NETWORK NAME       
lo --- loopback --- connected (externally) --- lo                 
p2p-dev-wlp9s0 --- wifi-p2p --- disconnected --- --
```

#### `awk`
The `awk` solution for parsing this output is very involved compared to most `awk` field extraction operations:
```bash
awk '
NR == 1 { print $1, "---", $2, "---", $3, "---", $4; next }
{
  device = $1
  type = $2

  # Field 3 and possibly 4 make up the STATE (e.g. "connected (externally)")
  state = $3
  i = 4
  if ($4 ~ /^\(/) {
    state = state " " $4
    i = 5
  }

  # Remaining fields are the CONNECTION
  connection = $i
  for (j = i + 1; j <= NF; j++) {
    connection = connection " " $j
  }

  print device, "---", type, "---", state, "---", connection
}'

DEVICE --- TYPE --- STATE --- CONNECTION
enp10s0 --- ethernet --- connected --- Wired connection 1
wlp9s0 --- wifi --- connected --- NETWORK NAME
lo --- loopback --- connected (externally) --- lo
p2p-dev-wlp9s0 --- wifi-p2p --- disconnected --- --
```

#### `vicut`
The solution using `vicut` for parsing this output is very concise by comparison:
```bash
vicut --linewise --trim-fields --delimiter ' --- ' \  
    -c 'E' \    # Capture to end of word
    -m 'w' \    # Move to start of next word
    -r 2 1 \    # Repeat 2 commands 1 time
    -c 'ef)' \  # Capture to end of word, or the next close parenthesis if one exists
    -m 'w' \    # Move to start of next word
    -c '$'      # Capture to end of line

DEVICE --- TYPE --- STATE --- CONNECTION
enp10s0 --- ethernet --- connected --- Wired connection 1
wlp9s0 --- wifi --- connected --- NETWORK NAME
lo --- loopback --- connected (externally) --- lo
p2p-dev-wlp9s0 --- wifi-p2p --- disconnected --- --
```
This command in particular also demonstrates how the `f` and `t` character-search motions can act like conditionals. If the target character (e.g. `)`) exists, the cursor moves; if not, it stays put — allowing `vicut` to gracefully handle optional formatting, like `(externally)` in the `STATE` field, without extra logic.
