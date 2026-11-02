#!/usr/bin/env bash
# End-to-end check of `fleet worktree close` (and gc on the same engine) against a scratch repo.
#
# Isolated from your setup: its own repo + bare origin under /tmp, its own fleet config and
# registry (FLEET_CONFIG_DIR / FLEET_DATA_DIR), an empty opencode data dir (XDG_DATA_HOME), and
# its own tmux server (TMUX_TMPDIR, TMUX unset). The "agent" is a stub binary named `opencode`
# that just sleeps, so no model is called. Everything is deleted on exit, pass or fail.
#
# Usage: scripts/e2e-close.sh [path/to/fleet]   (default: target/debug/fleet; run cargo build first)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
FLEET=${1:-$ROOT/target/debug/fleet}
[ -x "$FLEET" ] || { echo "no fleet binary at $FLEET (cargo build first)"; exit 2; }
for c in git tmux sqlite3 rustc; do command -v $c >/dev/null || { echo "needs $c"; exit 2; }; done

T=$(cd "$(mktemp -d /tmp/fleet-e2e.XXXX)" && pwd -P)
unset TMUX TMUX_PANE
export TMUX_TMPDIR=$T/tmux FLEET_CONFIG_DIR=$T/config FLEET_DATA_DIR=$T/data XDG_DATA_HOME=$T/xdg
mkdir -p "$TMUX_TMPDIR" "$FLEET_CONFIG_DIR" "$FLEET_DATA_DIR" "$XDG_DATA_HOME" "$T/bin"
cleanup() { tmux kill-server 2>/dev/null || true; rm -rf "$T"; }
trap cleanup EXIT

PASS=0
ok() { PASS=$((PASS + 1)); echo "  ok  $*"; }
die() { echo "  FAIL $*"; echo "--- last output:"; cat "$T/out" 2>/dev/null || true; exit 1; }
check() { local what=$1; shift; if "$@"; then ok "$what"; else die "$what"; fi; }
nocheck() { local what=$1; shift; if "$@"; then die "$what"; else ok "$what"; fi; }

# ── stub agent, repo, config, tmux server ──
printf 'fn main(){loop{std::thread::sleep(std::time::Duration::from_secs(60));}}' >"$T/stub.rs"
rustc -O -o "$T/bin/opencode" "$T/stub.rs" 2>/dev/null
export PATH=$T/bin:$PATH
REPO=$T/repo WT=$T/wt
git init -q --bare -b main "$T/origin.git"
git init -q -b main "$REPO"
git -C "$REPO" -c user.name=e2e -c user.email=e2e@x commit -q --allow-empty -m init
git -C "$REPO" remote add origin "$T/origin.git"
git -C "$REPO" push -q -u origin main
export GIT_AUTHOR_NAME=e2e GIT_AUTHOR_EMAIL=e2e@x GIT_COMMITTER_NAME=e2e GIT_COMMITTER_EMAIL=e2e@x
cat >"$FLEET_CONFIG_DIR/init.lua" <<EOF
return {
  defaults = { nvim_pane = false, notify = false },
  projects = { { name = "demo", repo = "$REPO", worktrees = "$WT" } },
}
EOF
printf 'set -g base-index 1\nset -g pane-base-index 0\nset -g default-shell /bin/sh\n' >"$T/tmux.conf"
tmux -f "$T/tmux.conf" new-session -d -s keep -c "$T"

# ── helpers ──
f() { "$FLEET" "$@" >"$T/out" 2>&1; }
has_wt() { [ -d "$WT/$1" ] && git -C "$REPO" worktree list --porcelain | grep -q "^worktree .*/$1\$"; }
has_branch() { git -C "$REPO" rev-parse -q --verify "refs/heads/$1" >/dev/null; }
has_session() { tmux has-session -t "=$1" 2>/dev/null; }
task_open() { [ "$(sqlite3 "$FLEET_DATA_DIR/fleet.db" "select count(*) from task where slug='$1' and closed_at is null")" = 1 ]; }
out_has() { grep -q -- "$1" "$T/out"; }
agent_pid() { pgrep -f "opencode --port .*$1" | head -1 || true; }
start() { f agent start "$1" -m "e2e $1" --repo demo --no-nvim --no-attach || die "start $1"; sleep 1; }
gone_all() { ! has_wt "$1" && ! has_branch "$2" && ! has_session "$1" && ! task_open "$1"; }

echo "1. a running agent blocks close; nothing changes"
start t1
nocheck "close t1 refused" f close t1 --yes
check "names the agent" out_has "agent running"
intact() { has_wt t1 && has_branch feat/t1 && has_session t1 && task_open t1 && [ -n "$(agent_pid t1)" ]; }
check "worktree, branch, session, task, agent intact" intact

echo "2. stop + rename (symlink), then close removes everything"
f agent stop t1 || die "stop"
sleep 1
f agent rename t1 t2 || die "rename"
check "rename left a symlink" test -L "$WT/t1"
check "close t2" f close t2 --yes
check "worktree, branch, session, task gone" gone_all t2 feat/t2
nocheck "symlink gone" test -L "$WT/t1"

echo "3. dirty + agent: refused listing them; --force closes"
start t3
pid=$(agent_pid t3)
echo hi >"$WT/t3/scratch.txt"
nocheck "close t3 refused" f close t3 --yes
check "lists the file" out_has "scratch.txt"
check "lists the agent" out_has "agent running"
check "still there" has_wt t3
check "close --force" f close t3 --force --yes
check "all gone" gone_all t3 feat/t3
nocheck "agent process gone" kill -0 "$pid" 2>/dev/null

echo "4. a commit only on this branch blocks; after push it closes (remote kept)"
start t4
f agent stop t4 || die "stop t4"
git -C "$WT/t4" commit -q --allow-empty -m "only-here-t4"
nocheck "close t4 refused" f close t4 --yes
check "lists the commit" out_has "only-here-t4"
git -C "$WT/t4" push -q -u origin feat/t4
check "close after push" f close t4 --yes
check "all gone" gone_all t4 feat/t4
remote_kept() { git -C "$REPO" rev-parse -q --verify refs/remotes/origin/feat/t4 >/dev/null; }
check "remote branch untouched" remote_kept

echo "5. a failing step rolls back what was done"
start t5
f agent stop t5 || die "stop t5"
f agent rename t5 t5b || die "rename t5"
tmux new-session -d -s t5b -c "$WT/t5b"
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
git -C "$REPO" worktree remove "$WT/t6"   # what a crash right after step 4 leaves behind
check "re-run by name" f close t6 --yes
check "  explains it" out_has "half-way"
check "branch, session, task gone" gone_all t6 feat/t6

echo "6. a plain git worktree (no registry row) closes by path"
git -C "$REPO" worktree add -q -b plain/x "$T/plain" main
check "close by path" f close "$T/plain" --yes
plain_gone() { ! test -d "$T/plain" && ! has_branch plain/x; }
check "folder and branch gone" plain_gone
nocheck "dry run without a tty and --yes changes nothing" bash -c "git -C '$REPO' worktree add -q -b plain/y '$T/plain2' main && '$FLEET' close '$T/plain2' </dev/null >'$T/out' 2>&1 && ! test -d '$T/plain2'"
check "  (it was a dry run)" out_has "dry run"
f close "$T/plain2" --yes || die "close plain2"

echo "7. close typed inside the task's own tmux session"
start t7
tmux new-window -t "=t7" -c "$WT/t7"
tmux send-keys -t "t7:2" "'$FLEET' close --force --yes t7 >'$T/t7.log' 2>&1" Enter
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

echo "all $PASS checks passed"
