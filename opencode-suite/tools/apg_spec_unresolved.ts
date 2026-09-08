import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows, projectOf } from "../lib/apg.ts"

export default tool({
  description:
    "Lint the spec/plan graph for a project (or all projects). Reports: pending anchors (expected — proposed code), unbuilt planned code (planned nodes not yet realized), unreferenced planned nodes (no task Builds them), orphan requirements (no Satisfies, no Implements), acceptance criteria without a covering requirement, spec drift (anchors to code that vanished), dangling depends_on/gates refs, and open/actioned review feedback.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Spec project to lint (default: all projects)."),
  },
  async execute(args, context) {
    const where = args.project ? ` WHERE s.fqn = ${lit(`${args.project}/spec`)}` : ""
    const specs = csvToRows(await runCypher(context, `MATCH (s:Spec)${where} RETURN s.fqn`))
    if (specs.length <= 1) {
      return "No specs found — nothing to lint."
    }

    const reqRows = csvToRows(await runCypher(context, "MATCH (r:Requirement) RETURN r.fqn, r.id, r.title"))
    // Planned Implementation nodes (status: planned) — the plan-writer's
    // tier-4 additions (GraphModel-SPEC.md). The placeholder node is gone.
    const plannedRows: string[][] = []
    for (const label of ["Struct", "Function", "File", "Module"]) {
      plannedRows.push(
        ...csvToRows(
          await runCypher(context, `MATCH (n:${label}) WHERE n.status = 'planned' RETURN n.fqn`),
        ).slice(1),
      )
    }
    const solutionRows: string[][] = []
    for (const label of ["System", "Container", "Component"]) {
      solutionRows.push(
        ...csvToRows(await runCypher(context, `MATCH (n:${label}) RETURN n.fqn`)).slice(1),
      )
    }
    const ancRows = csvToRows(
      await runCypher(context, "MATCH (r:Requirement)-[:Anchors]->(t) RETURN r.fqn, t.fqn"),
    )
    const buildRows = csvToRows(await runCypher(context, "MATCH (t:Task)-[:Builds]->(p) RETURN t.fqn, p.fqn"))
    const impRows = csvToRows(await runCypher(context, "MATCH (c)-[:Implements]->(r:Requirement) RETURN r.fqn"))
    const satRows = csvToRows(await runCypher(context, "MATCH (p:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN p.fqn, r.fqn"))
    const depRows = csvToRows(await runCypher(context, "MATCH (a:Requirement)-[:DependsOn]->(b:Requirement) RETURN a.fqn, b.fqn"))
    const gateRows = csvToRows(await runCypher(context, "MATCH (a:Phase)-[:Gates]->(b:Phase) RETURN a.fqn, b.fqn"))
    const phaseRows = csvToRows(await runCypher(context, "MATCH (p:Phase) RETURN p.fqn"))
    const fbRows = csvToRows(
      await runCypher(context, "MATCH (f:Feedback) RETURN f.fqn, f.status, f.disposition"),
    )
    const acRows = csvToRows(
      await runCypher(context, "MATCH (c)-[:Contains]->(ac:AcceptanceCriterion) RETURN c.fqn, ac.fqn"),
    )

    const out: string[] = []
    for (const [specFqn] of specs.slice(1)) {
      const p = projectOf(specFqn) ?? specFqn
      const pfx = `${p}/`
      const inP = (rows: string[][], col = 0) => rows.filter((r) => (r[col] ?? "").startsWith(pfx))

      const reqs = inP(reqRows)
      const reqSet = new Set(reqs.map((r) => r[0]))
      const impl = new Set(inP(impRows).map((r) => r[0]))
      const satisfied = new Set(inP(satRows, 1).map((r) => r[1]))

      // Planned code: a planned node still marked `planned` in the DB is
      // unbuilt (a branch scan would have replaced it with present code).
      // Unreferenced = no task Builds it.
      const unbuilt = plannedRows.map(([fqn]) => `  ${fqn} (planned) — no real code at the FQN yet`)
      const plannedFqnSet = new Set(plannedRows.map((r) => r[0]))
      const builtFqnSet = new Set(buildRows.map((r) => r[1]))
      const unreferenced = plannedRows
        .filter(([fqn]) => !builtFqnSet.has(fqn))
        .map(([fqn]) => `  ${fqn} (planned) — no task Builds it`)

      // Pending anchors: target is a planned node or a proposed Solution node.
      const pendingTargets = new Set([...plannedFqnSet, ...solutionRows.map((r) => r[0])])
      const pendingAnchors = ancRows.filter(
        (r) => r[0].startsWith(pfx) && pendingTargets.has(r[1]),
      )

      const orphans = reqs.filter(
        (r) => !impl.has(r[0]) && !satisfied.has(r[0]),
      )

      // AC coverage: an AC in a PlanPhase that Satisfies no requirement is
      // uncovered; a spec-level AC with no requirements at all is uncovered.
      const planPhaseSat = new Set(satRows.map((r) => r[0]))
      const uncoveredAC: string[] = []
      for (const [container, ac] of acRows) {
        if (!container.startsWith(pfx)) continue
        if (container.includes("/plan.")) {
          if (!planPhaseSat.has(container)) uncoveredAC.push(`  ${ac} — in ${container} which Satisfies no requirement`)
        } else if (reqs.length === 0) {
          uncoveredAC.push(`  ${ac} — spec has no requirements to cover`)
        }
      }

      // Dangling depends_on: the target must be a requirement *somewhere* —
      // spec graphs merge into one space, so a cross-project target (another
      // spec's requirement) is not dangling. A gate target must be a phase
      // node (phases live under the spec's own `spec.phase-<n>` root).
      const allReqSet = new Set(reqRows.map((r) => r[0]))
      const allPhaseSet = new Set(phaseRows.map((r) => r[0]))
      const dangling = [
        ...depRows
          .filter((r) => r[0].startsWith(pfx) && !allReqSet.has(r[1]))
          .map((r) => `  depends_on ${r[0]} -> ${r[1]}`),
        ...gateRows
          .filter((r) => r[0].startsWith(pfx) && !allPhaseSet.has(r[1]))
          .map((r) => `  gates ${r[0]} -> ${r[1]}`),
      ]

      const feedback = fbRows.filter((r) => r[0].startsWith(pfx) && r[1] !== "resolved")

      // Drift lint: agents can't write these values (the CLI sets them), so a
      // value outside the closed vocabulary means hand-edited JSONL that would
      // silently break `WHERE f.status = 'resolved'` and the apply gate.
      const drift = fbRows
        .filter(
          (r) =>
            r[0].startsWith(pfx) &&
            (!["open", "actioned", "resolved"].includes(r[1]) ||
              !["", "fixed", "wont-fix", "rejected"].includes(r[2] ?? "")),
        )
        .map((r) => `  ${r[0]} (status: ${r[1]}, disposition: ${r[2]})`)

      const sections: string[] = []
      if (pendingAnchors.length) {
        sections.push(`pending anchors (expected — proposed code, ${pendingAnchors.length}):`)
        for (const [, t] of pendingAnchors) sections.push(`  ${t}`)
      }
      if (unbuilt.length) {
        sections.push(`unbuilt planned code — planned nodes not yet realized (${unbuilt.length}):`)
        sections.push(...unbuilt)
      }
      if (unreferenced.length) {
        sections.push(`unreferenced planned nodes — no task Builds them (${unreferenced.length}):`)
        sections.push(...unreferenced)
      }
      if (orphans.length) {
        sections.push(`orphan requirements — no Satisfies, no Implements (${orphans.length}):`)
        for (const [, id, title] of orphans) sections.push(`  ${id} — ${title}`)
      }
      if (uncoveredAC.length) {
        sections.push(`acceptance criteria without a covering requirement (${uncoveredAC.length}):`)
        sections.push(...uncoveredAC)
      }
      if (dangling.length) {
        sections.push(`dangling refs (${dangling.length}):`)
        sections.push(...dangling)
      }
      if (feedback.length) {
        sections.push(`feedback under review — must be resolved before apply (${feedback.length}):`)
        for (const [fqn, status] of feedback) sections.push(`  ${fqn} (${status})`)
      }
      if (drift.length) {
        sections.push(`feedback status/disposition drift — hand-edited JSONL, breaks the resolved gate (${drift.length}):`)
        sections.push(...drift)
      }

      if (sections.length) {
        out.push(`## ${p}`)
        out.push(...sections.map((s) => `- ${s}`))
      }
    }
    if (out.length === 0) return "Lint clean: no unresolved spec/plan issues found."
    return out.join("\n")
  },
})