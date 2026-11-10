## Purpose

Let a fleet agent run on any configured machine — the laptop or a cloud machine — with the
same tmux, worktree and agent behaviour, and let fleet list and control those machines.

## ADDED Requirements

### Requirement: Built-in machine adapters
Fleet SHALL ship exactly the machine adapters `local` and `instabox`. A machine profile
whose `provider` names any other adapter MUST be rejected by config validation.

#### Scenario: Unknown provider
- **WHEN** `init.lua` declares `machines.x = { provider = "e2b" }`
- **THEN** loading the config reports an error naming `x` and the known providers

#### Scenario: Default machine
- **WHEN** the user's config declares no machines
- **THEN** the machine profile `laptop` with provider `local` exists and is the default

### Requirement: Machine profile resolution
A task SHALL run on the machine profile resolved in this order: CLI/popup, project task,
global task, project defaults, global defaults (`defaults.machine`).

#### Scenario: Task picks a cloud profile
- **WHEN** `tasks.review.machine = "ibx"` and the user starts the review task with no
  override
- **THEN** the agent runs on a machine created from profile `ibx`

#### Scenario: CLI override
- **WHEN** the user runs `fleet task review --machine laptop`
- **THEN** the agent runs on the laptop regardless of the task's profile

### Requirement: Local behaviour unchanged
On the `local` adapter every tmux, git and agent command SHALL be exactly the command fleet
ran before this change.

#### Scenario: Local start
- **WHEN** a task starts on `laptop`
- **THEN** the worktree, tmux session layout and agent launch are identical to before

### Requirement: Machine lifecycle per profile
A profile with `per_task = true` (the default for cloud providers) SHALL get a new machine
per started agent, named `<profile>-<agent>`, deleted when the agent's task is closed. A
profile with `per_task = false` SHALL reuse a running machine of that profile when one
exists, and the machine SHALL be deleted when its last task closes.

#### Scenario: Per-task machine
- **WHEN** agent `fix-a` starts on profile `ibx` with `per_task = true`
- **THEN** a machine `ibx-fix-a` is created and holds session `fix-a` and worktree `fix-a`

#### Scenario: Shared machine
- **WHEN** two agents start on a `per_task = false` profile
- **THEN** both run on the same machine, each in its own session and worktree

### Requirement: Unique agent names across machines
Starting an agent SHALL fail when an open task with the same name exists on any machine.

#### Scenario: Name taken remotely
- **WHEN** `fix-a` is open on `ibx-fix-a` and the user starts `fix-a` on the laptop
- **THEN** start fails and names the machine that already has `fix-a`

### Requirement: tmux on every machine
Every agent fleet starts SHALL run in a tmux session on its machine. `fleet machine doctor`
SHALL report whether `tmux`, `git` and the harness are present on a machine.

#### Scenario: Doctor on a machine without tmux
- **WHEN** `fleet machine doctor ibx-fix-a` runs on a machine without tmux
- **THEN** it reports tmux as missing and exits non-zero

### Requirement: Machine states
Fleet SHALL report each machine's state as one of `running`, `starting`, `paused`,
`stopped`, `cooldown` (with the time it ends) or `error`. The laptop is always `running`.
Listing machines MUST NOT wake a paused machine.

#### Scenario: Paused machine listed
- **WHEN** the TUI refreshes while `ibx-fix-c` is paused
- **THEN** it shows `paused` and the machine stays paused

### Requirement: Machine control without automatic pausing
Fleet SHALL offer `fleet machine ls|pause|resume|stop|start|rm|keep|release|idle|max-run|shell|snapshot|doctor`
for adapters that support each verb, and SHALL report "not supported" otherwise. Fleet
SHALL NOT pause or stop a machine on its own; the provider's idle rule does.

#### Scenario: Keep awake
- **WHEN** the user runs `fleet machine keep ibx-feat-b 2h`
- **THEN** the provider does not idle-pause the machine for 2 hours

#### Scenario: Unsupported verb
- **WHEN** the user runs `fleet machine pause laptop`
- **THEN** fleet reports that the `local` adapter does not support pause

### Requirement: Attach wakes the machine
Attaching to an agent or session on a paused machine SHALL resume the machine and then
attach, keeping the tmux session and agent intact.

#### Scenario: Attach after idle pause
- **WHEN** the user attaches to `fix-c` on paused `ibx-fix-c`
- **THEN** the machine resumes and the user lands in the `fix-c` session with the agent
  still waiting where it was

### Requirement: Remote agent state without waking
An agent on a cloud machine SHALL publish its state changes (state, plan port, time) to the
machine's provider metadata. The laptop SHALL read remote agent state from the provider's
machine list, never by running a command inside a machine during a refresh.

#### Scenario: Plan ready on a cloud machine
- **WHEN** the agent on `ibx-feat-b` submits a plan
- **THEN** the laptop shows the agent as waiting on a plan within one remote refresh, and
  the notification fires as for local agents

### Requirement: Secrets via the provider
Machine profiles SHALL name secrets only, never values. Proxy secrets SHALL be delivered by
the provider's egress proxy; file secrets (login files) by the provider's direct-secret
files. `fleet creds sync` SHALL push laptop OAuth logins with refresh tokens removed.

#### Scenario: Git push with a proxied token
- **WHEN** an agent on a cloud machine pushes to GitLab with `GITLAB_TOKEN` granted
- **THEN** the push succeeds and the machine's environment holds only a placeholder
