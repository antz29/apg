import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Remove a node (and all its incident edges) from a spec project: `apg spec rm <project> <fqn|id>`. Accepts a requirement/phase/decision/future id or a full future/<project>/... FQN. Write-through — the JSONL is updated and the live DB re-ingested.",
  args: {
    project: tool.schema.string().describe("Spec project (required)."),
    id: tool.schema
      .string()
      .describe("The node's id (e.g. R1, phase-2, gateway) or full future/<project>/... FQN (required)."),
  },
  async execute(args, context) {
    const { project, id } = args
    if (!project || !id) return "Error: project and id are required"
    return runCli(context, ["spec", "rm", project, id])
  },
})