import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgPlanVerifyInput = { project: string }

export default Plugin.define({
  id: "apg_plan_verify",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_verify",
        options: { codemode: false },
        description:
          "Run the pre-merge coherence gate: `apg plan verify <project>` (renamed from `apply` — the binary applies nothing). Verifies every planned Implementation node in the branch is realized (a scan found real code at its FQN), all Feedback is resolved, and derived solution coverage holds. On green it prints the merge handoff. Read-only; the merge itself is `apg project merge <project>` from the main checkout.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgPlanVerifyInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { project } = input
          if (!project) return { content: "Error: project is required" }
          return { content: await runInProject(merged, ["plan", "verify", project]) }
        },
      })
    })
  },
})
