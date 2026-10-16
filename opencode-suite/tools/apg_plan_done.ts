import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_plan_done",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_done",
        description:
          "Mark a plan task done: `apg plan done <project> <task-fqn>`. The implementer's assertion ONLY — no promotion, no code-graph verification (the plan's planned nodes stay declared until the verify gate's coherence check). Use apg_plan_undone to reverse.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            project: { type: "string", description: "Plan project (required)." },
            task: {
              type: "string",
              description:
                "Task FQN or short id, e.g. <project>/plan.phase-01.task-1 or plan.phase-01.task-1 (required).",
            },
          },
          required: ["project", "task"],
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
          const { project, task } = input
          if (!project || !task) return { content: "Error: project and task are required" }
          const fqn = task.includes("/") ? task : `${project}/${task.replace(/^plan\./, "plan.")}`
          return { content: await runCli(merged, ["plan", "done", project, fqn], input.directory) }
        },
      })
    })
  },
})
