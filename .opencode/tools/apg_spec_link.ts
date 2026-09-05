import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Add/remove a requirement's DependsOn edges: `apg spec link <project> <req-id> --depends-on <id>…`. Each target id must be a declared requirement of the same project (write-time error otherwise).",
  args: {
    project: tool.schema.string().describe("Spec project (required)."),
    reqId: tool.schema.string().describe("Requirement id whose depends-on edges to set, e.g. R1 (required)."),
    dependsOn: tool.schema
      .array(tool.schema.string())
      .describe("Requirement ids this requirement consumes (required, at least one)."),
  },
  async execute(args, context) {
    const { project, reqId, dependsOn } = args
    if (!project || !reqId || !dependsOn?.length) return "Error: project, reqId, and at least one dependsOn id are required"
    const cli = ["spec", "link", project, reqId]
    for (const d of dependsOn) cli.push("--depends-on", d)
    return runCli(context, cli)
  },
})