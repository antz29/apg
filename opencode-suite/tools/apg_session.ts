import { tool } from "@opencode-ai/plugin"
import { startDetachedSession } from "../lib/apg.ts"

export default tool({
  description:
    "Start a live apg session for the project's worktree. `apg session start` is a blocking foreground coordinator (it holds the worktree's apg/.trans/db.lbug and the extended specs.lock flock for its whole life and serves routed mutations/reads over a Unix socket), so this tool spawns it DETACHED, waits until its socket answers live, then returns while the session keeps running. Once live, durable `apg node`/`apg edge` mutations and `apg query` reads route through the session (seeing unsaved buffered changes); finish with `apg session save` (flush the buffer in one commit) and `apg session end` (or `apg session abort` to discard). Returns the session's root, socket, and pid, or an error string when no db is found, `start` exits early (e.g. a session already owns the worktree), or the socket never becomes live.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
  },
  async execute(args, context) {
    const session = await startDetachedSession(context, args.directory)
    if (typeof session === "string") {
      return session
    }
    return [
      `Session live for ${session.root}`,
      `  socket: ${session.socket}`,
      `  pid: ${session.pid}`,
      "Durable mutations and queries now route through the live session. Save with `apg session save`, then end with `apg session end` (or `apg session abort` to discard).",
    ].join("\n")
  },
})
