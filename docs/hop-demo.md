# Hop demo

This is the public walkthrough. It does not open a vendor window and it
does not write `trusted_hash`.

## Recorded failover

A Claude Code turn hits `StopFailure` `error=rate_limit`. The bus records
`routed displaced` under the same `bus_id`, opens a bus-owned Codex warm
(`warm:<id>`), writes `takeover.json`, and `conbus run` execs the next
leg in the same TTY. The next human prompt is `continued displaced`.

Replay the recorded shape without touching this machine's daemon:

```bash
conbus hop --demo
```

Expected lines: `route hop to codex · displaced · rate_limit`,
`meter routing 0 · failover 1`, and the `conbus hop --apply` /
`conbus run` pair a person would type on a live conversation.

## Directed hop

A chat line that is only an order (`이제 커서로 작업할거야`, `switch to grok`) is `--to` after the hook reply. Same from a terminal:

```bash
conbus hop cursor           # preview, current conversation
conbus go cursor            # apply
```

Cursor landing is official `cursor -n <workspace>`. The next Composer in that checkout binds the same `bus_id`. Claude and Codex still use bus-owned warms.

## Live hop (this machine)

```bash
conbus hop                  # decide
conbus hop --to codex       # directed; still a preview
conbus hop --from <bus_id> --apply
conbus run                  # same TTY
```

`--apply` records through the daemon and opens or syncs bus-owned warms.
It never injects into a thread the person opened. Codex hooks stay silent
until that person trusts them inside Codex.

## What success looks like

`conbus log --bus-id <id>` shows `routed` and `continued` on one `bus_id`.
`conbus verdict` counts failover hops separately from routing hops.
Re-explanation tokens are not in the log; do not invent them.
