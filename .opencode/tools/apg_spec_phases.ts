import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows } from "../lib/apg.ts"

export default tool({
  description:
    "List a spec project's phases (number, title) and their Gates edges (what each phase is gated on).",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
  },
  async execute(args, context) {
    const project = args.project
    if (!project) return "Error: project is required"

    const phases = csvToRows(
      await runCypher(
        context,
        `MATCH (p:Phase) WHERE p.fqn STARTS WITH ${lit(`future/${project}/spec.phase-`)} RETURN p.fqn, p.number, p.title ORDER BY p.number`,
      ),
    )
    if (phases.length <= 1) return `No phases for spec \`${project}\`.`

    const gates = new Map<string, string[]>()
    for (const [from, to] of csvToRows(
      await runCypher(
        context,
        `MATCH (a:Phase)-[:Gates]->(b:Phase) WHERE a.fqn STARTS WITH ${lit(`future/${project}/spec.phase-`)} RETURN a.fqn, b.fqn`,
      ),
    )) {
      if (from === "a.fqn") continue
      const key = from.replace(`future/${project}/spec.phase-`, "")
      gates.set(key, [...(gates.get(key) ?? []), to.replace(`future/${project}/spec.phase-`, "")])
    }

    const lines = ["number,title,gated-on"]
    for (const [fqn, number, title] of phases.slice(1)) {
      const n = fqn.replace(`future/${project}/spec.phase-`, "")
      lines.push(`${n},${`"${title.replace(/"/g, '""')}"`},${(gates.get(n) ?? []).join(";")}`)
    }
    return lines.join("\n")
  },
})