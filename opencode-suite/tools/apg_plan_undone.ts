import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgPlanUndoneInput = { project: string; task: string }

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
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            task: {
              type: "string",
              description:
                "Task FQN or short id, e.g. <project>/plan.phase-01.task-1 or plan.phase-01.task-1 (required).",
            },
          },
          required: ["project", "task"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgPlanUndoneInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { project, task } = input
          if (!project || !task) return { content: "Error: project and task are required" }
          const fqn = task.includes("/") ? task : `${project}/${task}`
          return { content: await runInProject(merged, ["plan", "undone", project, fqn]) }
        },
      })
    })
  },
})
