import { tool } from "@opencode-ai/plugin"
import { runCli, startDetachedSession } from "../lib/apg.ts"

export default tool({
  description:
    "Drive the live apg session lifecycle for the project's worktree: `action` = start (default) | save | end | abort. `start`: `apg session start` is a blocking foreground coordinator (it holds the worktree's apg/.trans/db.lbug and the extended specs.lock flock for its whole life and serves routed mutations/reads over a Unix socket), so this tool spawns it DETACHED, waits until its socket answers live, then returns while the session keeps running; durable `apg node`/`apg edge` mutations and `apg query` reads then route through the session (seeing unsaved buffered changes). `save`: `apg session save` — flush the whole buffer in one atomic `apg/layers/**` write and exactly one commit (a noop over an empty buffer). `end`: `apg session end` — release the DB/lock/socket; refuses while the buffer is dirty (save or abort first). `abort`: `apg session abort` — discard the buffer and release the session. The caller owns the whole lifecycle: start → mutate → save → end. Note `apg_spec_lint` reads the durable files, so lint after `save`.",
  args: {
    action: tool.schema
      .enum(["start", "save", "end", "abort"])
      .optional()
      .describe("Lifecycle step: start (default), save, end, or abort."),
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
  },
  async execute(args, context) {
    const action = args.action ?? "start"
    if (action !== "start") {
      return runCli(context, ["session", action], args.directory)
    }
    const session = await startDetachedSession(context, args.directory)
    if (typeof session === "string") {
      return session
    }
    return [
      `Session live for ${session.root}`,
      `  socket: ${session.socket}`,
      `  pid: ${session.pid}`,
      "Durable mutations and queries now route through the live session. Save with apg_session action=save, then end with action=end (or action=abort to discard).",
    ].join("\n")
  },
})
