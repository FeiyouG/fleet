-- Every key fleet reacts to. Vim motions keep their Vim meaning. `Space` is the leader: it
-- opens a menu for the selected row (an agent, a worktree, a review, a session, a machine) showing
-- only what applies. Values under "leader menu" are the key AFTER Space; tasks are under
-- Space + tasks (default `Space t`) and each task's own `key` (see tasks/). An agent row is
-- also a worktree row: when both define the same key, the agent action wins there.
return {
  -- global
  quit = "q", help = "?", refresh = "r", ["refresh.reviews"] = "R", command = ":",
  filter = "/", ["find.next"] = "f", ["find.prev"] = "F", ["filter.project"] = "*",
  inspect = "i", open = "o", yank = "y", primary = "Enter", sort = "S",
  -- motion
  up = "k", down = "j", top = "g g", bottom = "G", ["page.up"] = "C-u", ["page.down"] = "C-d",
  collapse = "h", expand = "l", ["fold.all"] = "z c", ["unfold.all"] = "z o",
  -- tabs and go-to
  ["tab.agents"] = "1", ["tab.machines"] = "2", ["tab.worktrees"] = "3", ["tab.reviews"] = "4",
  ["tab.next"] = "Tab", ["tab.prev"] = "S-Tab",
  ["goto.agents"] = "g a", ["goto.machines"] = "g m", ["goto.worktree"] = "g w", ["goto.review"] = "g v",
  ["project.prev"] = "[ p", ["project.next"] = "] p",
  ["machine.prev"] = "[ m", ["machine.next"] = "] m", ["filter.machine"] = "M",
  -- leader
  leader = "Space", tasks = "t",
  -- leader menu (key after Space), by row type
  ["agent.attach"] = "a", ["agent.send"] = "m", ["agent.stop"] = "s", ["agent.rename"] = "n",
  ["agent.open_plan"] = "p", ["agent.copy_plan"] = "y",
  ["worktree.shell"] = "S", ["worktree.close"] = "x", ["worktree.push"] = "u", ["worktree.pull"] = "U",
  ["worktree.open_review"] = "o", ["worktree.gc"] = "g", ["worktree.columns"] = "c",
  ["review.start"] = "r", ["review.open"] = "o", ["review.copy"] = "y", ["review.worktrees"] = "w",
  ["review.agents"] = "a", ["review.filter"] = "f",
  ["agent.diff"] = "d",
  ["tmux.kill"] = "x", ["tmux.rename"] = "n",
  ["machine.shell"] = "S", ["machine.pause"] = "z", ["machine.resume"] = "r", ["machine.keep"] = "k",
  ["machine.idle"] = "i", ["machine.stop"] = "s", ["machine.delete"] = "D",
}
