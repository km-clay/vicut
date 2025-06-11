# vicut Advanced Usage

`vicut` is a tool with many possible advanced uses. The four command flags available can be combined in very creative ways.

## Nested Repeats

Let's take this contrived input:
```
foo bar biz
biz foo bar
bar biz foo
```
Let's try to extract the first field of each line, then the second field of each line, then the third.  
  
Since we cannot use `--linewise` for this, we will have to repeat ourselves quite often here. Luckily, the `-r` flag can be used to keep things concise.

```bash
echo -e "foo bar biz\nbiz foo bar\nbar biz foo" | vicut --json \
    -c 'e' \
    -m 'jb' \
    -r 2 2 \
    -m 'wkk' \
    -n \
    -r 4 2 
```
Produces this JSON object:
```json
[
  {
    "field_1": "foo",
    "field_2": "biz",
    "field_3": "bar"
  },
  {
    "field_1": "bar",
    "field_2": "foo",
    "field_3": "biz"
  },
  {
    "field_1": "biz",
    "field_2": "bar",
    "field_3": "foo"
  }
]
```

### How this works
* The first repeat command `-r 2 2` repeats the pair `-c 'e'` and `-m 'jb'`, capturing the first word on each line.
* Then, we position the cursor on the start of the next field, and move back to the first line with `-m 'wkk'`, followed by a call to `-n` to split to a new field group.
* We then make another repeat command `-r 4 2` which repeats this entire sequence, including the previous repeat command.

Because calls to `-r` can be nested in this way, you can compose compact, layered field extraction logic without duplicating motion commands.

## Editing the Buffer

Motions are not the only commands that can be passed. Commands which edit the buffer can also be used, and can be used to great effect in order to remove any garbage data that may exist in the input.

For instance:
```
useful_data1 some_garbage useful_data2
```

Let's say we want `useful_data1` and `useful_data2` to exist in the same field. In order to accomplish this, we can do something like:
```bash
echo "useful_data1 some_garbage useful_data2" | vicut --json -c 'wdw$'
```
which will produce this JSON object:
```
