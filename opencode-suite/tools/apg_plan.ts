import { Plugin } from "@opencode/plugin"
import { runCypher, lit, csvToRows, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_plan",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan",
        description:
          "Plan overview: each plan project with its strategy, phase table (number, title, deliverable, satisfied requirements), and per-phase task progress (done/total). Plans are transient — they live under apg/.trans/plans/<project>.jsonl and never commit.",
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
          let verbs: string[][]
          try {
            plans = csvToRows(await runCypher(merged, `MATCH (p:Plan)${where} RETURN p.fqn, p.title, p.strategy ORDER BY p.fqn`, input.directory))
            phases = csvToRows(await runCypher(merged, "MATCH (pp:PlanPhase) RETURN pp.fqn, pp.number, pp.title, pp.deliverable", input.directory))
            sat = csvToRows(await runCypher(merged, "MATCH (pp:PlanPhase)-[:Satisfies]->(r:Requirement) RETURN pp.fqn, r.fqn", input.directory))
            tasks = csvToRows(await runCypher(merged, "MATCH (pp:PlanPhase)-[:Contains]->(t:Task) RETURN pp.fqn, t.fqn, t.status", input.directory))
            verbs = csvToRows(await runCypher(merged, "MATCH (t:Task) RETURN t.fqn, t.verb, t.target", input.directory))
          } catch (e) {
            // Surface the verbatim `apg query failed …` message instead of an
            // opaque crash when the guard rejects a result.
            return { content: e instanceof Error ? e.message : String(e) }
          }
          if (plans.length <= 1) {
            return { content: "No plans found. Author one with `apg plan add <project> --strategy ...` (or the apg_plan_add tool)." }
          }

          const out: string[] = []
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
                .map((r) => {
                  const v = verbs.find((x) => x[0] === r[1])
                  return v && v[2] ? `${v[1]} ${v[2]}` : ""
                })
                .filter(Boolean)
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
