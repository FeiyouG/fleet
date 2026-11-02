// fleet opencode plugin — installed by `fleet setup` into ~/.config/opencode/plugins/fleet.ts
//
// Writes one small JSON file per opencode process so that `fleet` can see what the
// database does not hold: pending permission prompts, exact session ids, and the
// process's own HTTP API url. Removed by `fleet setup --remove`.
//
// It also opens a tiny control endpoint on 127.0.0.1 (random port, random bearer token,
// both written to the state file, which is readable by the user only). `fleet agent send`
// posts `{ text, model?, agent? }` there: a plain message is typed into the TUI; a
// message with a model or agent goes through the session API so that turn uses it.
import type { Plugin } from "@opencode-ai/plugin"

export const FleetPlugin: Plugin = async ({ client, serverUrl, directory }) => {
  const dataDir =
    process.env.FLEET_DATA_DIR ?? `${process.env.XDG_DATA_HOME ?? `${process.env.HOME}/.local/share`}/fleet`
  const dir = `${dataDir}/agents`
  const file = `${dir}/${process.pid}.json`
  const token = crypto.randomUUID()
  const state = {
    version: 2,
    pid: process.pid,
    server_url: serverUrl.toString(),
    control_url: "",
    token,
    directory,
    // the root (non-subagent) session the user last worked in
    current_session: "",
    sessions: {} as Record<
      string,
      { status: string; parent?: string; agent?: string; pending: { id: string; title: string; type: string }[] }
    >,
    updated_at: 0,
  }
  const session = (id: string) => (state.sessions[id] ??= { status: "idle", pending: [] })
  const rootOf = (id: string) => {
    let cur = id
    for (let i = 0; i < 20 && state.sessions[cur]?.parent; i++) cur = state.sessions[cur].parent!
    return cur
  }
  const touch = (id: string) => {
    if (!id) return
    const root = rootOf(id)
    if (!state.sessions[root]?.parent) state.current_session = root
  }
  let timer: ReturnType<typeof setTimeout> | undefined
  const write = () => {
    // coalesce bursts of events into one write
    if (timer) return
    timer = setTimeout(() => {
      timer = undefined
      state.updated_at = Date.now()
      try {
        require("node:fs").writeFileSync(file, JSON.stringify(state), { mode: 0o600 })
      } catch {
        // the data dir may not exist yet; fleet creates it on first run
      }
    }, 50)
  }
  try {
    await Bun.$`mkdir -p ${dir}`.quiet()
  } catch {}

  try {
    const server = Bun.serve({
      hostname: "127.0.0.1",
      port: 0,
      async fetch(req) {
        if (req.headers.get("authorization") !== `Bearer ${token}`) return new Response("unauthorized", { status: 401 })
        if (req.method !== "POST" || new URL(req.url).pathname !== "/send") return new Response("not found", { status: 404 })
        try {
          const body = (await req.json()) as { text?: string; model?: string; agent?: string }
          const text = String(body.text ?? "")
          if (!text.trim()) return Response.json({ ok: false, error: "empty message" }, { status: 400 })
          if (!body.model && !body.agent) {
            // plain message: exactly as if typed into the TUI
            await client.tui.appendPrompt({ body: { text } })
            await client.tui.submitPrompt()
            return Response.json({ ok: true, via: "tui" })
          }
          const id = state.current_session
          if (!id) return Response.json({ ok: false, error: "no session yet in this agent" }, { status: 409 })
          let model: { providerID: string; modelID: string } | undefined
          if (body.model) {
            const i = body.model.indexOf("/")
            if (i <= 0) return Response.json({ ok: false, error: `model must be provider/model: ${body.model}` }, { status: 400 })
            model = { providerID: body.model.slice(0, i), modelID: body.model.slice(i + 1) }
          }
          // Without an agent opencode would fall back to its default agent, not the one in use.
          const agent = body.agent || state.sessions[id]?.agent || undefined
          const r: any = await client.session.promptAsync({
            path: { id },
            body: { agent, model, parts: [{ type: "text", text }] },
          })
          if (r?.error) return Response.json({ ok: false, error: JSON.stringify(r.error) }, { status: 502 })
          // The TUI's own agent/model selector does not follow an API message: say so.
          const what = [body.agent && `agent ${body.agent}`, body.model && `model ${body.model}`].filter(Boolean).join(", ")
          client.tui
            .showToast({ body: { title: "fleet", message: `This message uses ${what}; your own next message uses the TUI's selection.`, variant: "info" } })
            .catch(() => {})
          return Response.json({ ok: true, via: "session", session: id })
        } catch (e) {
          return Response.json({ ok: false, error: String(e) }, { status: 500 })
        }
      },
    })
    state.control_url = `http://127.0.0.1:${server.port}`
  } catch {
    // no control endpoint: fleet falls back to tmux keystrokes
  }
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
        case "session.created":
        case "session.updated": {
          const info = p.info ?? {}
          if (!info.id) return
          const s = session(info.id)
          if (info.parentID) s.parent = info.parentID
          if (event.type === "session.created" && !info.parentID) state.current_session = info.id
          break
        }
        case "message.updated": {
          const info = p.info ?? {}
          if (!info.sessionID) return
          touch(info.sessionID)
          if (info.role === "user" && info.agent) session(info.sessionID).agent = info.agent
          break
        }
        case "session.status":
          session(p.sessionID).status = p.status?.type ?? "idle"
          touch(p.sessionID)
          break
        case "session.idle":
          session(p.sessionID).status = "idle"
          session(p.sessionID).pending = []
          break
        case "session.deleted":
          delete state.sessions[p.info?.id ?? p.sessionID]
          if (state.current_session === (p.info?.id ?? p.sessionID)) state.current_session = ""
          break
        default:
          return
      }
      write()
    },
  }
}
