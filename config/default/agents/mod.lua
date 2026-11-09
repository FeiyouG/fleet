-- An agent is YOUR named configuration of a harness (opencode | claude | codex). Add as many
-- as you like: `fast = { harness = "opencode", model = "anthropic/claude-sonnet-4-5" }`.
-- Per agent: harness (required), enabled, bin, args, model (in the harness's own naming),
-- skills = { "/abs/path/SKILL.md", ... } always loaded for this agent, system = text appended
-- to fleet's system text.
return {
  opencode = { harness = "opencode", enabled = true },
  claude   = { harness = "claude",   enabled = true },
  codex    = { harness = "codex",    enabled = true },
}
