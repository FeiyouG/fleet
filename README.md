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
    mode = "plan",                     -- opencode agent for new tasks (`fleet agent choices` lists them)
    -- model = "anthropic/claude-opus-5-5",  -- model for new tasks; unset = the agent's own, else opencode's default
    nvim_pane = true, notify = true, refresh_secs = 2, review_ttl_secs = 60,
  },
  agent_args = {},                     -- extra args for opencode
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

The plugin also opens a small control endpoint on `127.0.0.1` (random port, random bearer token, both in the state file, mode 0600). `fleet agent send` uses it: a plain message is typed into the agent's TUI; a message with `--model`/`--agent` goes through opencode's session API so that turn uses that model/agent (the TUI's own selector is unchanged; a toast says so). This works for any opencode process with the plugin, including ones you started by hand. Agents without it get tmux keystrokes and can't switch model.

### Agents and models

`fleet agent choices [--repo P] [--json]` lists what you can pick in a project: the primary opencode agents (hidden internal ones dropped) with each one's default model, and every model opencode has credentials for (`opencode models`, run in the repo so its `opencode.json` counts). The list is the same for every opencode agent; only the default differs (the agent's own `model`, else the global one). The TUI loads it in the background and caches it for 5 minutes.

## Commands

```
fleet [--project NAME]                 the TUI (default scope: the project containing the cwd, else all)
fleet status [--short|--json]          one-shot snapshot (tmux status bar / scripts)
fleet agent start [NAME] [-m TEXT|-f FILE] [--repo P] [--mode AGENT] [--model PROVIDER/MODEL] [--no-nvim]
fleet agent send NAME TEXT [--model PROVIDER/MODEL] [--agent AGENT]
fleet agent list | choices [--repo P] | attach NAME | rename OLD NEW | stop NAME
fleet review list [--source S] | review start ID|URL        (re-review when it already exists)
fleet worktree list | worktree gc [--yes]                   (dry run first)
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

### tmux status bar

```tmux
set -g status-interval 15
set -g status-right '#(fleet status --short) | %H:%M'
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
| `w s/p/P/D/g/c/o` | worktree: shell, push -u, pull --ff-only, remove, gc, columns, open review |
| `v o/c/r/w/a/f` | review: open, copy, review/re-review, worktrees, agents, filter preset (all/local/mine/others/needs-me/failed) |
| `K`, `n` (Tmux) | kill / rename session |

`?` shows the full map after your `[keys]` overrides; prefix keys open a which-key menu.

**Start popup** (`a s`): name, repo, agent (`←`/`→` through the discovered agents), model (`Enter` opens a filterable picker: type `opus` or `gpt 5.4`; `Backspace` = default), prompt. **Message popup** (`a m`): `Tab` cycles the agent (`unchanged` → each agent), `C-o` picks a model; both default to unchanged, so a plain `Enter` sends as before.

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
