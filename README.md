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

Requires Rust (stable), `tmux`, `git`, `opencode`, and `glab` / `gh` logged in for the forges you use.

```bash
cargo install --path crates/fleet --locked
cd ~/Projects/my-repo && fleet config --init   # writes ~/.config/fleet/init.lua with this repo as a project
fleet project add ~/Projects/other-repo        # appends another project (reviews source from `origin`)
fleet setup                                    # installs the opencode plugin that reports permission prompts
fleet                                          # the TUI
```

### Config: `~/.config/fleet/init.lua`

Lua 5.4 is embedded (sandboxed: `string`, `table`, `math`, `os.getenv`); the file returns the config table, nvim-style.

```lua
local home = os.getenv("HOME")
return {
  defaults = {
    agent = "opencode",                -- required: adapter preselected by `fleet agent start`
    mode = "plan",                     -- plan | build (every adapter), or an opencode agent (`fleet agent choices`)
    nvim_pane = true, notify = true, refresh_secs = 2, review_ttl_secs = 60,
  },
  -- required: every adapter, explicitly. model = the model for new agents in that CLI's own
  -- naming (unset = the CLI's default); args are appended to every launch.
  agents = {
    opencode = { enabled = true },     -- e.g. model = "anthropic/claude-opus-5-5"
    claude   = { enabled = true },     -- e.g. model = "opus"
    codex    = { enabled = false, sandbox = "workspace-write" },
  },
  -- fleet:projects (fleet project add appends here)
  projects = {
    {
      name = "monid-services",
      repo = home .. "/Projects/monid/monid-services",
      worktrees = home .. "/Projects/monid/worktrees",   -- where `fleet agent start` creates worktrees for THIS project
      reviews = "gitlab:monid-ai/monid-services",         -- or "github:owner/repo"
      base = "main",
    },
  },
  -- fleet:projects:end
  keys = { -- ["agent.start"] = "C-n" },
}
```

Discovery lists every worktree git knows for a project's repo wherever it lives; `worktrees` only decides where new ones go.

### The opencode plugin (`fleet setup`)

opencode's database never records a pending permission prompt ("allow this bash command?"), so without help such an agent looks like `WORKING`. `fleet setup` installs a ~60-line plugin into `~/.config/opencode/plugins/fleet.ts` that writes `~/.local/share/fleet/agents/<pid>.json` on permission/session events. With it, fleet shows `🔒 WAIT:perm` plus the prompt text, and knows each process's exact session. Sessions started before the install keep the old behaviour until restarted (`fleet setup --check` counts them). `fleet setup --remove` uninstalls.

The plugin also opens a small control endpoint on `127.0.0.1` (random port, random bearer token, both in the state file, mode 0600). `fleet agent send` uses it: every message goes through opencode's session API into the session you last worked in, so a plain message and one with `--mode`/`--model` always land in the same session. A plain message keeps the session's agent and model; an override applies to that one turn (the TUI's own selector is unchanged; a toast says so). This works for any opencode process with the plugin, including ones you started by hand. opencode agents without it get tmux keystrokes and can't switch mode or model.

### Modes and models

Every adapter knows `plan` and `build`. `fleet agent choices [--repo P] [--agent A] [--json]` lists what you can pick in a project:

- **opencode**: its primary agents (hidden internal ones dropped) as modes, each with its default model, and every model opencode has credentials for (`opencode models`, run in the repo so its `opencode.json` counts). `--mode` and `--model` are checked against this list.
- **claude / codex**: `plan` and `build`; their CLIs can't list models, so any `--model` name is passed through (`--model` on their command line). They can't switch mode or model for a single message.

The list is cached for 5 minutes (in memory and in `~/.local/share/fleet/cache/`, so separate `fleet` calls share it); a failed lookup is not cached. If the lookup itself fails, `start` and `send` warn and go ahead unchecked.

## Commands

```
fleet [--project NAME]                 the TUI (default scope: the project containing the cwd, else all)
fleet status [--short|--json]          one-shot snapshot (tmux status bar / scripts)
fleet agent start [NAME] [-m TEXT|-f FILE] [--repo P] [--agent opencode|claude|codex] [--mode MODE] [--model MODEL] [--no-nvim]
fleet agent send NAME TEXT [--mode MODE] [--model MODEL]
fleet agent list | choices [--repo P] [--agent A] | attach NAME | rename OLD NEW | stop NAME
fleet review list [--source S] | review start ID|URL        (re-review when it already exists)
fleet worktree list | worktree gc [--yes]                   (dry run first)
fleet worktree close TARGET [--force] [--yes] [--json]      (alias: fleet close; TARGET = task, folder, branch, session or path)
fleet project list | project add [PATH] [--name N] [--reviews S] [--worktrees DIR] [--base B]
fleet tmux list
fleet setup [--remove|--check] | config [--init] | keys
```

`fleet start …` = `agent start`, `fleet review 514` = `review start 514`. `--project` scopes every list command.

### Start a task

`fleet agent start -m "Fix the SFS content-type header"` (no name) creates
`wip-xxxx`; the prompt header tells the agent to pick a name and run
`fleet agent rename wip-xxxx fix-sfs-content-type`, which renames the branch
(`fix/sfs-content-type`), moves the worktree, leaves a symlink at the old path
while the agent is alive (opencode keeps absolute paths), renames the tmux
session and updates the registry. With a name, everything is named up front.

### Close a task

`fleet worktree close feat-x` (alias `fleet close feat-x`; `w x` on any row of that task in the
TUI) finishes a task and removes
everything that belonged to it, in this order: close the registry task, remove rename symlinks,
stop its agents, `git worktree remove`, delete the **local** branch, kill its tmux panes/sessions
(a session that also has panes elsewhere loses only the task's panes). Remote branches are not
touched. It works on any worktree, fleet-started or not; the target is a task name, folder name,
branch, tmux session or path.

It **refuses** (exit 1, nothing changed) and lists exactly what would be lost when:
- the worktree has uncommitted or untracked files;
- the branch is **not merged**: it has commits that are neither in the base branch (`main`:
  local, `origin/main`, or the live remote tip) nor in a merged MR/PR of this branch. Squash
  merges are recognised by asking the forge (`glab`/`gh`), and only when the PR's head commit
  is really on this branch, so an old merged PR of a reused branch name does not count. A
  commit added after the merge still blocks. Pushed is not enough;
- an agent in it is **busy** (working, or waiting on a plan, question or permission). An
  **idle** agent is just stopped, and fleet prints the command that resumes its conversation;
- it is locked (`git worktree lock`).

A detached checkout (`fleet review start` makes `review-N` that way) has no branch: it only
blocks for commits that exist on no remote branch, tag or other local branch. `--force`
closes anyway (TUI: `F` shows in red what will be lost, a second `F` closes; `Enter` does not).
The main clone is never closed. Without `--yes` it asks; without a terminal it is a dry run.

Leftovers close too: when the worktree folder is already gone (deleted by hand, or a close
interrupted half-way), `fleet close <folder name | branch | tmux session>` finds the branch
(registry, else the branch named like the folder that is checked out nowhere), the tmux sessions
whose panes still sit in the vanished folder, and the registry task, and removes what is left.

All-or-nothing: if a step fails, the steps already done are undone in reverse (task reopened,
symlink re-created, worktree re-added from its branch, branch re-created at its old commit) and
fleet says what it could not restore: a stopped agent (its conversation resumes with
`opencode -s` / `claude --resume` / `codex resume <session>`), killed tmux panes, and with
`--force` the uncommitted files. Ignored files (build output) go with the folder. Run inside the
task's own tmux session (or by the task's own agent), fleet kills that session last and the
agent running it after that.

`fleet worktree gc` (TUI `w g`) lists everything that can be cleaned and cleans it in one go
(`--yes`, or `Enter` in the popup): finished worktrees (`merged` MR/PR, `in base` with no
agent, folder missing, stale detached `$TMPDIR` checkout, review gone), each closed exactly
like `fleet worktree close` without `--force` (a blocked one is listed as blocked, with the
reason), plus tmux sessions in deleted folders and registry rows / rename links left behind.
Before applying, gc plans again and skips an item that changed since the dry run (an agent
started there meanwhile). Only `worktree list` and `worktree gc` ask the forge about merged
MRs/PRs per branch; `fleet status` (status bar) makes one call per project.

### tmux status bar and a key to reach fleet

```tmux
set -g status-interval 15
set -g status-right '#(fleet status --short) | %H:%M'

# prefix f from any session: a dedicated `fleet` session running the TUI (created on first
# use, kept between visits). Enter on a row switches to that agent's session; prefix f comes back.
# `fleet:` is the session-only target form: a bare name can match a pane that runs fleet instead.
bind-key f run-shell 'tmux has-session -t fleet: 2>/dev/null || tmux new-session -d -s fleet -c ~ fleet; tmux switch-client -t fleet:'
```

## TUI

Four tabs: **Agents** (flat, one row per running agent, default), **Tmux** (session → panes),
**Worktrees** (project section → worktree → agents), **Reviews** (project section → MR/PR → local
worktrees → agents). Every tab is a tree: `j/k` walk every visible node, `l`/`h` unfold/fold the
selected parent or step into/out of it, `z o`/`z c` all. `Enter` acts on the selected node: attach
for an agent/pane/session/worktree, shell for a worktree with no agent, unfold a review with local
worktrees, start a review agent for one without. The detail panel describes the selected node.

**Projects:** `p`/`P` cycle `project1 → project2 → all`; under `all`, Worktrees and Reviews get one
foldable section per project plus "outside projects". `:project [name|all]` and `--project` set it.

| key | |
|---|---|
| `1`–`4`, `Tab` | tabs |
| `j`/`k`, `g g`/`G`, `C-u`/`C-d` | move |
| `l`/`h`, `z o`/`z c` | unfold/fold one · all |
| `Enter` | attach / shell / unfold / start review (per node) |
| `o`, `c` | open / copy the plan URL (if waiting) else the review URL |
| `Space` | detail panel |
| `/` | live filter · `Esc` clears |
| `p`/`P` | next / previous project |
| `r`, `R`, `S` | refresh now / refresh reviews / cycle sort |
| `:` | run any `fleet …` subcommand, or `:project X` |
| `g a/t/w/v` | this row in Agents / Tmux / Worktrees / Reviews |
| `a a/s/m/n/x/o/c` | agent: attach, start, message, rename, stop, open plan, copy plan |
| `w s/p/P/x/g/c/o` | worktree: shell, push -u, pull --ff-only, close task (popup lists every step and blocker; `F` twice forces), gc, columns, open review |
| `v o/c/r/w/a/f` | review: open, copy, review/re-review, worktrees, agents, filter preset (all/local/mine/others/needs-me/failed) |
| `K`, `n` (Tmux) | kill / rename session |

`?` shows the full map after your `[keys]` overrides; prefix keys open a which-key menu.

**Start popup** (`a s`): name, repo, agent (opencode/claude/codex), mode (`←`/`→` through that agent's modes), model (`Enter` opens a filterable picker: type `opus` or `gpt 5.4`; for claude/codex type the name; `Backspace` = default), prompt. **Message popup** (`a m`): `Tab` cycles the mode (`unchanged` → each mode), `C-o` picks a model; both default to unchanged, so a plain `Enter` sends as before.

## How it works

`fleet-core` (no terminal code) runs three collector families concurrently and
joins them local-first:

| family | adapter | reads |
|---|---|---|
| agents | opencode | process table, `~/.local/share/opencode/opencode.db` (read-only; session, activity, model of the last message), the plugin's state files (`agents/<pid>.json`, incl. its control endpoint), the HTTP API of fleet-started agents, a `GET /api/plan` probe on each listening port (Plannotator) |
| reviews | gitlab, github | `glab api` / `gh pr list` + GraphQL threads: open MRs/PRs, approvals, resolved/unresolved threads (cached 60 s, refreshed in the background) |
| worktrees | git | `git worktree list --porcelain`, status, ahead/behind |

plus the tmux helper (`list-panes -a`). The join walks pid → tmux pane → cwd →
worktree → branch → review. Every type in `fleet_core::model` is `Serialize`,
so `fleet status --json` is the same data the TUI renders; a `fleet serve`
for other frontends is the planned next step. Status rules live in
`status.rs` and are unit-tested on fixtures captured from a real machine.

See `DESIGN.md` for the approved design (screens, state model, sequence diagrams).

## Status

v0.2, built for one person's workflow (opencode + tmux + GitLab/GitHub). Issues and PRs welcome at
[github.com/FeiyouG/fleet](https://github.com/FeiyouG/fleet).

## License

[MIT](LICENSE)
