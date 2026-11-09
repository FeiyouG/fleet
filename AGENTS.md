# Agent instructions for the fleet repo

## HARD RULE: never touch the user's tmux server

Every fleet-started agent runs inside a pane of the user's own tmux server, so `$TMUX` is set
in your shell. While `$TMUX` is set, tmux **ignores `TMUX_TMPDIR`** and every bare `tmux …`
command goes to the user's server. `TMUX_TMPDIR=/tmp/x tmux kill-server` therefore kills
**all** of the user's sessions (this has happened).

- Never run `tmux kill-server`, `pkill tmux`, `pkill -f tmux`, or any tmux command meant for a
  test server without an explicit socket: `tmux -S <socket path> …`.
- In scripts, wrap every tmux call: `t() { tmux -S "$T/sock" "$@"; }`, and check the socket
  path is inside your scratch dir before using it. Do not rely on `unset TMUX` alone.
- Never act on the user's server except through fleet itself (`fleet agent stop`,
  `fleet close`, the TUI), and only on what the task asked for.
- Leftover scratch dirs (`/tmp/fleet-e2e.*`): `rm -rf` them. Do not kill processes to clean up.

## Tests

- `cargo fmt && cargo test`; `cargo clippy --all-targets` should add no new warnings.
- `scripts/e2e-close.sh` (after `cargo build`): close/gc end to end against a scratch repo,
  registry and tmux server (explicit socket), with stub `opencode` and `gh` binaries. It
  asserts every agent it starts is the stub, so no real agent or model is ever called.
- Test configs must declare `defaults.agent` and an `agents` table (`Config::validate`).

## Conventions

- Conventional commits: `feat(core): …`, `fix(tui): …`, `docs: …`.
- Changes to behaviour update README.md (user-facing) and the OpenSpec specs.

## OpenSpec

- `openspec/specs/**` is the source of truth for behaviour; read the relevant spec before the
  code. Proposed changes live in `openspec/changes/<name>/` (proposal, design, specs delta,
  tasks) and are archived into `openspec/specs/` once merged.
- New behaviour starts as a change (`openspec new change <name>`); keep its `tasks.md` in sync
  with the work and run `openspec validate --all --strict` before committing.
- `DESIGN.md` is the history of decisions made before OpenSpec; don't append to it.
