-- Machines: the computers agents run on. A machine profile is YOUR named recipe for one,
-- built on a provider (an adapter built into fleet): `local` (this computer) or `instabox`
-- (cloud machines, https://instabox.sh). `defaults.machine` and `tasks.<name>.machine` pick a
-- profile; `fleet task <name> --machine <profile>` overrides it.
--
--   machines = {
--     ibx = {
--       provider = "instabox",
--       api_key_env = "INSTABOX_API_KEY",          -- env var holding the API key
--       snapshot = "fleet-monid-services",         -- or template = "agent-browser"
--       size = { vcpus = 4, mem_mib = 8192 },
--       per_task = true,                           -- one machine per agent (false: shared)
--       idle_min = 30, max_run_min = -1,           -- pause after idle; -1 = no run cap
--       secrets = { "GITLAB_TOKEN", "ANTHROPIC_API_KEY" },   -- InstaBox proxy secrets (names)
--       files = { [".local/share/opencode/auth.json"] = "OPENCODE_AUTH" },  -- file secrets
--       workdir = "/data/home/src",                -- where repos and worktrees live
--     },
--   },
return {
  laptop = { provider = "local" },
}
