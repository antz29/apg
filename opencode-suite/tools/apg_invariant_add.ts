import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Materialize a graph-wide invariant (`apg invariant add [<project>] <name> --title … --body … --category … --scope … [--guard <fqn>]*`). Without a project it is universal (`invariant/<name>`); with a project, project-scoped (`<project>/invariant/<name>`). category ∈ process|product|graph-integrity. Optionally guards artifacts (`GuardedBy`): a code, spec/plan, or domain/solution node the invariant applies to. Invariants are emergent — the flow works identically with zero invariants.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Optional spec project for a project-scoped invariant (omit for a universal invariant)."),
    name: tool.schema.string().describe("The invariant's short name, e.g. plan.task-kind-in-set (required)."),
    title: tool.schema.string().describe("One-line title (required)."),
    body: tool.schema.string().optional().describe("The rule body."),
    category: tool.schema
      .string()
      .describe("process (spec/plan/review structure), product (domain rules about the codebase), or graph-integrity (data rules about the graph) (required)."),
    scope: tool.schema.string().optional().describe("The artifact kind it applies to: spec, plan, review, code, …."),
    guard: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("Artifact FQNs the invariant guards (GuardedBy): a code node, spec/plan node, or domain/solution node."),
  },
  async execute(args, context) {
    const { name } = args
    if (!name) return "Error: name is required"
    const cli = ["invariant", "add"]
    if (args.project) cli.push(args.project)
    cli.push(name)
    if (args.title) cli.push("--title", args.title)
    if (args.body) cli.push("--body", args.body)
    if (args.category) cli.push("--category", args.category)
    if (args.scope) cli.push("--scope", args.scope)
    for (const g of args.guard ?? []) cli.push("--guard", g)
    return runCli(context, cli)
  },
})