import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a spec project's dependency edges: requirement DependsOn (consumes) pairs and cross-spec SpecDependsOn (antecedent) pairs.",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"

    const lines: string[] = []
    const reqDeps = csvToRows(
      await runCypher(
        context,
        `MATCH (a:Requirement)-[:DependsOn]->(b:Requirement) WHERE a.fqn STARTS WITH ${lit(`future/${project}/spec.`)} RETURN a.fqn, b.fqn ORDER BY a.fqn`,
      ),
    )
    lines.push("Requirement DependsOn (consumes)")
    if (reqDeps.length <= 1) {
      lines.push("  (none)")
    } else {
      for (const [from, to] of reqDeps.slice(1)) {
        const fromId = from.replace(`future/${project}/spec.`, "")
        const toId = to.startsWith(`future/${project}/`)
          ? to.replace(`future/${project}/spec.`, "")
          : to.replace("future/", "").replace("/spec.", "/")
        lines.push(`  ${fromId} -> ${toId}`)
      }
    }

    const specDeps = csvToRows(
      await runCypher(
        context,
        `MATCH (a:Spec)-[:SpecDependsOn]->(b:Spec) WHERE a.fqn = ${lit(`future/${project}/spec`)} RETURN a.fqn, b.fqn`,
      ),
    )
    lines.push("Cross-spec SpecDependsOn (antecedent)")
    if (specDeps.length <= 1) {
      lines.push("  (none)")
    } else {
      for (const [, to] of specDeps.slice(1)) {
        lines.push(`  ${project} -> ${to.replace("future/", "").replace("/spec", "")}`)
      }
    }
    return lines.join("\n")
  },
})