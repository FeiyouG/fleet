# fleet

A terminal UI that tracks and launches your local coding agents across tmux,
git worktrees and code reviews. Built for one workflow: many `opencode`
sessions in tmux, one worktree each, GitLab merge requests, Plannotator plan
reviews in the browser.

```
 fleet [1 Agents] 2 Machines  3 Worktrees  4 Reviews   project: all  machine: all ▸ 25 agents · 7 need you · 4 working · 14 idle
 AGENT              STATE         TITLE                         TASK              WHERE                          WORKTREE                REVIEW
▸▾ monid-services                 18 agents · 5 need you · 3 working
   ↳ opencode build ◆ WAIT:plan   Admin users paging            admin-users       󰌢 laptop › admin-users ○       admin-users ↕5/4 *      !537 open – 👍0 💬5 ✘3
   ↳ opencode plan  ? WAIT:quest… Firecrawl monitor             firecrawl-monitor 󰅟 ibx-firecrawl › firecr… ○   firecrawl-monitor =     !482 open – 👍1 💬0
   ↳ opencode build ⏸● WORKING    Signup credit codes           signup-credits    󰅟 ibx-signup › signup-cr… ○   signup-credits = *      !534 open – 👍0 💬15 ✘5
```
What it answers, without opening anything:

- which agent is **waiting for me** (a plan to approve, a question, a permission prompt), and the Plannotator URL to reopen it — even after the tab was closed;
- which worktree is **unsynced / dirty**, and which MR it belongs to;
- which open MR is **approved / failed / has unresolved threads**, and whether I have a local worktree and agent on it;
- which machine (this laptop or a cloud machine) and tmux session hosts which agent, and how to jump there.

It also starts work: one command creates branch + worktree + tmux session + agent, and one command starts a review agent for a teammate's MR.

## Install

Requires Rust (stable), `tmux`, `git`, at least one coding agent CLI (`opencode`, `claude` or
`codex`), `glab` / `gh` logged in for the forges you use, and a
[Nerd Font](https://www.nerdfonts.com) in the terminal (the TUI's type icons).

```bash
cargo install --git https://github.com/FeiyouG/fleet fleet --locked
```

## Getting started

fleet ships every default except two things only you know: **which agent CLI you have** and
**which repos you work in**. Create `~/.config/fleet/init.lua` with this (keep the agents you
actually have installed):

```lua
-- ~/.config/fleet/init.lua
local home = os.getenv("HOME")

return {
  -- 1. Your agents: a name of your choice -> which harness it runs on, and optionally a model
  --    in that harness's own naming. Define at least one.
  agents = {
    opencode = { harness = "opencode" },              -- e.g. model = "anthropic/claude-opus-5-5"
    -- claude = { harness = "claude" },               -- if you use Claude Code; e.g. model = "opus"
    -- codex  = { harness = "codex" },                -- if you use Codex;       e.g. model = "gpt-5.4"
  },

  -- 2. Which agent new tasks use (one of the names above) and the mode they start in.
  defaults = { agent = "opencode", mode = "plan" },   -- plan (read-only) | build | yolo (no prompts)

  -- 3. Your repos. `fleet project add <path>` appends one for you (reviews from `origin`).
  projects = {
    { name = "my-repo",
      repo = home .. "/Projects/my-repo",
      worktrees = home .. "/Projects/my-repo-worktrees",  -- where fleet puts new task worktrees
      reviews = "github:owner/my-repo",                   -- or "gitlab:group/project"; omit = no reviews
      base = "main" },
  },
}
```

Then:

```bash
fleet config --check   # validates it: fatal problems stop fleet, warnings are listed
fleet setup            # installs the hook/plugin for each agent you defined (permission prompts)
fleet                  # the TUI; Space opens the menu for the selected row, Space t the tasks
```

`fleet config --init` writes this file for you with the current repo as the first project.
Everything else — tasks, keys, prompts, modes — has a shipped default you can see with
`fleet config --default` and override piece by piece (next section).

### Config: `~/.config/fleet/init.lua`

fleet ships a complete default configuration (`config/default/` in this repo, embedded in the
binary). Your `~/.config/fleet/init.lua` is **deep-merged over it**: tables merge key by key,
scalars and lists replace. Write only what differs. `fleet config --default` prints the shipped
tree (the reference and the template), `fleet config --effective` the merged result,
`fleet config --check` validates yours.

Lua 5.4, sandboxed: `string`, `table`, `math`, `os.getenv`, plus `require("name")` (loads
`name/mod.lua` or `name.lua` next to the calling file) and `read("file.md")` (the text of a file
next to the calling file). Each section can be its own folder with a `mod.lua`, and a prompt
lives beside the Lua that reads it — no `[[ … ]]` long strings.

Three layers: a **harness** is an adapter in code (`opencode`, `claude`, `codex`); an **agent**
is your named configuration of a harness (`agents.<name>`); a **task** is a preset for starting
an agent (`tasks.<name>`). Every section can be overridden per project.

```lua
-- ~/.config/fleet/init.lua
local home = os.getenv("HOME")
return {
  -- defaults = { agent = "opencode", mode = "plan" },         -- see `fleet config --default`
  agents = {
    opencode = { model = "anthropic/claude-opus-5-5" },         -- model in the harness's own naming
    -- fast = { harness = "opencode", model = "anthropic/claude-sonnet-4-5" },
    -- per agent: harness, enabled, bin, args, model, skills = { "/abs/SKILL.md" }, system = "…"
  },
  tasks = require("tasks"),                                     -- ~/.config/fleet/tasks/mod.lua, see below
  projects = {
    { name = "monid-services",
      repo = home .. "/Projects/monid/monid-services",
      worktrees = home .. "/Projects/monid/worktrees",          -- where new worktrees go
      reviews = "gitlab:monid-ai/monid-services",                -- or "github:owner/repo"; omit = no Reviews for it
      base = "main",
      -- any section, for this project only:
      -- defaults = { agent = "claude", mode = "build" },
      -- agents = { opencode = { model = "anthropic/claude-sonnet-4-5" } },
      -- tasks = { review = { skills = { … } }, release = { key = "R", skills = { … } } },
      -- keys = { ["tab.reviews"] = "5" },
    },
  },
  -- keys = { ["agent.stop"] = "K" },                           -- `fleet keys` lists every action
}
```

```lua
-- ~/.config/fleet/tasks/mod.lua
local skills = os.getenv("HOME") .. "/.config/opencode/skills"
return {
  start  = { skills = { skills .. "/implement-change/SKILL.md" } },
  review = { skills = { skills .. "/review-team-mr/SKILL.md" }, instructions = read("review.md") },
  bugfix = { key = "b", mode = "build", prompt = "Fix the bug described below.",
             instructions = read("bugfix.md") },
}
```

Discovery lists every worktree git knows for a project's repo wherever it lives; `worktrees`
only decides where new ones go. A project without `reviews` has no Reviews section and
review tasks are greyed out for it.

### Tasks

A task is what `Space t <key>` in the TUI and `fleet task <name>` on the CLI start. Fields:
`key` (the key after `Space t`), `on` (`project` → a new worktree from `base`; `review` → the
selected MR/PR head, detached), `agent`, `mode`, `model`, `skills` (absolute `SKILL.md` paths),
`instructions` (text after fleet's system text; keep it in a `.md` and `read()` it), `prompt`
(preset, editable in the popup), `label`. Three ship: `start` (`s`), `review` (`r`, plan mode,
with review instructions), `explain` (`e`, plan mode: explain a part of the codebase to a
newcomer with diagrams and `file:line` excerpts).

Resolution for every field: CLI flag / popup edit → project task → global task → project
agent → global agent → project defaults → global defaults (your file over the shipped one at
each level). Skills accumulate (agent + task); everything else: first one wins.

What the agent receives: fleet's system text (`defaults.system`, placeholders `{slug} {repo}
{branch} {base} {agent} {review_id} {review_url} {review_title} {source_branch}
{target_branch}`), the rename line for a `wip-*` slug, the review line for review tasks,
`Skills to read and follow first: …`, the agent's `system`, the task's `instructions`, then the
prompt. Claude Code gets the system part through `--append-system-prompt`; opencode and
Codex get it prepended to the first message. A skill outside the harness's own skill folders
is symlinked into them so the agent can load it by name.

### Machines (laptop and cloud)

A **machine profile** says where a task runs: `machines.<name> = { provider = …, … }`, picked
like agents (`--machine` → project task → task → project defaults → `defaults.machine`). Two
providers are built in: `local` (this computer; the shipped `laptop` profile and default) and
`instabox` ([InstaBox](https://instabox.sh) cloud machines, over its HTTP API — no CLI needed).

```lua
machines = {
  ibx = {
    provider = "instabox",                 -- API key from $INSTABOX_API_KEY (api_key_env)
    snapshot = "fleet-monid-services",     -- or template = "agent-browser"
    size = { vcpus = 4, mem_mib = 8192 },
    per_task = true,                       -- one machine per agent (default); false = shared
    idle_min = 30, max_run_min = -1,       -- paid plans only; leave out on the free plan
    secrets = { "GITLAB_TOKEN" },          -- InstaBox Org Secrets (proxied; names only)
    files = { [".local/share/opencode/auth.json"] = "OPENCODE_AUTH" },
  },
},
tasks = { review = { machine = "ibx" } },
```

On a cloud machine, a start creates (or reuses) the machine, installs tmux if missing, sets
git credentials to read `GITLAB_TOKEN` / `GITHUB_TOKEN`, clones the project over HTTPS into
`workdir` (default `/data/home/src`), then makes the same worktree, tmux layout and agent as
on the laptop. tmux is required on every machine. Agent names are unique across all machines.
InstaBox pauses an idle machine itself (memory kept, so tmux and the agent survive); attaching
or `fleet machine shell` wakes it. Fleet never pauses or stops a machine on its own.

**Secrets (entered in InstaBox by hand, for now).** The profile names secrets; you create them
in InstaBox (Settings → Secrets). Nothing refreshes a pasted value, so paste long-lived ones:

| Secret | Paste | Not |
|---|---|---|
| `GITLAB_TOKEN` (proxy) | a GitLab personal/project access token (`api`, `write_repository`) | glab's OAuth token (expires in ~2 h) |
| `OPENCODE_AUTH` (file) | an `auth.json` with API keys only, e.g. `{"anthropic":{"type":"api","key":"sk-ant-…"}}` | your laptop's OAuth `auth.json` (access token < 8 h; a refresh token there can sign your laptop out) |

To use an OAuth subscription anyway, `fleet creds sync --profile ibx` uploads the laptop file
with refresh tokens removed; re-run it at least every 8 h. The InstaBox API key stays in your
laptop shell (`INSTABOX_API_KEY`), never in the config. Provider-neutral credentials (fleet
pushing the laptop's own logins to any machine) are planned: `openspec/changes/add-remote-machines/design.md` Q7.

`fleet machine ls | create PROFILE | pause | resume | stop | start | rm | keep M [1h] | release |
idle M 10m|off | max-run M 4h|off | shell M [--print] [-- CMD…] | doctor M | url M PORT`.
Attach uses the system `ssh` (or `mosh`) with a short-lived certificate fleet requests for its
own key in `~/.local/share/fleet/instabox/`, and checks the gateway's host CA strictly.
Re-reviews, `fleet agent attach|send|stop|diff|rename` and `fleet close` work on cloud tasks
too. Each machine publishes its agents' state into its InstaBox metadata, so the TUI shows remote
agents without waking a paused machine (their state is shown frozen, `⏸`).

`fleet machine snapshot PROFILE` builds the profile's snapshot (tmux, fleet, the repo clone,
the agent config, `setup` commands). `fleet creds sync [--profile P] [--watch]` copies the
laptop's model credentials into InstaBox secrets with refresh tokens stripped: only the laptop
refreshes OAuth tokens.

### The opencode plugin (`fleet setup`)

opencode's database never records a pending permission prompt ("allow this bash command?"), so without help such an agent looks like `WORKING`. `fleet setup` installs a ~60-line plugin into `~/.config/opencode/plugins/fleet.ts` that writes `~/.local/share/fleet/agents/<pid>.json` on permission/session events. With it, fleet shows `! WAIT:perm` plus the prompt text, and knows each process's exact session. Sessions started before the install keep the old behaviour until restarted (`fleet setup --check` counts them). `fleet setup --remove` uninstalls.

The plugin also opens a small control endpoint on `127.0.0.1` (random port, random bearer token, both in the state file, mode 0600). `fleet agent send` uses it: every message goes through opencode's session API into the session you last worked in, so a plain message and one with `--mode`/`--model` always land in the same session. A plain message keeps the session's agent and model; an override applies to that one turn (the TUI's own selector is unchanged; a toast says so). This works for any opencode process with the plugin, including ones you started by hand. opencode agents without it get tmux keystrokes and can't switch mode or model.

### Modes and models

Three modes everywhere: `plan` (read-only), `build` (edits in the worktree; the harness asks
before risky steps), `yolo` (no prompts at all, full access — opencode `--agent build`, Claude
`--permission-mode bypassPermissions`, Codex `--sandbox danger-full-access --ask-for-approval
never`). How a mode maps to a harness's switches is adapter code, not config. opencode also
offers its own primary agents as extra modes (`fleet agent choices`).

Models are set per agent in the harness's own naming (`agents.opencode.model =
"anthropic/claude-opus-5-5"`, `agents.claude.model = "opus"`); unset = the harness's default.
`fleet agent choices [--repo P] [--agent A]` lists what a harness offers in a project
(opencode: agents + models with credentials; claude/codex: plan/build/yolo, any model name
passed through). Cached 5 minutes.

## Commands

```
fleet [--project NAME]                 the TUI (default scope: the project containing the cwd, else all)
fleet status [--short|--json]          one-shot snapshot (tmux status bar / scripts)
fleet task NAME [SLUG|REVIEW-ID] [-m TEXT|-f FILE] [--repo P] [--agent A] [--mode M] [--model ID] [--skill PATH…|--no-skills]
fleet agent start [NAME] [-m TEXT|-f FILE] [--repo P] [--task T] [--agent A] [--mode plan|build|yolo] [--model ID] [--skill PATH…] [--left-pane CMD|--no-left-pane]
fleet agent send NAME TEXT [--mode MODE] [--model MODEL]
fleet agent list | choices [--repo P] [--agent A] | attach NAME | rename OLD NEW | stop NAME
fleet review list [--source S] | review start ID|URL [--task T] [--agent A] [--skill PATH…]   (re-review when it already exists)
fleet worktree list | worktree gc [--yes]                   (dry run first)
fleet worktree close TARGET [--force] [--yes] [--json]      (alias: fleet close; TARGET = task, folder, branch, session or path)
fleet project list | project add [PATH] [--name N] [--reviews S] [--worktrees DIR] [--base B]
fleet tmux list
fleet machine ls | create | pause | resume | stop | start | rm | keep | release | idle | max-run | shell | snapshot | doctor | url   (and --machine P on start/task/review)
fleet agent diff NAME                  nvim diff against the base, on the task's machine
fleet creds sync [--profile P] [--watch]
fleet setup [--remove|--check] | config [--init|--default|--effective|--check] | keys [--project P]
```

`fleet start …` = `agent start` = `fleet task start …`; `fleet review 514` = `review start 514` = `fleet task review 514`. `--project` scopes every list command.

### Start a task

`fleet agent start -m "Fix the SFS content-type header"` (no name) creates
`wip-xxxx`; the prompt header tells the agent to pick a name and run
`fleet agent rename wip-xxxx fix-sfs-content-type`, which renames the branch
(`fix/sfs-content-type`), moves the worktree, leaves a symlink at the old path
while the agent is alive (opencode keeps absolute paths), renames the tmux
session and updates the registry. With a name, everything is named up front.

In the TUI (`Space t s`) the prompt box takes multi-line text: paste as much as you like (bracketed
paste, every line kept; the title shows the line count), type Enter for a new line, `C-e` to
write it in `$EDITOR`, `C-s` to start. `-f FILE` (`-` = stdin) does the same on the CLI.

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

Four tabs: **Agents** (project section → agent, default), **Machines** (provider → machine →
tmux session → panes), **Worktrees** (project section → worktree), **Reviews** (project section →
MR/PR → agents, plus local worktrees with no agent). A level hides only when the scope pins it
(a project scope hides project sections; a machine scope hides providers and machines), never
because of the data. Every tab is a tree: `j/k` walk every visible node, `l`/`h` unfold/fold the
selected parent or step into/out of it, `z o`/`z c` all. `Enter` acts on the selected node: attach
for an agent/pane/session/worktree (on its machine, waking it if paused), a shell for a machine or
a worktree with no agent, unfold a review with local worktrees, start a review agent for one
without. The detail panel describes the selected node.

**State:** one icon and one colour per meaning in every tab: `●` running/working (cyan), `◌` in
progress (dim cyan), `◆` plan / `?` question / `!` permission = needs you (yellow), `○` idle
(grey), `⏸` paused (blue; an agent on a paused machine shows `⏸` then its last state, dimmed),
`■` stopped/gone, `✕` failed (red), `✓` done/good (green). Type icons (laptop, cloud, terminal,
branch, pull request) need a [Nerd Font](https://www.nerdfonts.com) in the terminal.

**Machines:** `]m`/`[m` cycle the machine scope `all → each provider → all`; `M` pins the
selected row's machine. The header shows both scopes: `project: … machine: …`.

**Projects:** `p`/`P` cycle `project1 → project2 → all`; under `all`, Worktrees and Reviews get one
foldable section per project plus "outside projects". `:project [name|all]` and `--project` set it.

A Vim user should be able to drive fleet blind. Motions are Vim's; `Space` is the **leader**
and opens a menu for the selected row showing only what applies to it; `Space t` opens the
tasks menu. Every key below comes from the config (`fleet keys`, `fleet keys --project P`).

| key | |
|---|---|
| `1`–`4`, `Tab`/`S-Tab` | tabs |
| `j`/`k`, `g g`/`G`, `C-u`/`C-d` | move |
| `l`/`h`, `z o`/`z c` | unfold/fold one · all |
| `/` · `f`/`F` · `*` | filter (`Esc` clears) · next/previous match (no filter: next row that needs you) · scope to the row's project |
| `Enter` | attach / shell / unfold / start review (per row) |
| `o`, `y` | open / yank (copy) the plan URL if waiting, else the review URL |
| `i` | detail panel |
| `[p` / `]p` | previous / next project scope |
| `[m` / `]m`, `M` | previous / next machine scope · pin this row's machine |
| `r`, `R`, `S` | refresh now / refresh reviews / cycle sort |
| `:` | run any `fleet …` subcommand, or `:project X` |
| `g a/m/w/v` | this row in Agents / Machines / Worktrees / Reviews |
| `Space` … agent row | `a` attach · `m` message · `s` stop · `n` rename · `p` open plan · `y` copy plan · `d` diff (any machine) |
| `Space` … worktree row (and agent rows) | `S` shell · `x` close task · `u`/`U` push -u / pull --ff-only · `o` open review |
| `Space` … review row | `r` review / re-review · `o` open · `y` copy · `w` worktrees · `a` agents · `f` filter preset |
| `Space` … session row | `x` kill session · `n` rename |
| `Space` … machine row | `S` shell · `z` pause · `r` resume · `k` keep awake 1h · `i` idle pause 10m → 30m → off · `s` stop/start · `D` delete |
| `Space` … any row | `g` gc · `c` columns · `t` tasks → each task's `key` (`s` start, `r` review, `e` explain, yours) |

`?` shows the effective map. A key value that is a chord (`"a s"`) or a modified key (`C-n`) is
a top-level binding, so old-style overrides keep working.

**Start popup** (`Space t s`, or any project task): the title names the task and its skills; name, repo, agent (opencode/claude/codex), mode (`←`/`→` through that agent's modes), model (`Enter` opens a filterable picker: type `opus` or `gpt 5.4`; for claude/codex type the name; `Backspace` = default), prompt. **Message popup** (`Space m`): `Tab` cycles the mode (`unchanged` → each mode), `C-o` picks a model; both default to unchanged, so a plain `Enter` sends as before.

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
