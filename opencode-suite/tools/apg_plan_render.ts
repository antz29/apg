import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Render a plan as PLAN.md-style markdown from the graph: `apg plan render <project> [--out -]`. Default writes apg/.trans/plans/<project>.md (gitignored); pass out=stdout to print it. The render is a projection — never edit it back into the graph.",
  args: {
    project: tool.schema.string().describe("Plan project to render (required)."),
    out: tool.schema
      .string()
      .optional()
      .describe('Output destination: "stdout" prints the markdown, otherwise the default apg/.trans/plans/<project>.md.'),
  },
  async execute(args, context) {
    const { project } = args
    if (!project) return "Error: project is required"
    if (args.out === "stdout") return runCli(context, ["plan", "render", project, "--out", "-"])
    return runCli(context, ["plan", "render", project])
  },
})