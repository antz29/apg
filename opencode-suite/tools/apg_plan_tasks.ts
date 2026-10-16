import { Plugin } from "@opencode/plugin"
import { runCypher, lit, csvToRows, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_plan_tasks",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_tasks",
        options: { codemode: false },
        description:
          "List a plan's tasks with phase, kind (owning role), tier (verification depth, test only), status, the Task→Implementation verb (creates/modifies/deletes/renames/moves) with its target FQN(s), and the new FQN for renames/moves. The implementation checklist view.",
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
          const pfx = `${project}/plan.`
          const limit = input.limit ? Math.max(1, Math.min(1000, Number(input.limit))) : 500

          let tasks: string[][]
          try {
            tasks = csvToRows(
              await runCypher(
                merged,
                `MATCH (pp:PlanPhase)-[:Contains]->(t:Task) WHERE pp.fqn STARTS WITH ${lit(pfx)} RETURN pp.fqn, t.fqn, t.title, t.kind, t.tier, t.status, t.verb, t.target, t.new_fqn ORDER BY t.fqn LIMIT ${limit}`,
                input.directory,
              ),
            )
          } catch (e) {
            // Surface the verbatim `apg query failed …` message instead of an
            // opaque crash (or a benign "no tasks") when the guard rejects the result.
            return { content: e instanceof Error ? e.message : String(e) }
          }
          if (tasks.length <= 1) return { content: `No tasks in plan \`${project}\`.` }

          const lines = ["task,phase,title,kind,tier,status,verb,target,new_fqn"]
          for (const [phase, fqn, title, kind, tier, status, verb, target, newFqn] of tasks.slice(1)) {
            if (input.status && status !== input.status) continue
            const short = fqn.replace(`${project}/`, "")
            const phaseShort = phase.replace(`${project}/`, "")
            lines.push(
              [
                short,
                phaseShort,
                `"${title.replace(/"/g, '""')}"`,
                kind,
                tier,
                status,
                verb,
                target,
                newFqn,
              ].join(","),
            )
          }
          return {
            content: lines.length > 1 ? lines.join("\n") : `No tasks match status=${input.status ?? "any"}`,
          }
        },
      })
    })
  },
})
