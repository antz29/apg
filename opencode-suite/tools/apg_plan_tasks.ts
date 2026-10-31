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

/** The task listing Cypher: every task under the project's phases with its
 *  optional assigned Agent and implemented Component. The status predicate
 *  joins the phase-prefix predicate in the one WHERE, so the LIMIT bounds the
 *  filtered set instead of a pre-filter page. */
export function tasksQuery(project: string, status: string | undefined, limit: number): string {
  const where = [`pp.fqn STARTS WITH ${lit(`${project}/plan.`)}`]
  if (status) where.push(`t.status = ${lit(status)}`)
  return `MATCH (pp:PlanPhase)-[:Contains]->(t:Task)
 WHERE ${where.join(" AND ")}
 OPTIONAL MATCH (t)-[:AssignedTo]->(a:Agent)
 OPTIONAL MATCH (t)-[:Implements]->(c:Component)
 RETURN pp.fqn, t.fqn, t.title, t.status, a.fqn, c.fqn ORDER BY t.fqn LIMIT ${limit}`
}

/** The page verdict for a task listing: the data rows (the CSV header already
 *  stripped) clamped to exactly `limit`, plus the notice to show in place of
 *  the table (zero rows) or beneath it (truncated). An exactly-`limit` page is
 *  complete and carries no notice, so the caller fetches one probe row past
 *  the limit — only that extra row can prove the page was truncated. */
export function tasksPageNotice(
  project: string,
  status: string | undefined,
  limit: number,
  rows: string[][],
): { rows: string[][]; notice?: string } {
  if (rows.length === 0)
    return {
      rows,
      notice: status ? `No tasks match status=${status}` : `No tasks in plan \`${project}\`.`,
    }
  if (rows.length > limit)
    return {
      rows: rows.slice(0, limit),
      notice: `Truncated to the first ${limit} tasks; raise \`limit\` to see more.`,
    }
  return { rows }
}

/** Every task's operation rel rows plus its `Target` destination rows, as one
 *  relation set (the DB has no `type()` function, so each table is named). */
function operationQuery(): string {
  return [
    ...OPERATION_EDGES.map(
      ([table, kind, verb]) =>
        `MATCH (t:Task)-[:${table}]->(x) RETURN t.fqn, '${kind}' AS kind, '${verb}' AS verb, x.fqn AS target`,
    ),
    "MATCH (t:Task)-[:Target]->(x) RETURN t.fqn, '' AS kind, 'target' AS verb, x.fqn AS target",
  ].join(" UNION ALL ")
}

export default Plugin.define({
  id: "apg_plan_tasks",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_tasks",
        options: { codemode: false },
        description:
          "List a plan's tasks with phase, title, status, the assigned Agent (the assigned-to edge), the implemented Component (the implements edge) and the per-kind operation edges (kind, verb and target, plus a renames/moves destination). A task whose assigned Agent is absent from the branch graph is marked `(drift)`. The implementation checklist view.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            project: { type: "string", description: "Plan project (required)." },
            status: {
              type: "string",
              description: 'Only tasks in this state: "pending" or "done".',
            },
            limit: { type: "string", description: "Max rows (default 500)." },
          },
          required: ["project"],
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
          const project = input.project
          if (!project) return { content: "Error: project is required" }
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 500

          let tasks: string[][]
          let edges: string[][]
          try {
            tasks = csvToRows(
              await runCypher(merged, tasksQuery(project, input.status, limit + 1), input.directory),
            )
            edges = csvToRows(await runCypher(merged, operationQuery(), input.directory))
          } catch (e) {
            // Surface the verbatim `apg query failed …` message instead of an
            // opaque crash (or a benign "no tasks") when the guard rejects the result.
            return { content: e instanceof Error ? e.message : String(e) }
          }
          // The header row is stripped before the page verdict, so a
          // status-filtered empty result is a zero-match, never "no plan".
          const page = tasksPageNotice(project, input.status, limit, tasks.slice(1))
          if (page.rows.length === 0 && page.notice) return { content: page.notice }

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

          const lines = ["task,phase,title,status,assignedTo,implements,operations"]
          for (const [phase, fqn, title, status, assigned, component] of page.rows) {
            const short = fqn.replace(`${project}/`, "")
            const phaseShort = phase.replace(`${project}/`, "")
            // The projection drops an AssignedTo edge whose Agent is absent
            // from the branch graph, so a task with no edge is a drifted one.
            const operations = (ops.get(fqn) ?? [])
              .map(([kind, verb, target]) => {
                const to = dest.get(fqn)
                const arrow = to && (verb === "rename" || verb === "move") ? ` -> ${to}` : ""
                return `${kind} ${verb} ${target}${arrow}`
              })
              .join("; ")
            lines.push(
              [
                short,
                phaseShort,
                `"${title.replace(/"/g, '""')}"`,
                status,
                assigned || "(drift)",
                component,
                `"${operations}"`,
              ].join(","),
            )
          }
          const table = lines.join("\n")
          return { content: page.notice ? `${table}\n\n${page.notice}` : table }
        },
      })
    })
  },
})
