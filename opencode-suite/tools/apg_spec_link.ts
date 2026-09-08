import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Set a requirement's DependsOn edges, or a spec's cross-spec SpecDependsOn edges: `apg spec link <project> <req-id|spec> --depends-on <id|proj/id>…`. For a requirement, targets are same-project ids (`R4`) or cross-project `<project>/<id>` (a requirement in another spec). For the spec node itself, pass reqId `spec` and target other spec projects (`--depends-on <project>`) to declare whole-spec antecedents. Cycles (across all spec projects) and undeclared targets are write-time errors.",
  args: {
    project: tool.schema.string().describe("Spec project (required)."),
    reqId: tool.schema
      .string()
      .describe("Requirement id whose depends-on edges to set, e.g. R1 — or the literal `spec` to link the spec node to other specs (required)."),
    dependsOn: tool.schema
      .array(tool.schema.string())
      .describe("For a requirement: ids this requirement consumes (`R4`, or `other-proj/R9` for a cross-project requirement). For `spec`: other spec project names this spec depends on. (required, at least one)."),
  },
  async execute(args, context) {
    const { project, reqId, dependsOn } = args
    if (!project || !reqId || !dependsOn?.length) return "Error: project, reqId, and at least one dependsOn target are required"
    const cli = ["spec", "link", project, reqId]
    for (const d of dependsOn) cli.push("--depends-on", d)
    return runCli(context, cli)
  },
})