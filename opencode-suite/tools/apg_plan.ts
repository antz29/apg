import { Plugin } from "@opencode/plugin"
import { runCypher, lit, csvToRows, type ToolContext } from "../lib/apg.ts"

/** The per-kind operation rel tables (SCHEMA.md task tier) with the kind and
 *  verb each one carries, in the projection's table order. */
const OPERATION_EDGES: [table: string, kind: string, verb: string][] = [
  ...["module", "file", "struct", "function"].flatMap((kind) =>
    ["create", "modify", "delete", "move", "rename"].map(
      (verb) => [`${kind[0].toUpperCase()}${kind.slice(1)}${verb[0].toUpperCase()}${verb.slice(1)}`, kind, verb] as [string, string, string],
    ),
  ),
]

/** Every task's assigned-to/implements rows, its per-kind operation rows and
 *  its `Target` destination, as one relation set (the DB has no `type()`
 *  function, so each table is named). */
function taskEdgeQuery(): string {
  return [
    ...OPERATION_EDGES.map(
      ([table, kind, verb]) =>
        `MATCH (t:Task)-[:${table}]->(x) RETURN t.fqn, '${kind}' AS kind, '${verb}' AS verb, x.fqn AS target`,
    ),
    "MATCH (t:Task)-[:Target]->(x) RETURN t.fqn, '' AS kind, 'target' AS verb, x.fqn AS target",
    "MATCH (t:Task)-[:AssignedTo]->(x:Agent) RETURN t.fqn, 'assigned' AS kind, '' AS verb, x.fqn AS target",
    "MATCH (t:Task)-[:Implements]->(x:Component) RETURN t.fqn, 'implements' AS kind, '' AS verb, x.fqn AS target",
  ].join(" UNION ALL ")
}

export default Plugin.define({
  id: "apg_plan",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan",
        options: { codemode: false },
        description:
          "Plan overview: each plan project with its strategy, phase table (number, title, deliverable, satisfied requirements), and per-phase task progress (done/total) plus its targets — derived from each task's assigned Agent, implements Component and per-kind operation edges. Plans are transient — they live under apg/.trans/plans/<project>.jsonl and never commit.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            project: {
              type: "string",
              description: "Restrict to one plan project (default: all).",
            },
          },
          required: [],
          additionalProperties: false,
        },
        async execute(input, context) {
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const where = input.project ? ` WHERE p.fqn = ${lit(`${input.project}/plan`)}` : ""
          let plans: string[][]
          let phases: string[][]
          let sat: string[][]
          let tasks: string[][]
          let edges: string[][]
          try {
            plans = csvToRows(await runCypher(merged, `MATCH (p:Plan)${where} RETURN p.fqn, p.title, p.strategy ORDER BY p.fqn`, input.directory))
            phases = csvToRows(await runCypher(merged, "MATCH (pp:PlanPhase) RETURN pp.fqn, pp.number, pp.title, pp.deliverable", input.directory))
            sat = csvToRows(await runCypher(merged, "MATCH (pp:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN pp.fqn, r.fqn", input.directory))
            tasks = csvToRows(await runCypher(merged, "MATCH (pp:PlanPhase)-[:Contains]->(t:Task) RETURN pp.fqn, t.fqn, t.status", input.directory))
            edges = csvToRows(await runCypher(merged, taskEdgeQuery(), input.directory))
          } catch (e) {
            // Surface the verbatim `apg query failed …` message instead of an
            // opaque crash when the guard rejects a result.
            return { content: e instanceof Error ? e.message : String(e) }
          }
          if (plans.length <= 1) {
            return { content: "No plans found. Author one with `apg plan add <project> --strategy ...` (or the apg_plan_add tool)." }
          }

          const out: string[] = []
          const ops = new Map<string, [string, string, string][]>()
          const dest = new Map<string, string>()
          for (const [fqn, kind, verb, target] of edges.slice(1)) {
            if (verb === "target") {
              dest.set(fqn, target)
              continue
            }
            const list = ops.get(fqn) ?? []
            list.push([kind, verb, target])
            ops.set(fqn, list)
          }
          for (const [planFqn, title, strategy] of plans.slice(1)) {
            const project = planFqn.replace("/plan", "")
            out.push(`## ${planFqn}\t${title}`)
            if (strategy) out.push(`  strategy: ${strategy.replace(/\n/g, " ")}`)
            for (const [pfqn, number, ptitle, deliverable] of phases.slice(1)) {
              if (!pfqn.startsWith(`${project}/plan.phase-`)) continue
              const satReqs = sat
                .filter((r) => r[0] === pfqn)
                .map((r) => r[1].replace("requirements.requirement.", ""))
                .join(",")
              const phaseTasks = tasks.filter((r) => r[0] === pfqn)
              const done = phaseTasks.filter((r) => r[2] === "done").length
              const targets = phaseTasks
                .flatMap((r) =>
                  (ops.get(r[1]) ?? []).map(([kind, verb, target]) => {
                    const to = dest.get(r[1])
                    const arrow = to && (verb === "rename" || verb === "move") ? ` -> ${to}` : ""
                    return verb ? `${kind} ${verb} ${target}${arrow}` : `${kind} ${target}`
                  }),
                )
                .join(",")
              out.push(
                `  Phase ${number} — ${ptitle}${deliverable ? ` (${deliverable})` : ""} | satisfies: ${satReqs || "-"} | tasks: ${done}/${phaseTasks.length} done | targets: ${targets || "-"}`,
              )
            }
          }
          return { content: out.join("\n") }
        },
      })
    })
  },
})
