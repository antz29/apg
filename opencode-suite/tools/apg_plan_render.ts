import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

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
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            project: { type: "string", description: "Plan project to render (required)." },
            out: {
              type: "string",
              description:
                'Output destination: "stdout" prints the markdown, otherwise the default apg/.trans/plans/<project>.md.',
            },
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
          if (input.out === "stdout") {
            return { content: await runCli(merged, ["plan", "render", project, "--out", "-"], input.directory) }
          }
          return { content: await runCli(merged, ["plan", "render", project], input.directory) }
        },
      })
    })
  },
})
