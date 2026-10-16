import { Plugin } from "@opencode/plugin"
import { runCli, startDetachedSession, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_session",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_session",
        options: { codemode: false },
        description:
          "Drive the live apg session lifecycle for the project's worktree: `action` = start (default) | save | end | abort. `start`: `apg session start` is a blocking foreground coordinator (it holds the worktree's apg/.trans/db.lbug and the extended specs.lock flock for its whole life and serves routed mutations/reads over a Unix socket), so this tool spawns it DETACHED, waits until its socket answers live, then returns while the session keeps running; durable `apg node`/`apg edge` mutations and `apg query` reads then route through the session (seeing unsaved buffered changes). `save`: `apg session save` — flush the whole buffer in one atomic `apg/layers/**` write and exactly one commit (a noop over an empty buffer). `end`: `apg session end` — release the DB/lock/socket; refuses while the buffer is dirty (save or abort first). `abort`: `apg session abort` — discard the buffer and release the session. The caller owns the whole lifecycle: start → mutate → save → end. Note `apg_spec_lint` reads the durable files, so lint after `save`.",
        input: {
          type: "object",
          properties: {
            action: {
              type: "string",
              enum: ["start", "save", "end", "abort"],
              description: "Lifecycle step: start (default), save, end, or abort.",
            },
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
          },
          required: [],
          additionalProperties: false,
        },
        async execute(input, context) {
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const action = input.action ?? "start"
          if (action !== "start") {
            return { content: await runCli(merged, ["session", action], input.directory) }
          }
          const session = await startDetachedSession(merged, input.directory)
          if (typeof session === "string") {
            return { content: session }
          }
          return {
            content: [
              `Session live for ${session.root}`,
              `  socket: ${session.socket}`,
              `  pid: ${session.pid}`,
              `  log: ${session.logPath}`,
              "Durable mutations and queries now route through the live session. Save with apg_session action=save, then end with action=end (or action=abort to discard).",
            ].join("\n"),
          }
        },
      })
    })
  },
})
