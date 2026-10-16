import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_project",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_project",
        options: { codemode: false },
        description:
          "Project lifecycle. `start <name>` — run from the MAIN checkout: creates the project worktree at <main>/apg/.worktrees/<name>, a branch off the repo's default branch, and seeds the branch DB by copying the main checkout's current scan (run `apg scan` on main first — start refuses a stale or missing scan); the binary prints the worktree path and sessions then operate with cwd INSIDE that worktree (walk-up discovery finds the worktree's own apg/). `verify <name>` — the pre-merge coherence gate (`apg plan verify`): every planned node realized, all feedback resolved, derived solution coverage holds; read-only. `merge <name>` — run from the MAIN checkout: verify gate → merge the project branch → rebuild main's graph unguarded. Main is never a mutation place.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            action: { type: "string", description: '"start", "verify", or "merge" (required).' },
            name: { type: "string", description: "Project name (== branch name == worktree dir) (required)." },
          },
          required: ["action", "name"],
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
          const { action, name } = input
          if (!action || !name) return { content: "Error: action and name are required" }
          if (action === "start" || action === "merge") {
            return { content: await runCli(merged, ["project", action, name], input.directory) }
          }
          if (action === "verify") {
            return { content: await runCli(merged, ["plan", "verify", name], input.directory) }
          }
          return { content: 'Error: action must be "start", "verify", or "merge"' }
        },
      })
    })
  },
})
