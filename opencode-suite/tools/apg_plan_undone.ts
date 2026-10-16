import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_plan_undone",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_undone",
        options: { codemode: false },
        description:
          "Flip a done plan task back to pending: `apg plan undone <project> <task-fqn>`. A checklist correction only — it does not recreate retired planned nodes (the code stays in the present).",
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
          const fqn = task.includes("/") ? task : `${project}/${task}`
          return { content: await runCli(merged, ["plan", "undone", project, fqn], input.directory) }
        },
      })
    })
  },
})
