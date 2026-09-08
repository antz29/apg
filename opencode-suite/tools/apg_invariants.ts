import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "List the active graph-wide invariants (`apg invariants [--scope …] [--project …]`): fqn, title, category (process|product|graph-integrity), scope, status. The awareness tool for writers and reviewers — query it to see which known rules should hold while authoring. Filter by scope (substring) or project. Invariants are emergent; an empty list is normal.",
  args: {
    scope: tool.schema.string().optional().describe("Only invariants whose scope contains this (e.g. plan, spec, code)."),
    project: tool.schema.string().optional().describe("Only project-scoped invariants of this project."),
  },
  async execute(args, context) {
    const cli = ["invariants"]
    if (args.scope) cli.push("--scope", args.scope)
    if (args.project) cli.push("--project", args.project)
    return runCli(context, cli)
  },
})