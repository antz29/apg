import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Render a spec project as platform-template markdown from the graph: `apg spec render <project> [--out -]`. Default writes apg/.trans/specs/<project>.md (gitignored); pass out=stdout to print the markdown (the render is a projection — never edit it back into the graph).",
  args: {
    project: tool.schema.string().describe("Spec project to render (required)."),
    out: tool.schema
      .string()
      .optional()
      .describe('Output destination: "stdout" prints the markdown, otherwise the default apg/.trans/specs/<project>.md.'),
  },
  async execute(args, context) {
    const { project } = args
    if (!project) return "Error: project is required"
    if (args.out === "stdout") {
      return runCli(context, ["spec", "render", project, "--out", "-"])
    }
    return runCli(context, ["spec", "render", project])
  },
})