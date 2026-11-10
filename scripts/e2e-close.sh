#!/usr/bin/env bash
# End-to-end check of `fleet worktree close` (and gc on the same engine) against a scratch repo.
#
# Isolated from your setup: its own repo + bare origin under /tmp, its own fleet config and
# registry (FLEET_CONFIG_DIR / FLEET_DATA_DIR), an empty agent data dir (XDG_DATA_HOME), a stub
# `opencode` (sleeps; no model is called; when "armed" for its folder it runs one command the
# way an agent's bash tool does) and a stub `gh` (answers "which PR of this branch was merged"
# from files the script writes, and logs every call).
#
# tmux SAFETY: this script is usually run from inside your own tmux, where $TMUX points at
# your server and tmux ignores TMUX_TMPDIR. So the script never calls bare `tmux`: every call
# goes through `t`, which passes an explicit socket inside the scratch dir, and it never runs
# `tmux kill-server` on anything but that socket. fleet itself (which calls plain `tmux`) runs
# with TMUX unset and TMUX_TMPDIR=$T/tmux, so it lands on the same scratch socket.
#
# Usage: scripts/e2e-close.sh [path/to/fleet]   (default: target/debug/fleet; cargo build first)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FLEET=${1:-$ROOT/target/debug/fleet}
[ -x "$FLEET" ] || { echo "no fleet binary at $FLEET (cargo build first)"; exit 2; }
for c in git tmux sqlite3 rustc; do command -v $c >/dev/null || { echo "needs $c"; exit 2; }; done

T=$(cd "$(mktemp -d /tmp/fleet-e2e.XXXX)" && pwd -P)
# fleet's own tmux calls: no $TMUX, so tmux uses $TMUX_TMPDIR/tmux-<uid>/default.
unset TMUX TMUX_PANE
export TMUX_TMPDIR=$T/tmux
SOCK=$TMUX_TMPDIR/tmux-$(id -u)/default
t() { tmux -S "$SOCK" "$@"; }
case "$SOCK" in "$T"/*) ;; *) echo "refusing: socket $SOCK is outside $T"; exit 2 ;; esac
export FLEET_CONFIG_DIR=$T/config FLEET_DATA_DIR=$T/data XDG_DATA_HOME=$T/xdg
mkdir -p "$TMUX_TMPDIR" "$FLEET_CONFIG_DIR" "$FLEET_DATA_DIR" "$XDG_DATA_HOME" "$T/bin" "$T/merged"
mkdir -p "$(dirname "$SOCK")"
chmod 700 "$TMUX_TMPDIR" "$(dirname "$SOCK")"
cleanup() {
  # Only the scratch server, by explicit socket; then the scratch dir.
  [ -S "$SOCK" ] && t kill-server 2>/dev/null || true
  rm -rf "$T"
}
trap cleanup EXIT

PASS=0
ok() { PASS=$((PASS + 1)); echo "  ok  $*"; }
die() { echo "  FAIL $*"; echo "--- last output:"; cat "$T/out" 2>/dev/null || true; exit 1; }
check() { local what=$1; shift; if "$@"; then ok "$what"; else die "$what"; fi; }
nocheck() { local what=$1; shift; if "$@"; then die "$what"; else ok "$what"; fi; }

# ── stubs ──
cat >"$T/stub.rs" <<'RS'
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
// Sleeps forever. When $E2E_ARM/arm-<cwd folder name> exists it first runs $E2E_CMD like an
// agent's bash tool: own process group, output piped back to the agent.
fn main() {
    let cwd = std::env::current_dir().unwrap();
    let leaf = cwd.file_name().unwrap().to_string_lossy().to_string();
    let arm = std::env::var("E2E_ARM").unwrap_or_default();
    let mut _child = None;
    if !arm.is_empty() && std::path::Path::new(&format!("{arm}/arm-{leaf}")).exists() {
        std::thread::sleep(std::time::Duration::from_secs(5));
        _child = Command::new("sh")
            .arg("-c")
            .arg(std::env::var("E2E_CMD").unwrap_or_default())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .ok();
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
RS
rustc -O -o "$T/bin/opencode" "$T/stub.rs" 2>/dev/null
cat >"$T/bin/gh" <<EOF
#!/bin/sh
# Stub gh: only \`pr list\` is answered. A merged PR of branch B exists when $T/merged/<B with / as %>
# holds "<number> <head sha>".
echo "\$*" >>"$T/gh.log"
head=""; state="open"
while [ \$# -gt 0 ]; do
  case "\$1" in --head) head=\$2; shift ;; --state) state=\$2; shift ;; esac; shift
done
f="$T/merged/\$(printf %s "\$head" | tr / %)"
if [ "\$state" = merged ] && [ -f "\$f" ]; then
  read -r n sha <"\$f"
  printf '[{"number":%s,"title":"t","author":{"login":"me"},"headRefName":"%s","baseRefName":"main","headRefOid":"%s","url":"u","updatedAt":"2026-11-01T00:00:00Z","state":"MERGED"}]\n' "\$n" "\$head" "\$sha"
else
  echo '[]'
fi
EOF
chmod +x "$T/bin/gh"
export PATH=$T/bin:$PATH
merged_pr() { printf '%s %s\n' "$2" "$3" >"$T/merged/$(printf %s "$1" | tr / %)"; }

# ── repo, origin, config, tmux server ──
REPO=$T/repo WT=$T/wt ORIGIN=$T/origin.git
export GIT_AUTHOR_NAME=e2e GIT_AUTHOR_EMAIL=e2e@x GIT_COMMITTER_NAME=e2e GIT_COMMITTER_EMAIL=e2e@x
git init -q --bare -b main "$ORIGIN"
git init -q -b main "$REPO"
git -C "$REPO" commit -q --allow-empty -m init
git -C "$REPO" remote add origin "$ORIGIN"
git -C "$REPO" push -q -u origin main
cat >"$FLEET_CONFIG_DIR/init.lua" <<EOF
return {
  defaults = { agent = "opencode", nvim_pane = false, notify = false },
  agents = { opencode = { enabled = true }, claude = { enabled = false }, codex = { enabled = false } },
  projects = { { name = "demo", repo = "$REPO", worktrees = "$WT", reviews = "github:e2e/demo" } },
}
EOF
printf 'set -g base-index 1\nset -g pane-base-index 0\nset -g default-shell /bin/sh\nset -g default-command /bin/sh\n' >"$T/tmux.conf"
# Inherited by every pane of the scratch server, so the armed stub agent can run fleet.
export E2E_ARM=$T E2E_FLEET=$FLEET
export E2E_CMD='exec "$E2E_FLEET" close --yes "$(basename "$PWD")" >"$E2E_ARM/self-close.log" 2>&1'
t -f "$T/tmux.conf" new-session -d -s keep -c "$T"
[ "$(t display -p '#{socket_path}')" = "$SOCK" ] || die "scratch tmux socket mismatch"

# ── helpers ──
f() { "$FLEET" "$@" >"$T/out" 2>&1; }
has_wt() { [ -d "$WT/$1" ] && git -C "$REPO" worktree list --porcelain | grep -q "^worktree .*/$1\$"; }
has_branch() { git -C "$REPO" rev-parse -q --verify "refs/heads/$1" >/dev/null; }
has_session() { t has-session -t "=$1" 2>/dev/null; }
task_open() { [ "$(sqlite3 "$FLEET_DATA_DIR/fleet.db" "select count(*) from task where slug='$1' and closed_at is null")" = 1 ]; }
out_has() { grep -q -- "$1" "$T/out"; }
agent_pid() { pgrep -f "opencode --port .*$1" | head -1 || true; }
start() {
  f agent start "$1" -m "e2e $1" --repo demo --no-left-pane --no-attach || die "start $1"
  sleep 1
  # The agent must be the stub, never a real agent from your PATH.
  local pid; pid=$(agent_pid "$1")
  [ -n "$pid" ] || die "no agent for $1"
  local exe; exe=$(lsof -a -p "$pid" -d txt -Fn 2>/dev/null | sed -n 's/^n//p' | head -1)
  [ "$exe" = "$T/bin/opencode" ] || die "agent of $1 runs $exe, not the stub"
}
gone_all() { ! has_wt "$1" && ! has_branch "$2" && ! has_session "$1" && ! task_open "$1"; }
# A pending permission prompt in the agent's state file: activity WAIT:perm (busy).
busy() { mkdir -p "$FLEET_DATA_DIR/agents"; printf '{"pid":%s,"agent":"opencode","sessions":{"s":{"pending":[{"id":"p","title":"bash","type":"bash"}]}}}' "$1" >"$FLEET_DATA_DIR/agents/$1.json"; }

echo "1. a busy agent blocks close; nothing changes; once idle, close stops it"
start t1
pid=$(agent_pid t1)
busy "$pid"
nocheck "close t1 refused" f close t1 --yes
check "names the busy agent" out_has "agent busy: opencode"
intact() { has_wt t1 && has_branch feat/t1 && has_session t1 && task_open t1 && [ -n "$(agent_pid t1)" ]; }
check "worktree, branch, session, task, agent intact" intact
rm "$FLEET_DATA_DIR/agents/$pid.json"
check "idle agent: close t1" f close t1 --yes
check "  all gone" gone_all t1 feat/t1
nocheck "  agent process gone" kill -0 "$pid" 2>/dev/null

echo "2. stop + rename (symlink), then close removes everything"
start r1
f agent stop r1 || die "stop"
f agent rename r1 t2 || die "rename"
check "rename left a symlink" test -L "$WT/r1"
check "close t2" f close t2 --yes
check "worktree, branch, session, task gone" gone_all t2 feat/t2
nocheck "symlink gone" test -L "$WT/r1"

echo "3. dirty: refused listing the file (an idle agent is not a blocker); --force closes"
start t3
pid=$(agent_pid t3)
echo hi >"$WT/t3/scratch.txt"
nocheck "close t3 refused" f close t3 --yes
check "lists the file" out_has "scratch.txt"
nocheck "does not list the idle agent" out_has "agent busy"
check "still there" has_wt t3
check "close --force" f close t3 --force --yes
check "all gone" gone_all t3 feat/t3
nocheck "agent process gone" kill -0 "$pid" 2>/dev/null

echo "4. a commit not merged into main blocks, pushed or not; merged into main it closes"
start t4
f agent stop t4 || die "stop t4"
git -C "$WT/t4" commit -q --allow-empty -m "only-here-t4"
nocheck "close t4 refused" f close t4 --yes
check "lists the commit" out_has "only-here-t4"
git -C "$WT/t4" push -q -u origin feat/t4
nocheck "pushed but not merged: still refused" f close t4 --yes
check "  says not merged" out_has "not merged"
git -C "$REPO" merge -q --no-ff -m "merge t4" feat/t4
git -C "$REPO" push -q origin main
check "merged into main: closes" f close t4 --yes
check "all gone" gone_all t4 feat/t4
remote_kept() { git -C "$ORIGIN" rev-parse -q --verify refs/heads/feat/t4 >/dev/null; }
check "remote branch untouched" remote_kept

echo "5. a failing step rolls back what was done"
start t5
f agent stop t5 || die "stop t5"
f agent rename t5 t5b || die "rename t5"
t new-session -d -s t5b -c "$WT/t5b"
# A git hook that refuses exactly one thing: deleting refs/heads/feat/t5b.
mkdir -p "$REPO/.git/hooks"
cat >"$REPO/.git/hooks/reference-transaction" <<'HOOK'
#!/bin/sh
[ "$1" = prepared ] || exit 0
while read -r old new ref; do
  case "$ref" in refs/heads/feat/t5b)
    case "$new" in *[!0]*) ;; *) echo "e2e hook: refusing to delete $ref" >&2; exit 1 ;; esac ;;
  esac
done
exit 0
HOOK
chmod +x "$REPO/.git/hooks/reference-transaction"
nocheck "close fails at delete branch" f close t5b --yes
check "names the failed step" out_has "FAILED: delete branch feat/t5b"
check "reports the rollback" out_has "rolled back"
check "worktree re-created" has_wt t5b
check "branch kept" has_branch feat/t5b
check "task reopened" task_open t5b
check "symlink restored" test -L "$WT/t5"
check "session untouched" has_session t5b
check "  and says nothing was lost" out_has "nothing lost"
rm "$REPO/.git/hooks/reference-transaction"
check "close again" f close t5b --yes
check "all gone" gone_all t5b feat/t5b
nocheck "symlink gone" test -L "$WT/t5"

echo "5b. a close interrupted after the worktree went is finished by a re-run"
start t6
f agent stop t6 || die "stop t6"
git -C "$REPO" worktree remove "$WT/t6"   # what a crash right after the worktree step leaves
check "re-run by name" f close t6 --yes
check "  explains it" out_has "half-way"
check "branch, session, task gone" gone_all t6 feat/t6

echo "6. a plain git worktree (no registry row) closes by path"
git -C "$REPO" worktree add -q -b plain/x "$T/plain" main
check "close by path" f close "$T/plain" --yes
plain_gone() { ! test -d "$T/plain" && ! has_branch plain/x; }
check "folder and branch gone" plain_gone
git -C "$REPO" worktree add -q -b plain/y "$T/plain2" main
"$FLEET" close "$T/plain2" </dev/null >"$T/out" 2>&1 || die "dry run failed"
check "no tty and no --yes is a dry run" out_has "dry run"
check "  that changed nothing" test -d "$T/plain2"
f close "$T/plain2" --yes || die "close plain2"

echo "7. close typed inside the task's own tmux session"
start t7
t new-window -t "=t7" -c "$WT/t7"
t send-keys -t "t7:2" "'$FLEET' close --force --yes t7 >'$T/t7.log' 2>&1" Enter
for _ in $(seq 1 40); do has_session t7 || break; sleep 0.25; done
cp "$T/t7.log" "$T/out" 2>/dev/null || true
check "all gone, own session last" gone_all t7 feat/t7

echo "8. gc uses the same planner: stale detached tmp worktrees"
git -C "$REPO" worktree add -q --detach "$T/gc-clean" main
git -C "$REPO" worktree add -q --detach "$T/gc-local" main
git -C "$T/gc-local" commit -q --allow-empty -m "only-here-gc"
f worktree gc --yes || die "gc"
check "clean one removed" bash -c "! test -d '$T/gc-clean'"
check "one with a local-only commit kept" test -d "$T/gc-local"
check "  and reported blocked" out_has "blocked: gc-local"

echo "9. squash-merged PR whose branch the forge deleted (stale, then pruned, remote ref)"
start t9
f agent stop t9 || die "stop t9"
git -C "$WT/t9" commit -q --allow-empty -m "only-here-t9"
git -C "$WT/t9" push -q -u origin feat/t9
git -C "$ORIGIN" branch -q -D feat/t9            # the forge deletes the branch after merging
stale_ref() { git -C "$REPO" rev-parse -q --verify refs/remotes/origin/feat/t9 >/dev/null; }
check "stale origin/feat/t9 still exists locally" stale_ref
nocheck "not merged anywhere: the stale ref does not make it safe" f close t9 --yes
check "  lists the commit" out_has "only-here-t9"
merged_pr feat/t9 9 "$(git -C "$WT/t9" rev-parse HEAD)"
git -C "$REPO" fetch -q --prune origin
nocheck "remote ref pruned" stale_ref
check "merged on the forge: closes" f close t9 --yes
check "  with a note naming the PR and commit" out_has "#9 is merged and contains"
check "  all gone" gone_all t9 feat/t9

echo "9b. a commit added after the merge still blocks"
start t10
f agent stop t10 || die "stop t10"
git -C "$WT/t10" commit -q --allow-empty -m "in-pr-t10"
merged_pr feat/t10 10 "$(git -C "$WT/t10" rev-parse HEAD)"
git -C "$WT/t10" commit -q --allow-empty -m "after-merge-t10"
nocheck "close t10 refused" f close t10 --yes
check "  lists only the commit after the merge" out_has "after-merge-t10"
nocheck "  not the merged one" out_has "    [0-9a-f]* in-pr-t10"
f close t10 --force --yes || die "force t10"

echo "9c. gc sees the merged PR (merged lookups feed gc_verdict)"
git -C "$REPO" worktree add -q -b feat/gcm "$WT/gcm" main
git -C "$WT/gcm" commit -q --allow-empty -m "only-here-gcm"
merged_pr feat/gcm 11 "$(git -C "$WT/gcm" rev-parse HEAD)"
f worktree list || die "list"
check "worktree list says gc yes: merged" out_has "yes: merged"
f worktree gc --yes || die "gc gcm"
check "gc removed it" bash -c "! test -d '$WT/gcm'"
nocheck "  and deleted the squash-merged branch" has_branch feat/gcm

echo "9d. an old merged PR of a reused branch name does not count"
git -C "$REPO" worktree add -q -b feat/reuse "$WT/reuse" main
git -C "$WT/reuse" commit -q --allow-empty -m "new-work-reuse"
git -C "$WT/reuse" push -q -u origin feat/reuse
t new-session -d -s reuse-nvim -c "$WT/reuse"
merged_pr feat/reuse 3 0123456789abcdef0123456789abcdef01234567
f worktree list || die "list"
nocheck "worktree list does not call it merged" grep -q "^reuse .*merged" "$T/out"
f worktree gc || die "gc dry run"
nocheck "gc does not offer it" out_has "close reuse"
nocheck "close refuses it (not merged)" f close reuse --yes
check "  lists its commit" out_has "new-work-reuse"
check "  session untouched" has_session reuse-nvim
f close reuse --force --yes || die "force reuse"

echo "9e. a one-shot snapshot does not ask the forge once per worktree"
for i in 1 2 3; do git -C "$REPO" worktree add -q -b feat/idle$i "$WT/idle$i" main; done
: >"$T/gh.log"
"$FLEET" status --short >/dev/null 2>&1
check "fleet status: one gh call" test "$(wc -l <"$T/gh.log" | tr -d ' ')" -le 1
for i in 1 2 3; do f close "idle$i" --yes || die "close idle$i"; done

echo "9f. gc applies only what is still true: an agent started after the dry run is not hit"
git -C "$REPO" worktree add -q -b feat/late "$WT/late" main
# An interactive gc in a scratch pane (a terminal, so it asks [y/N]); the agent starts while
# the dry run is on screen, then the answer is yes.
t new-session -d -s gcrun -c "$T"
t send-keys -t "gcrun:1" "'$FLEET' worktree gc >'$T/gc.log' 2>&1" Enter
for _ in $(seq 1 80); do grep -q "apply" "$T/gc.log" 2>/dev/null && break; sleep 0.25; done
cp "$T/gc.log" "$T/out"
check "dry run offers the merged-into-main worktree" out_has "close late (in base)"
t new-session -d -s late -c "$WT/late"
t send-keys -t "late:1" "opencode --port 4999 late" Enter
sleep 1
t send-keys -t "gcrun:1" "y" Enter
for _ in $(seq 1 80); do grep -q "skipped\|ok$" "$T/gc.log" 2>/dev/null && break; sleep 0.25; done
sleep 0.5
cp "$T/gc.log" "$T/out"
check "  the item is skipped" out_has "close late (in base): skipped"
check "  worktree kept" has_wt late
check "  agent alive" test -n "$(agent_pid late)"
t kill-session -t "=gcrun"
t kill-session -t "=late"
sleep 0.5
f close late --yes || die "close late"

echo "10. leftovers: worktree deleted by hand, branch + tmux session remain (no registry row)"
git -C "$REPO" worktree add -q -b refactor/leftover "$WT/leftover" main
t new-session -d -s demo-leftover-update -c "$WT/leftover"
t split-window -t "demo-leftover-update:1" -c "$WT/leftover"
rm -rf "$WT/leftover"
git -C "$REPO" worktree prune
check "close by folder name" f close leftover --yes
check "  notes the branch found by the folder name" out_has "found by the folder name"
nocheck "  branch gone" has_branch refactor/leftover
nocheck "  session gone" has_session demo-leftover-update

git -C "$REPO" worktree add -q -b refactor/left2 "$WT/left2" main
t new-session -d -s demo-left2-update -c "$WT/left2"
rm -rf "$WT/left2"
git -C "$REPO" worktree prune
check "close by session name" f close demo-left2-update --yes
nocheck "  branch gone" has_branch refactor/left2
nocheck "  session gone" has_session demo-left2-update

git -C "$REPO" branch -q refactor/bare main
check "close a bare leftover branch by name" f close refactor/bare --yes
nocheck "  branch gone" has_branch refactor/bare

echo "11. an agent closes its own task (fleet runs as the agent's child, output piped to it)"
touch "$T/arm-t11"
start t11
t new-window -t "=t11" -c "$WT/t11"     # a shell window in the task's session
for _ in $(seq 1 60); do has_session t11 || break; sleep 0.25; done
cp "$T/self-close.log" "$T/out" 2>/dev/null || true
nocheck "session gone (agent stopped last)" has_session t11
check "  worktree, branch, task gone" gone_all t11 feat/t11

check "the scratch tmux server is the only one touched (keep session alive)" has_session keep
echo "all $PASS checks passed"
