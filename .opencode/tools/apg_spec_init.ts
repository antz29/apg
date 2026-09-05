import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description: "Create a new spec project: `apg spec init <project> --title ... --goal ...`. Creates the Spec node and apg/specs/<project>.jsonl. The project name should be short and hyphenated, e.g. workitem-timer.",
  args: {
    project: tool.schema.string().describe("Spec project name, e.g. workitem-timer (required)."),
    title: tool.schema.string().optional().describe("Spec title (defaults to the project name)."),
    goal: tool.schema.string().optional().describe("One-line goal statement."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"
    const cli = ["spec", "init", project]
    if (args.title) cli.push("--title", args.title)
    if (args.goal) cli.push("--goal", args.goal)
    return runCli(context, cli)
  },
})