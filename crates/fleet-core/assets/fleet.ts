// fleet opencode plugin — installed by `fleet setup` into ~/.config/opencode/plugins/fleet.ts
//
// Writes one small JSON file per opencode process so that `fleet` can see what the
// database does not hold: pending permission prompts, exact session ids, and the
// process's own HTTP API url. Removed by `fleet setup --remove`.
import type { Plugin } from "@opencode-ai/plugin"

export const FleetPlugin: Plugin = async ({ serverUrl, directory }) => {
  const dataDir =
    process.env.FLEET_DATA_DIR ?? `${process.env.XDG_DATA_HOME ?? `${process.env.HOME}/.local/share`}/fleet`
  const dir = `${dataDir}/agents`
  const file = `${dir}/${process.pid}.json`
  const state = {
    version: 1,
    pid: process.pid,
    server_url: serverUrl.toString(),
    directory,
    sessions: {} as Record<string, { status: string; pending: { id: string; title: string; type: string }[] }>,
    updated_at: 0,
  }
  const session = (id: string) => (state.sessions[id] ??= { status: "idle", pending: [] })
  let timer: ReturnType<typeof setTimeout> | undefined
  const write = () => {
    // coalesce bursts of events into one write
    if (timer) return
    timer = setTimeout(async () => {
      timer = undefined
      state.updated_at = Date.now()
      try {
        await Bun.write(file, JSON.stringify(state))
      } catch {
        // the data dir may not exist yet; fleet creates it on first run
      }
    }, 50)
  }
  try {
    await Bun.$`mkdir -p ${dir}`.quiet()
  } catch {}
  write()
  const cleanup = () => {
    try {
      require("node:fs").unlinkSync(file)
    } catch {}
  }
  process.on("exit", cleanup)
  process.on("SIGINT", () => {
    cleanup()
    process.exit(130)
  })
  process.on("SIGTERM", () => {
    cleanup()
    process.exit(143)
  })

  return {
    event: async ({ event }) => {
      const p: any = (event as any).properties
      switch (event.type as string) {
        // opencode <= 1.18 emits permission.updated, newer builds permission.asked
        case "permission.asked":
        case "permission.updated": {
          const s = session(p.sessionID)
          if (!s.pending.some((x) => x.id === p.id)) {
            // field names differ between opencode builds: title | permission | patterns
            const patterns = Array.isArray(p.patterns) ? p.patterns.join(", ") : Array.isArray(p.pattern) ? p.pattern.join(", ") : (p.pattern ?? "")
            const kind = p.type ?? p.permission ?? ""
            const title = p.title ?? (kind && patterns ? `${kind}: ${patterns}` : kind || patterns)
            s.pending.push({ id: p.id, title: String(title ?? ""), type: String(kind ?? "") })
          }
          break
        }
        case "permission.replied": {
          const s = session(p.sessionID)
          s.pending = s.pending.filter((x) => x.id !== p.permissionID)
          break
        }
        case "session.status":
          session(p.sessionID).status = p.status?.type ?? "idle"
          break
        case "session.idle":
          session(p.sessionID).status = "idle"
          session(p.sessionID).pending = []
          break
        case "session.deleted":
          delete state.sessions[p.info?.id ?? p.sessionID]
          break
        default:
          return
      }
      write()
    },
  }
}
