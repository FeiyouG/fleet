## Why

Fleet runs every agent in a tmux pane on the laptop. At 10–15 tasks a day (spikes of
20–30) the laptop is the bottleneck, agents stop when it sleeps, and a task waiting hours
for plan review keeps local resources busy. Agents should be able to run on cloud machines
(InstaBox first) that pause while they wait for you and wake when you attach, while the
laptop TUI keeps one view of everything.

## What Changes

- New **machine** concept: a computer an agent runs on. Built-in machine adapters
  (providers) `local` (the laptop, today's behaviour) and `instabox` (HTTP API). No
  user-defined adapters.
- New config section `machines.<name>` (machine profiles), picked like agents:
  CLI/popup → project task → task → project defaults → defaults. Shipped default:
  `laptop = { provider = "local" }`, `defaults.machine = "laptop"`.
- tmux sessions and git worktrees run *through* a machine; tmux is required on every
  machine.
- Cloud machines are one per task by default (`per_task = true`); sharing is allowed
  (`per_task = false`).
- Agent names are unique across all machines.
- TUI tabs become **Agents · Machines · Worktrees · Reviews** (Machines replaces Tmux),
  one state vocabulary (one icon + colour per meaning, Nerd Font), a machine scope that
  cycles next to the project scope, and tree levels that hide only when a scope pins them.
- `fleet machine …` commands to list and control machines (pause, resume, keep awake,
  idle timer, stop/start, delete, shell, snapshot, doctor). Fleet does not pause machines
  on its own; the provider's idle rule does.
- Cloud agents publish their state into the machine's provider metadata so the laptop
  reads it without waking the machine.
- Secrets use the provider's built-in egress proxy; OAuth logins are synced from the
  laptop with refresh tokens removed.

## Capabilities

### New Capabilities
- `machines`: the machine interface, the `local` and `instabox` adapters, machine
  profiles, names, states, pausing and remote state.
- `tui-machines`: tabs, the state vocabulary, adaptive tree levels and scopes.

### Modified Capabilities
<!-- none: fleet has no existing OpenSpec specs; prior behaviour lives in DESIGN.md -->

## Impact

- `crates/fleet-core`: new `machines/` module; `config` gains `machines`; registry
  `Task` gains `machine` and `machine_id`; `tmux.rs`, `worktrees/git.rs` and their
  callers route through a machine.
- `crates/fleet`: TUI tabs, header scopes, leader menus, `fleet machine` CLI.
- New dependency use: `reqwest` (already a dependency) for the InstaBox API.
- README (Nerd Font requirement, machines config), DESIGN.md pointer to OpenSpec.
- Local tasks behave exactly as today with the shipped default.
