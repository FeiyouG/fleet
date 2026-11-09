## 0. Setup

- [x] 0.1 Worktree + branch `feat/remote-machines`, `openspec init`
- [x] 0.2 Change artifacts (proposal, design, specs, tasks); `openspec validate --all`
- [x] 0.3 AGENTS.md rule (behaviour → `openspec/specs/**`), DESIGN.md pointer; draft PR

## 1. Machine interface + `local` adapter

- [x] 1.1 `machines/interfaces`: `MachineAdapter`, `MachineSpec`, `MachineRow`, `MachineState`, `ExecOut`, `Unsupported`
- [x] 1.2 `machines/adapters/local`: exec = local process, tty = argv unchanged, list = `laptop`
- [x] 1.3 `adapter_for(provider, cfg)`; tests

## 2. Session + Worktree through a machine

- [~] 2.1 tmux calls take a machine: start/review run tmux through the machine (`machines::remote::On`); other callers (send, close, rename, TUI) still local-only
- [~] 2.2 git worktree calls take a machine: start/review on a machine; rename/close of remote tasks not yet
- [~] 2.3 Existing tests pass on `local`; gc/close/discovery skip remote tasks (`Registry::local_tasks`); `RowKey` machine id pending (TUI)

## 3. Config + registry

- [x] 3.1 `machines.<name>` profiles; shipped `laptop`; `defaults.machine`; `tasks.*.machine`
- [x] 3.2 Resolution order like agents; `--machine` flag; validate unknown providers/profiles
- [x] 3.3 Registry `Task.machine`, `Task.machine_id` (serde default for old rows)
- [x] 3.4 Unique agent names across the registry

## 4. InstaBox adapter (HTTP)

- [x] 4.1 Client: auth, idempotency keys, error codes, retry on `retryable`, operation polling
- [x] 4.2 create/list/get/destroy, exec, pause/resume, keepalive (lease), set_idle, publish (metadata), port_url
- [x] 4.3 tty_command via SSH certificate from `/connections` (fleet key pair, cached cert)
- [x] 4.4 Mocked-HTTP tests

## 5. Snapshot builder

- [ ] 5.1 `fleet machine snapshot <profile>`: tmux, fleet, repo clone, connectors, docker images, opencode config, `fleet setup`

## 6. Remote state

- [ ] 6.1 In-machine `agent-hook` publishes state changes to metadata
- [ ] 6.2 Laptop snapshot merges remote rows from `list()`; paused machines never woken

## 7. TUI + CLI

- [ ] 7.1 Tabs Agents · Machines · Worktrees · Reviews; columns per spec
- [ ] 7.2 State vocabulary (one icon/colour per meaning); Nerd Font in README
- [ ] 7.3 Machine scope `]m`/`[m`, `filter.machine`; adaptive levels by scope only
- [ ] 7.4 Leader menus (attach/plan/diff on agents; machine controls)
- [x] 7.5 `fleet machine ls|pause|resume|stop|start|rm|keep|release|idle|max-run|shell|snapshot|doctor`

## 8. Secrets

- [x] 8.1 Profile `secrets`/`files` passed at create
- [ ] 8.2 `fleet creds sync` (refresh tokens stripped)

## 9. Live spike + docs

- [~] 9.1 Spike on InstaBox (one machine, `fleet-spike-*`, cleaned up); results into design.md — machine lifecycle, start, ssh, pause/resume done; agent/plan/idle/docker pending
- [ ] 9.2 `docs/machines.md`; archive the change
