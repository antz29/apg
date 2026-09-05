import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Anchor a requirement to a node: `apg spec anchor <project> <req-id> <fqn>`. fqn must be a resolved code FQN (use apg_find_symbol to look it up) or an existing future/<project>/<name> FQN (a Future declared with apg_spec_add future). An unresolvable FQN is an error — never auto-created.",
  args: {
    project: tool.schema.string().describe("Spec project (required)."),
    reqId: tool.schema.string().describe("Requirement id, e.g. R1 (required)."),
    fqn: tool.schema
      .string()
      .describe("Code FQN or future/<project>/<name> FQN to anchor to (required)."),
  },
  async execute(args, context) {
    const { project, reqId, fqn } = args
    if (!project || !reqId || !fqn) return "Error: project, reqId, and fqn are required"
    return runCli(context, ["spec", "anchor", project, reqId, fqn])
  },
})