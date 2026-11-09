-- A task is a preset for starting an agent. `on = "project"` creates a worktree from the
-- project's base branch; `on = "review"` checks out the selected MR/PR head (detached).
-- Per task: key (the key AFTER the tasks prefix, `Space t` by default), on, agent, mode,
-- model, skills, instructions (text; put it in a .md next to this file and read() it),
-- prompt (preset user prompt, editable in the popup), label.
return {
  start   = { key = "s", on = "project" },
  review  = { key = "r", on = "review",  mode = "plan", instructions = read("review.md") },
  explain = { key = "e", on = "project", mode = "plan", instructions = read("explain.md") },
}
