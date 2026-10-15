import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_plan_verify",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_verify",
        description:
          "Run the pre-merge coherence gate: `apg plan verify <project>` (renamed from `apply` — the binary applies nothing). Verifies every planned Implementation node in the branch is realized (a scan found real code at its FQN), all Feedback is resolved, and derived solution coverage holds. On green it prints the merge handoff. Read-only; the merge itself is `apg project merge <project>` from the main checkout.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            project: { type: "string", description: "Plan project (required)." },
          },
          required: ["project"],
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
          const { project } = input
          if (!project) return { content: "Error: project is required" }
          return { content: await runCli(merged, ["plan", "verify", project], input.directory) }
        },
      })
    })
  },
})
