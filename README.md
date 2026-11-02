# fleet

A terminal UI that tracks and launches your local coding agents across tmux,
git worktrees and code reviews. Built for one workflow: many `opencode`
sessions in tmux, one worktree each, GitLab merge requests, Plannotator plan
reviews in the browser.

```
 fleet [1 Overview] 2 Tmux  3 Worktrees  4 Reviews    22 worktrees · 25 agents · 7 need you · 0 failed · 0 ready · reviews 6s ago
 OVERALL    NAME                       BRANCH                             AGENT                  TMUX                        REVIEW
▸NEEDS YOU  monid-services             main =                             build ⏳ WAIT:plan +3  monid-services:2.0 ○        —
 NEEDS YOU  monid-services-admin-users update/admin-users-paging ↕5/4 *   build ❓ WAIT:question monid-services-admin-us… ○  !537 open – 👍0 💬5 ✘3
 NEEDS YOU  firecrawl-monitor          feat/firecrawl-monitor =           plan  ⏳ WAIT:plan     22:1.1 ○                    !482 open – 👍1 💬0
 WORKING    monid-signup-credit-codes  feat/monid-signup-credit-… = *     build ● WORKING        monid-signup-credit-att… ●  !534 open – 👍0 💬15 ✘5
 IDLE       sfs-content-type           fix/sfs-content-type =             build ○ IDLE           monid-sfs-content-type:… ○  !530 open – 👍0 💬13 ✘4
 DEAD       wt-head                    (detached)                         —     NONE             —                           —
```

What it answers, without opening anything:

- which agent is **waiting for me** (a plan to approve, a question, a permission prompt), and the Plannotator URL to reopen it — even after the tab was closed;
- which worktree is **unsynced / dirty**, and which MR it belongs to;
- which open MR is **approved / failed / has unresolved threads**, and whether I have a local worktree and agent on it;
- which tmux session hosts which agent, and how to jump there.

It also starts work: one command creates branch + worktree + tmux session + agent, and one command starts a review agent for a teammate's MR.

## Install

Requires Rust (stable), `tmux`, `git`, `glab` (logged in), `opencode`.

```bash
cargo install --path crates/fleet --locked
fleet config --init          # writes ~/.config/fleet/config.toml
$EDITOR ~/.config/fleet/config.toml
fleet                        # the TUI
```

Config (XDG paths on every platform, `~/.config/fleet/config.toml`):

```toml
worktrees_dir = "~/.local/share/fleet/worktrees"   # where `fleet agent start` puts worktrees
review_ttl_secs = 60
refresh_secs = 2
default_mode = "plan"      # opencode agent to start with: plan | build
nvim_pane = true
notify = true              # macOS banner when an agent starts waiting for you

[[repos]]
name = "monid-services"
path = "~/Projects/monid/monid-services"
review_source = "gitlab:monid-ai/monid-services"
base = "main"

[keys]                     # any action -> "key" or "prefix key"; `fleet keys` lists them
# "agent.start" = "C-n"
```

## Commands

```
fleet                                  the TUI
fleet status [--short|--json]          one-shot snapshot (tmux status bar / scripts)
fleet agent start [NAME] [-m TEXT|-f FILE] [--repo R] [--mode plan|build] [--no-nvim]
fleet agent list | attach NAME | send NAME TEXT | rename OLD NEW | stop NAME
fleet review list [--source S] | review start ID|URL        (re-review when it already exists)
fleet worktree list | worktree gc [--yes]                   (dry run first)
fleet tmux list
fleet config [--init] | keys
```

`fleet start …` = `agent start`, `fleet review 514` = `review start 514`.

### Start a task

`fleet agent start -m "Fix the SFS content-type header"` (no name) creates
`wip-xxxx`; the prompt header tells the agent to pick a name and run
`fleet agent rename wip-xxxx fix-sfs-content-type`, which renames the branch
(`fix/sfs-content-type`), moves the worktree, leaves a symlink at the old path
while the agent is alive (opencode keeps absolute paths), renames the tmux
session and updates the registry. With a name, everything is named up front.

Fleet-started agents run with `--port`, so fleet also sees permission prompts
and can message them over opencode's HTTP API. Agents started by hand are
discovered too (state from opencode's SQLite DB + a Plannotator port probe);
they only lack the permission-prompt state.

### tmux status bar

```tmux
set -g status-interval 15
set -g status-right '#(fleet status --short) | %H:%M'
```

## TUI keys

Single keys for the common actions, prefix menus (which-key style) for the
rest. `?` shows the full map after your overrides.

| key | |
|---|---|
| `1`–`4`, `Tab` | tabs: Overview, Tmux, Worktrees, Reviews |
| `j`/`k`, `g g`/`G` | move |
| `Enter` | attach (tmux) / shell here / review |
| `o`, `c` | open / copy the plan URL (if waiting) else the review URL |
| `Space` | detail panel |
| `/` | live filter · `Esc` clears |
| `r`, `R` | refresh now / refresh reviews now |
| `S` | cycle sort |
| `:` | run any `fleet …` subcommand |
| `g o/t/w/v` | this row in Overview / Tmux / Worktrees / Reviews |
| `a a/s/m/n/x/o/c` | agent: attach, start, message, rename, stop, open plan, copy plan |
| `w s/p/P/D/g/c/o` | worktree: shell, push -u, pull --ff-only, remove, gc, columns, open review |
| `v o/c/r/w/a/f/S` | review: open, copy, review/re-review, worktrees, agents, filter preset, source |
| `K`, `n` (Tmux tab) | kill / rename session |

## How it works

`fleet-core` (no terminal code) runs three collector families concurrently and
joins them local-first:

| family | adapter | reads |
|---|---|---|
| agents | opencode | process table, `~/.local/share/opencode/opencode.db` (read-only), the HTTP API of fleet-started agents, a `GET /api/plan` probe on each listening port (Plannotator) |
| reviews | gitlab | `glab api`: open MRs, approvals, discussions (cached 60 s, refreshed in the background) |
| worktrees | git | `git worktree list --porcelain`, status, ahead/behind |

plus the tmux helper (`list-panes -a`). The join walks pid → tmux pane → cwd →
worktree → branch → review. Every type in `fleet_core::model` is `Serialize`,
so `fleet status --json` is the same data the TUI renders; a `fleet serve`
for other frontends is the planned next step. Status rules live in
`status.rs` and are unit-tested on fixtures captured from a real machine.

See `DESIGN.md` for the approved design (screens, state model, sequence diagrams).
