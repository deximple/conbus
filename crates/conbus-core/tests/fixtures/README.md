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

`claude_transcript_2.1.316_shapes.jsonl` is not a segment. It is a Claude Code
transcript in the vendor's own shapes, one real line of each kind with every
value redacted, and `tests/transcript_shapes.rs` pins what the backfill parser
makes of it.
