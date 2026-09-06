import { tool } from "@opencode-ai/plugin"
import { runCypher, lit, csvToRows, projectOf, resolvesInCode } from "../lib/apg.ts"

export default tool({
  description:
    "Lint the spec/plan graph for a project (or all projects). Reports: pending anchors (expected — planned future code), satisfiable futures (target code now exists — run promote), unsatisfied futures (planned code not yet built), orphan requirements (no Satisfies, no Implements), acceptance criteria without a covering requirement, spec drift (anchors to code that vanished), dangling depends_on/gates refs, and open/actioned review feedback.",
  args: {
    project: tool.schema
      .string()
      .optional()
      .describe("Spec project to lint (default: all projects)."),
  },
  async execute(args, context) {
    const where = args.project ? ` WHERE s.fqn = ${lit(`future/${args.project}/spec`)}` : ""
    const specs = csvToRows(await runCypher(context, `MATCH (s:Spec)${where} RETURN s.fqn`))
    if (specs.length <= 1) {
      return "No specs found — nothing to lint."
    }

    const reqRows = csvToRows(await runCypher(context, "MATCH (r:Requirement) RETURN r.fqn, r.id, r.title"))
    const futRows = csvToRows(await runCypher(context, "MATCH (f:Future) RETURN f.fqn, f.kind, f.target"))
    const ancRows = csvToRows(
      await runCypher(context, "MATCH (r:Requirement)-[:Anchors]->(t) RETURN r.fqn, t.fqn"),
    )
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
      const pfx = `future/${p}/`
      const inP = (rows: string[][], col = 0) => rows.filter((r) => (r[col] ?? "").startsWith(pfx))

      const reqs = inP(reqRows)
      const reqSet = new Set(reqs.map((r) => r[0]))
      const impl = new Set(inP(impRows).map((r) => r[0]))
      const satisfied = new Set(inP(satRows, 1).map((r) => r[1]))

      const pending: string[] = []
      const satisfiable: string[] = []
      const unsatisfied: string[] = []
      for (const [fqn, kind, target] of inP(futRows)) {
        if (target) {
          const ok = await resolvesInCode(context, target)
          const anchors = ancRows.filter((r) => r[1] === fqn)
          if (ok) satisfiable.push(`  ${fqn} (${kind}) — target ${target} now exists → \`apg spec promote ${p} ${fqn.slice(pfx.length)}\``)
          else unsatisfied.push(`  ${fqn} (${kind}) — target ${target} not in the code graph`)
          if (anchors.length === 0) pending.push(`  ${fqn} (${kind}) — no requirement anchors to it`)
        } else {
          unsatisfied.push(`  ${fqn} (${kind}) — no target declared`)
        }
      }
      // Anchors to future nodes = pending anchors (expected for future code).
      const pendingAnchors = ancRows.filter((r) => r[0].startsWith(pfx) && r[1].startsWith("future/"))

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
      // silently break `WHERE f.status = 'resolved'` and the archive/complete
      // gates.
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
        sections.push(`pending anchors (expected — future code, ${pendingAnchors.length}):`)
        for (const [, t] of pendingAnchors) sections.push(`  ${t}`)
      }
      if (satisfiable.length) {
        sections.push(`satisfiable futures — target code now exists, run \`apg spec promote\` (${satisfiable.length}):`)
        sections.push(...satisfiable)
      }
      if (unsatisfied.length) {
        sections.push(`unsatisfied futures — planned code not yet built (${unsatisfied.length}):`)
        sections.push(...unsatisfied)
      }
      if (pending.length) {
        sections.push(`unreferenced futures — no requirement anchors to them (${pending.length}):`)
        sections.push(...pending)
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
        sections.push(`feedback under review — must be resolved before archive/complete (${feedback.length}):`)
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