import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgPlanRenderInput = { project: string; out?: string }

export default Plugin.define({
  id: "apg_plan_render",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_render",
        options: { codemode: false },
        description:
          "Render a plan as PLAN.md-style markdown from the graph: `apg plan render <project> [--out -]`. Default writes apg/.trans/plans/<project>.md (gitignored); pass out=stdout to print it. The render is a projection — never edit it back into the graph.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            out: {
              type: "string",
              description:
                'Output destination: "stdout" prints the markdown, otherwise the default apg/.trans/plans/<project>.md.',
            },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgPlanRenderInput
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
          if (input.out === "stdout") {
            return { content: await runInProject(merged, ["plan", "render", project, "--out", "-"]) }
          }
          return { content: await runInProject(merged, ["plan", "render", project]) }
        },
      })
    })
  },
})
