## Purpose

Show every agent, machine, session, worktree and review in one TUI view, whatever machine it
runs on, with one state vocabulary and tree levels that depend only on the scope.

## ADDED Requirements

### Requirement: Tabs
The TUI SHALL have the tabs Agents, Machines, Worktrees and Reviews, in that order. Agents
SHALL always be the first tab. Machines replaces the Tmux tab.

#### Scenario: Tab order
- **WHEN** the TUI opens
- **THEN** the tab bar reads `Agents │ Machines │ Worktrees │ Reviews`

### Requirement: Tree levels per tab
Each tab SHALL have a fixed list of tree levels: Agents = Project → Agent; Machines =
Provider → Machine → Session (panes unfold on Enter); Worktrees = Project → Worktree;
Reviews = Project → Review → Agent.

#### Scenario: Review with agents on two machines
- **WHEN** `!512` has review agents on the laptop and on `ibx-review-512`
- **THEN** both agents appear under `!512`, each row showing its machine, session and
  worktree

### Requirement: Adaptive levels depend only on scope
A tree level SHALL be hidden exactly when the scope pins it (project scope hides Project;
machine scope pinned to a provider hides Provider; pinned to one machine hides Provider and
Machine). The levels MUST NOT change with the data (e.g. a second project appearing).

#### Scenario: Project pinned
- **WHEN** the project scope is `monid-services`
- **THEN** the Agents, Worktrees and Reviews tabs show no Project rows

#### Scenario: Only one project exists
- **WHEN** the project scope is `all` and only one project has rows
- **THEN** the Project level is still shown

### Requirement: Machine scope
The header SHALL show `project: <scope>  machine: <scope>`. The machine scope SHALL cycle
`all → <each configured provider> → all` with `machine.next`/`machine.prev` (default
`]m`/`[m`); `filter.machine` SHALL pin the selected row's machine. The default is `all`.
There SHALL be no machine selector that replaces the view.

#### Scenario: Cycle to a provider
- **WHEN** the user presses `]m` from `machine: all`
- **THEN** the scope becomes `machine: local` and only laptop rows remain

### Requirement: One state vocabulary
Every state SHALL be shown with one icon and one colour per meaning, identical in every
tab: running/working `●` cyan; in progress `◌` dim cyan; needs you `◆` plan / `?` question
/ `!` permission, yellow bold; idle `○` grey; paused `⏸` blue (followed by the last-known
state, dimmed, for agents on paused machines); stopped/gone `■` dim grey; failed `✕` red
bold; done/good `✓` green bold. The column header names what the state is about. The TUI
requires a Nerd Font for type icons (laptop, cloud, terminal, branch, pull request).

#### Scenario: Frozen agent
- **WHEN** an agent waiting on a plan runs on a paused machine
- **THEN** its STATE shows `⏸◆`, with `⏸` blue and `◆` dimmed

### Requirement: Columns
Agents columns SHALL be `AGENT · STATE · TITLE · TASK · WHERE · WORKTREE · REVIEW · AGE`,
where WHERE = machine type icon, machine name and session. Worktrees columns SHALL be
`WORKTREE · BRANCH · SYNC · CHANGES · STATE · REVIEW · AGENTS · MACHINE`. Machines columns
SHALL be `MACHINE/SESSION · STATE · SIZE · IDLE · COST · AGENTS`.

#### Scenario: Remote agent row
- **WHEN** `feat-b` runs on `ibx-feat-b`
- **THEN** its WHERE cell reads `<cloud icon> ibx-feat-b › feat-b`

### Requirement: Remote actions
On an agent row the leader menu SHALL offer attach (waking a paused machine), open plan,
diff (nvim in a new tmux window on that machine), message, rename and close. On a machine
row it SHALL offer shell, pause, resume, keep awake, idle timer, stop/start and delete.

#### Scenario: Diff on a cloud machine
- **WHEN** the user picks diff on `feat-b`
- **THEN** a tmux window running nvim's diff against `origin/<base>` opens on
  `ibx-feat-b` and the user is attached to it
