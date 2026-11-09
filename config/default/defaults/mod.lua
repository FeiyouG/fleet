return {
  agent = nil,                      -- REQUIRED in your init.lua: the agent (agents.<name>) new tasks use
  mode = "plan",                    -- plan (read-only) | build (edits; the harness asks before risky steps) | yolo (no prompts)
  left_pane = nil,                  -- command typed into the LEFT shell pane of a new task; nil = bare shell
  notify = true,                    -- macOS banner when an agent starts waiting for you / fails / is ready
  refresh_secs = 2,                 -- TUI snapshot interval
  review_ttl_secs = 60,             -- how long the MR/PR list is cached before asking gh/glab again
  system = read("system.md"),       -- fleet's system text, sent first to every agent (placeholders: see README)
}
