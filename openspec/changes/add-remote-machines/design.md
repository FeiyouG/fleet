# Design: remote machines

Source: the approved plan (2026-11-09). Open questions Q1, Q2a, Q3 and Q6 were approved with
their **recommended** options: criteria as written; always a git worktree (tab stays
Worktrees); machines push their agent state into provider metadata; the laptop is the only
OAuth refresher and machines get access tokens only. Revisit here if any changes.

Tags: [LIVE] checked against InstaBox with a test key (read-only); [DOCS] vendor docs;
[DATA] the author's local opencode history (Oct 1 – Nov 9 2026).

## Background (terms used below)

- **Provider.** One adapter implementation: `local` (this computer) or `instabox`. It is the machine counterpart of an agent *harness* (`opencode`, `claude`, `codex`).
- **Machine profile.** A named entry under `machines = { … }` in `init.lua`. It is the recipe for a machine, e.g. `ibx` = InstaBox, 4 vCPU / 8 GiB, these secrets. It is the counterpart of an agent entry under `agents = { … }`.
- **Machine.** One real computer: your laptop, or InstaBox sandbox `ibx_123`. It has a status: running, paused, stopped and so on.
- **Session.** A tmux session on a machine. Your laptop has many sessions; a cloud machine usually has one.
- **Workspace.** A checkout that an agent edits on a machine. On the laptop it is a git worktree, as today. On a cloud machine it is a git worktree inside that machine, e.g. `/data/home/wt/fix-x`.
- **Pause.** The machine is frozen with memory kept, so its sessions and agents survive. Only disk is billed.

Tags: **[LIVE]** = checked today with your key (read-only). **[DOCS]** = vendor docs. **[DATA]** = your local opencode history.

---

## Decisions

## Q0. What your history says  [DATA]

This comes from the opencode database: Oct 1 to Nov 9, 303 top-level sessions. It is scaled to 10–15 tasks a day.

| Finding | Number | Consequence |
|---|---|---|
| Agents working at the same time | 2 typical, 5 at p90, 10–15 at peak today; about 2× at the new volume | up to about 20 cloud machines awake at peak (InstaBox Pro allows 100) |
| Repos and work types | 89% monid-services; 37% MR reviews | one prebuilt monid-services snapshot covers almost everything |
| Plan review | 762 `submit_plan` calls; you reply in 4.4 min median, 3.1 h at p90 | remote plan review, and pausing while it waits, are core |
| Gaps between turns | 18% over 8 h; 12% of sessions resumed after more than 24 h | pause must keep tmux and opencode alive |
| LLM spend | $223/day | machine cost (about $50 a month on InstaBox) is noise next to it |
| Tools | docker, temporal, localstack, glab, aws; MCP: posthog, productlane, clickhouse, grafana, notion, clerk | each machine needs Docker, GitLab access and the MCP logins |
| Where you act | `Space t` presets, attach, open plan, close; agents found outside Fleet show up too | the UI must keep untracked sessions and agents visible |

## Q1. Criteria

**Musts:**
- SSH and tmux;
- pause that keeps processes, and wakes on attach;
- built-in secret delivery;
- Docker with 16 GiB or more;
- a private URL for a port;
- templates.

**Shoulds:** a working agent is never paused; usable from Sydney; an API plus webhooks; fork; maturity.

**Nice:** price.

**[LIVE] facts about InstaBox:**
- **Plan.** You are on the **Free** plan:
  - 1 machine, 4 vCPU / 8 GiB only;
  - idle pause fixed at 30 min;
  - each run is capped at 2–8 h, then a cooldown of 5–20 min;
  - enough for the spike. `services:dev` probably needs Pro ($20 a month, up to 16 GiB).
- **Location.** The API sits in AWS us-west-2 (Oregon), about 175 ms round trip from Sydney. Where the machines themselves run gets measured in the spike. mosh is supported, which hides typing lag.


## Q2. Concepts: names, states, icons, and the screens

### Definitions (in config and code; no tab)

| Concept | What it is | Example |
|---|---|---|
| Harness | a coding-agent program (adapter, built in) | `opencode`, `claude`, `codex` |
| Machine adapter (provider) | where machines come from (adapter, built in) | `local`, `instabox` |
| Agent profile `agents.<name>` | your settings for a harness | `opencode` = opencode + claude-opus-5-5 |
| Machine profile `machines.<name>` | your recipe for a machine | `laptop` = local; `ibx` = InstaBox 4/8 + secrets + snapshot |
| Task `tasks.<name>` | a **template**: the target (a project or an MR), mode, skills, instructions, agent profile, machine profile, key | `start`, `review`, `explain` |

### Runtime things

```
Task template ── you start it ──▶ Agent  = a harness running that template with your prompt
                                    ├ runs on   Machine › Session (tmux)
                                    ├ edits    Worktree (on the same machine)
                                    └ about    Review (optional)
```

- A **task** is a recipe and has no state.
- An **agent** is one run of a recipe. It has a name, a title and a state.
- One MR can therefore have several agents: several runs of `review`, possibly on different machines.

### Names

| Thing | Name (identifier) | Title (free text) | Created | Unique? |
|---|---|---|---|---|
| Agent | the slug `fix-a`. You type it at start; otherwise Fleet picks `wip-xxxx` and the agent renames itself (prompt header, `cmd/start.rs:42-53`) | the harness's session title, e.g. "Fix rate limiter off-by-one" | at start (`Space t <key>`, `fleet task`) | **yes**, across all machines. Start checks the registry (today only local paths, `cmd/start.rs:80-85`) |
| Machine | the laptop is `laptop`. Cloud: `<profile>-<agent>` per task (`ibx-fix-a`), `<profile>-<n>` when shared | laptop: hostname. Cloud: profile + size | laptop: always exists. Cloud: at start, unless a reusable machine exists | yes. InstaBox enforces names per org; the `ibx_…` id appears only in the detail panel |
| Session | = the agent's name (as today) | — | at start, on the agent's machine | per machine (tmux enforces it) |
| Worktree | = the agent's name, or `review-<id>` for reviews (as today) | its branch (`fix-a` → `fix/a`, `slug.rs:32`) | at start, on the agent's machine | per machine. Fleet-made names are unique everywhere; foreign ones always show with their machine |
| Review | `!512` / `#12` | the MR title | on the forge | per project |

## Q2a. Worktree or Workspace?

### Options compared

| | A. Always a git worktree; keep the name **Worktrees** | B. **Workspace** interface; a worktree is one kind (others: a fresh clone, a plain folder) |
|---|---|---|
| What you get | Every agent works in a git worktree on its machine. The cloud snapshot carries the main clone | A cloud machine could `git clone` instead; non-git folders count too |
| Pros | One word, no new concept. All of today's worktree logic (sync, gc, close, rename) is reused. A clone plus `git worktree add` takes seconds | Works without a prebuilt snapshot; leaves room for non-git projects |
| Cons | Needs the main clone first (the snapshot has it; a bare machine clones once) | A new word and interface for a case we don't have. Sync, gc and rename all need a "not a worktree" branch |

**Recommended: A.** The screens below assume it.


### One state vocabulary: the same icon and colour for the same meaning, everywhere

The column header says *which* thing the icon is about: an agent's STATE, a machine's STATE, a worktree's STATE, or a review's CI. The icons themselves never change meaning. The TUI **requires a Nerd Font**; the README says so under Install. The Unicode glyphs below stand in for the Nerd Font icons.

| Icon | Colour | Meaning | Agent | Machine | Worktree (overall) | Review |
|---|---|---|---|---|---|---|
| `●` | cyan | running / working | working | running | WORKING | — |
| `◌` | cyan, dim | in progress | starting | starting, resuming | — | CI running |
| `◆` | yellow, bold | needs you: plan | plan to review | — | NEEDS YOU (plan) | an agent on it needs you |
| `?` | yellow, bold | needs you: question | asked you something | — | NEEDS YOU (question) | ″ |
| `!` | yellow, bold | needs you: permission | permission prompt | — | NEEDS YOU (permission) | ″ |
| `○` | grey | idle | turn done, waiting for you | — | IDLE | — |
| `⏸` | blue | paused | frozen (its machine is paused); the last-known state follows, dimmed: `⏸◆` | paused; `⏸ until 14:05` in a cooldown | its machine is paused (`⏸` + last known) | — |
| `■` | grey, dim | stopped / gone | no process | stopped | DEAD | closed |
| `✕` | red, bold | failed | crashed | error | FAILED | CI failed, conflicts |
| `✓` | green, bold | done / good | — | — | READY, MERGED | CI passed, approved, mergeable, merged |

The next table lists **type icons**. They say what a thing is, not its state, and they stay simple.

| Type | Icon (Nerd Font name) |
|---|---|
| local machine | laptop outline (`nf-md-laptop`) |
| InstaBox machine | cloud (`nf-md-cloud_outline`) |
| tmux session | terminal (`nf-cod-terminal`) |
| worktree / branch | branch (`nf-dev-git_branch`) |
| review | pull request (`nf-oct-git_pull_request`) |

### The hierarchy adapts to the scope, and only to the scope

The header has two scopes:
- **project:** cycle with `]p`/`[p`, as today.
- **machine:** cycle with `]m`/`[m` through all → local → instabox, one stop per configured provider. `filter.machine` pins the selected row's machine instead, which shows as `machine: instabox/ibx-feat-b`.

**The rule.** Each tab has a fixed list of levels. A level is hidden exactly when the scope pins it. The layout depends on the scope, never on the data, so it doesn't jump when a second project or machine appears.

| Tab | Levels (outer → inner) | Hidden when project is pinned | Hidden when machine scope is pinned |
|---|---|---|---|
| Agents | Project → Agent | Project | — (the machine is a column; rows are filtered) |
| Machines | Provider → Machine → Session (→ panes on `Enter`) | — (rows are filtered) | Provider (scope = a provider); Provider and Machine (scope = one machine) |
| Worktrees | Project → Worktree | Project | — (filtered) |
| Reviews | Project → Review → Agent | Project | — (agent rows are filtered) |

### The screens

The scenario:
- **laptop:** runs `fix-a` (working) and `review-512` (plan), plus your own tmux session `scratch`;
- **InstaBox:**
  - `ibx-feat-b` runs `feat-b` (working);
  - `ibx-fix-c` is paused with `fix-c` (plan);
  - `ibx-review-512` runs `review-512-2`, a second review of !512.

`WHERE` = type icon, machine name and session.

**Agents**, scope `project: all · machine: all`:

```
 AGENT          STATE    TITLE                         TASK    WHERE                                WORKTREE        REVIEW  AGE
 ▾ monid-services
   fix-a        ●        Fix rate limiter off-by-one   start   💻 laptop › fix-a                   fix-a           —       4m
   review-512   ◆        Review rate limiter MR        review  💻 laptop › review-512              review-512      !512    12m
   review-512-2 ●        Review rate limiter MR        review  ☁ ibx-review-512 › review-512-2     review-512-2    !512    3m
   feat-b       ●        Add serp provider             start   ☁ ibx-feat-b › feat-b               feat-b          !530    30m
   fix-c        ⏸◆       Fix webhook retry             start   ☁ ibx-fix-c › fix-c                 fix-c           —       2h
```

The same tab with `project: monid-services · machine: instabox`. The project level is gone, and the laptop rows are filtered out:

```
 AGENT          STATE    TITLE                         TASK    WHERE                                WORKTREE        REVIEW  AGE
 review-512-2   ●        Review rate limiter MR        review  ☁ ibx-review-512 › review-512-2     review-512-2    !512    3m
 feat-b         ●        Add serp provider             start   ☁ ibx-feat-b › feat-b               feat-b          !530    30m
 fix-c          ⏸◆       Fix webhook retry             start   ☁ ibx-fix-c › fix-c                 fix-c           —       2h
```

**Machines**, scope `machine: all`. The levels are Provider → Machine → Session:

```
 MACHINE / SESSION        STATE          SIZE  IDLE          COST   AGENTS
 ▾ 💻 local
   ▾ laptop              ●              —     —             —      ●1 ◆1
       fix-a              attached · 2 panes                         ● fix-a
       review-512         detached · 2 panes                         ◆ review-512
       scratch            detached · 1 pane (nvim)
 ▾ ☁ instabox
   ▾ ibx-feat-b          ●              4/8   pauses ~30m   $0.40  ●1
       feat-b             detached · 2 panes                         ● feat-b
   ▾ ibx-review-512      ●              4/8   pauses ~30m   $0.05  ●1
       review-512-2       detached · 2 panes                         ● review-512-2
   ▾ ibx-fix-c           ⏸ 2h           4/8   —             $0.12  ⏸◆1
       fix-c              as of 14:02 · 2 panes                      ⏸◆ fix-c
```

With `machine: instabox`, the `local` and `instabox` rows disappear and the machines become the top level.

**Worktrees.** Focus is on the worktree: name, branch, sync. The machine comes last.

```
 WORKTREE       BRANCH          SYNC    CHANGES  STATE        REVIEW  AGENTS  MACHINE
 ▾ monid-services
   (main)       main            synced  clean    —            —       —       💻 laptop
   fix-a        fix/a           local   dirty    ●            —       ●1      💻 laptop
   review-512   @a1b2 (!512)    —       clean    ◆            !512    ◆1      💻 laptop
   review-512-2 @a1b2 (!512)    —       clean    ●            !512    ●1      ☁ ibx-review-512
   feat-b       feat/b          ↑2      clean    ●            !530    ●1      ☁ ibx-feat-b
   fix-c        fix/c           synced  clean    ⏸◆           —       ⏸◆1    ☁ ibx-fix-c
```

**Reviews.** Project → Review → Agent. An agent row spells out where it runs and what it edits.

```
 REVIEW / AGENT     TITLE / STATE        CI   STATUS     WHERE                               WORKTREE
 ▾ monid-services
   ▾ !512           Add rate limiter     ✓    Open       ◆1 ●1
       review-512   ◆                                    💻 laptop › review-512             review-512
       review-512-2 ●                                    ☁ ibx-review-512 › review-512-2    review-512-2
   ▾ !530           Serp integration     ◌    Draft      ●1
       feat-b       ●                                    ☁ ibx-feat-b › feat-b              feat-b
     !541           Fix CDK roles        ✕    CI failed  —
```

**Actions** (`Space` on a row):
- **Agent row** (in any tab):
  - `a` attach (wakes a paused machine);
  - `p` plan;
  - `d` diff in nvim on that machine;
  - `m` message;
  - `n` rename;
  - `x` close.
- **Machine row:**
  - `S` shell;
  - `z` pause;
  - `r` resume;
  - `k` keep awake 2h;
  - `i` idle timer;
  - `s` stop/start;
  - `D` delete.
- **Session row:** `a` attach, `x` kill.

### tmux is required everywhere

Every agent Fleet starts runs in a tmux session on its machine.
- The InstaBox snapshot installs `tmux`; the default template doesn't list it.
- `fleet machine doctor <name>` checks for `tmux`, `git` and the harness.
- An agent started by hand outside tmux still shows up, under its machine with no session, as today.

## Q3. How does the laptop learn a cloud agent's state without keeping the machine awake?

**Finding [DOCS].** On InstaBox, an `exec`, a file read or an SSH connection counts as activity, and it wakes a paused machine. The TUI refreshes every 2 s today. If it ran `fleet status` inside each machine on every refresh, no machine would ever pause.

### Options compared

| | A. The machine pushes its state into its own InstaBox metadata | B. The laptop polls inside the machine (`exec`) |
|---|---|---|
| What you get | The in-machine Fleet hook (already fires on WaitPlan, WaitPermission, Idle, …) calls `PATCH /v1/machines/{self}` with `metadata = {fleet.fix-c.state = "wait_plan", …}`. The laptop reads `GET /v1/machines`, which never wakes a machine | Every N s, run `fleet status --json` in each running machine |
| Pros | Zero wake-ups; one HTTP call lists every machine and its agent states; works with the TUI closed; metadata can be set in any state [DOCS] | No new code inside the machine |
| Cons | The machine needs an InstaBox key to patch itself. Keys are per InstaBox workspace, so it could touch other machines too. Mitigation: a dedicated `fleet` workspace and key | Machines never idle-pause while the TUI is open; slow with 20 machines |

**Recommended: A.** B is still used for on-demand detail, when you open a row's detail panel.


## Q4. Machine adapters: `local` and `instabox`, the same interface

**Decided:** the laptop is an adapter like any other.

| Interface method | `local` (the laptop) | `instabox` (HTTP, no CLI) |
|---|---|---|
| `list` / `get` | one machine, `laptop`, always running | `GET /v1/machines` (never wakes a machine) |
| `create` / `destroy` | not supported (the laptop always exists) | `POST /v1/machines` + poll the operation / `DELETE` with `X-Confirm-Delete` |
| `exec` | `tokio::process::Command` | `POST /v1/machines/{id}/exec` (argv form, at most 10 min) |
| `tty_command` | the argv unchanged | key pair under `~/.local/share/fleet/instabox/` + a short-lived certificate from `POST …/connections` [LIVE: schema checked] → system `ssh` or `mosh` |
| `pause` / `resume` / `keepalive` / `set_idle` | not supported | `suspend` / `resume` / activity lease / `PATCH autoSuspendAfterSeconds` |
| `port_url` | `http://127.0.0.1:<port>` | `POST …/ports` (private, token-gated) |
| `publish` (agent state) | today's local state files | `PATCH` the machine's metadata |

**Why HTTP for InstaBox:**
- InstaBox ships TypeScript and Python SDKs but no Rust one.
- It publishes a full OpenAPI 3.1 document.
- Fleet already depends on `reqwest` (`Cargo.toml:21`).
- The CLI is not needed, even for SSH.

## Q5. Machine status and control; what pausing can and can't know

**Decided:** Fleet shows machine status and can control it, but has no automatic pause policy of its own yet.

- **Shows** (Machines tab, `fleet machine ls`):
  - state: running, paused, stopped;
  - `autoSuspendedAt`, `cooldownUntil`, `maxRunMinutes`, `oomKills`;
  - the idle setting and the cost so far this month.
- **Controls** (`fleet machine <name|id> …` and Machines-row actions):
  - `pause`, `resume`, `stop`, `start`, `rm`;
  - `keep 2h` (an activity lease) and `release`;
  - `idle 10m|off`, `max-run 4h|off`;
  - `shell`.
- **Who pauses: InstaBox.** A machine counts as idle only when no agent is working, there is no SSH or terminal traffic, there is no CPU load, and no exec, file read or lease is in flight. Unclear signals count as busy.

What no rule can see (documented in the doc):

| Case | Result | What to do |
|---|---|---|
| You're SSHed in and typing | traffic, so the machine stays awake | nothing |
| SSH open but untouched | it idles and pauses; the connection closes; memory is kept | reattach, which wakes it |
| A silent background job (`sleep`, waiting on CI, a quiet download) | may pause; the job resumes afterwards | `fleet machine keep 2h` |
| Outbound connections (DB, websocket) across a pause | the far end may drop them | clients reconnect |
| Free plan: run capped at 2–8 h | pauses even when busy, then a cooldown | Pro: `max-run off` |

The interface keeps `pause`, `keepalive` and `set_idle`, so a Fleet-driven policy can be added later without changing the interface.

## Q6. OAuth logins (opencode subscription, MCP) on cloud machines

**Why the channel is safe [DOCS]:**
- "direct" secrets are written through the API by an admin key and are never returned;
- they reach the machine as in-memory files (mode 0600), never on disk and never in snapshots;
- every use is audited by name;
- a rotation reaches running machines in about 10 s.

**The open risk:** two parties refreshing the same OAuth login.

### Options compared

| | A. The laptop is the only refresher; machines get the access token only | B. Machines get the full login file |
|---|---|---|
| What you get | `fleet creds sync` pushes the laptop's `auth.json`/`mcp-auth.json` with the refresh tokens removed, after every change and every N min | the same push, with the refresh tokens included |
| Pros | No revocation fights; one login serves every machine | Machines keep working while the laptop sleeps |
| Cons | If the laptop sleeps longer than the token lifetime, machines lose that login until it wakes (fallback: an API key) | A refresh on one machine can sign out the laptop and the other machines |

**Recommended: A,** verified in the spike.


---


## Appendix — vendor comparison (reference; goes into the doc)

| | InstaBox | Daytona | E2B | boat.dev | Agent 37 | Funky |
|---|---|---|---|---|---|---|
| What it is | Firecracker microVMs | container or Linux VM | Firecracker microVMs | full VMs | gVisor or VM | managed agent runtime (on E2B) |
| SSH + tmux | ✅ + mosh | ✅ | ⚠️ WebSocket proxy | ✅ | ✅ | ❌ |
| Pause keeps processes | ✅ | VM class only | ✅ | ❌ stop only | ✅ "usually" | n/a |
| Working ≠ idle | ✅ agent, CPU, SSH, lease | ❌ needs a keep-alive | ❌ timeout | ❌ deadline | ❌ URL traffic only | — |
| Secret proxy | ✅ headers + Basic + files | ✅ + response scrubbing | ✅ headers | AI + custom base-URL APIs; renews Claude/ChatGPT logins | ❌ | open-source only (via E2B) |
| Docker / max RAM | ✅ / 16 (Pro), 32 (Builder) GiB | 8 GiB default | 8 GiB | 32 GiB | 16 GiB | — |
| Region | API in us-west-2 [LIVE] | US, EU | US | EU only | ? | — |
| ~Month at our volume | ~$50 | ~$250 | ~$400+ | ~$35 | ~$10 | — |
| Verdict | **chosen** | best backup | mature, RAM-limited | no pause, far away | no proxy | different product (no opencode/tmux) |
