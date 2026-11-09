-- fleet's shipped configuration. Embedded in the binary; your ~/.config/fleet/init.lua is
-- deep-merged OVER this (tables merge key by key; scalars and lists replace). Every value
-- fleet uses lives here, so `fleet config --default` is the complete reference and the
-- template for your own file. Each section is a folder with a mod.lua; a prompt file lives
-- next to the Lua that reads it.
return {
  defaults = require("defaults"),
  agents   = require("agents"),
  tasks    = require("tasks"),
  machines = require("machines"),
  keys     = require("keys"),
  projects = {},                    -- your init.lua lists them (fleet project add appends)
}
