import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Set the spine edges out of a tier node: `apg spec spine <project> <from-fqn> --drives <to> --requires <to> --realises <to> --represents <to> --implemented-by <to>`. The spine threads the tiers end to end: a Requirement drives/requires a Domain; a Domain realises/represents a Solution node (system/container/component); a Solution node is implemented-by code. Each edge kind is a set — repeated calls replace only that kind's outgoing edges. Targets are declared spec-node FQNs (a Requirement source may be given by bare id, e.g. R1); --implemented-by targets are code FQNs.",
  args: {
    project: tool.schema.string().describe("Spec project (required)."),
    from: tool.schema
      .string()
      .describe("The source node: a requirement id (e.g. R1) or a tier-node FQN, e.g. <project>/domain.Auth (required)."),
    drives: tool.schema.array(tool.schema.string()).optional().describe("Requirement → Domain edges (targets are domain FQNs)."),
    requires: tool.schema.array(tool.schema.string()).optional().describe("Requirement → Domain edges (targets are domain FQNs)."),
    realises: tool.schema.array(tool.schema.string()).optional().describe("Domain → Solution edges (targets are system/container/component FQNs)."),
    represents: tool.schema.array(tool.schema.string()).optional().describe("Domain → Solution edges (targets are system/container/component FQNs)."),
    implementedBy: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("Solution → code edges (targets are code FQNs, e.g. a Struct or Function FQN)."),
  },
  async execute(args, context) {
    const { project, from } = args
    if (!project || !from) return "Error: project and from are required"
    const cli = ["spec", "spine", project, from]
    for (const t of args.drives ?? []) cli.push("--drives", t)
    for (const t of args.requires ?? []) cli.push("--requires", t)
    for (const t of args.realises ?? []) cli.push("--realises", t)
    for (const t of args.represents ?? []) cli.push("--represents", t)
    for (const t of args.implementedBy ?? []) cli.push("--implemented-by", t)
    if (cli.length === 4) return "Error: provide at least one edge (--drives/--requires/--realises/--represents/--implemented-by)"
    return runCli(context, cli)
  },
})