# Golden segments

Real segments produced by old builds, replaying the hook payloads in
`crates/conbus/tests/fixtures/` through each build's own daemon and `hook`.
They pin the 1.x promise — a 1.x build reads every record any earlier build
wrote — so they must never be edited; an edited line is a claim about the
format nobody checked.

| file | `v` | written by |
| --- | --- | --- |
| `segment-v1.jsonl` | 1 | `6ea53ab` (last commit before `3f711f7` bumped the schema) |
| `segment-v2.jsonl` | 2 | `6e9f48e` (last commit before `897164c`) |
| `segment-v3.jsonl` | 3 | `897164c` |
