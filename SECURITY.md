# Security

## Reporting a vulnerability

Please report it privately through GitHub: the repository's **Security** tab →
**Report a vulnerability**. Do not open a public issue for it.

## What conbus does and does not touch

conbus runs on your machine only. Its threat model is that of a local tool
that other programs call on every agent turn.

- **Credentials.** conbus never reads, stores or forwards vendor auth tokens,
  and never changes how a vendor logs in. Quota comes from what the vendors
  already expose to their own hooks and CLIs.
- **Consent gates.** Codex asks you to trust each hook before it runs one.
  conbus never writes that trust record for you.
- **The socket.** The daemon listens on a Unix socket in its data directory
  with mode `0600`. Nothing listens on a network port unless you start
  `conbus hook --inference-listen`, which binds `127.0.0.1:8787` by default
  and refuses any address that is not loopback, because it authenticates
  nothing.
- **Hooks never block you.** A hook that cannot reach the daemon still exits
  0 with well-formed JSON; the record is dropped and noted in
  `~/.conbus/drops.jsonl`. conbus's own background sessions are marked so the
  hooks do not record them.
- **Your files.** `install` and `uninstall` merge into vendor settings files,
  keep a backup before each change, write by replacing atomically, and refuse
  to touch a file they cannot parse.
- **The log at rest.** Plain JSONL under `~/.conbus`, readable by your user
  only: the directory is `0700` and each segment `0600`. Builds before 0.2.5
  left them `0755` and `0644`; the recorder closes such a directory when it
  starts. Tool bodies are not written by default; `CONBUS_RETENTION=encrypted_tools`
  seals them with AES-256-GCM under a key kept in your login Keychain, and
  `ephemeral` writes nothing. The daemon disables core dumps and locks its
  memory where the OS allows.
- **What leaves the machine.** See "What is recorded, and where it goes" in the
  [README](README.md): briefings, hops and `conbus summary` put excerpts of the
  log into a vendor session, and `conbus usage --otlp URL`, only when you run
  it, sends counts to a collector on this machine that you name; nothing is
  sent anywhere else.

For a reviewer, [`docs/security-packet.md`](docs/security-packet.md) lists on
one page what conbus keeps, runs, changes and sends, and its known gaps.

## Supported versions

The latest 1.x release is supported; fixes land in the next 1.x release rather
than being backported. A release candidate (`v1.0.0-rc.N`) is supported until
the release it precedes ships. 0.x releases are no longer supported.
