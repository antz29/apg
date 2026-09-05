import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Promote a spec's planned code into the present: `apg spec promote <project> <future-name>|--all`. For each Anchors(req→Future) whose target resolves in the code graph: re-point the anchor to the real code node, add Implements(code→req), and retire the Future. Errors if the target does not resolve (never guesses). Use --all to promote every satisfiable Future.",
  args: {
    project: tool.schema.string().describe("Spec project (required)."),
    future: tool.schema
      .string()
      .optional()
      .describe("Future name (e.g. gateway) to promote. Omit or pass 'all' to promote every satisfiable future."),
  },
  async execute(args, context) {
    const { project } = args
    if (!project) return "Error: project is required"
    const name = args.future && args.future !== "all" ? args.future : "--all"
    return runCli(context, ["spec", "promote", project, name])
  },
})