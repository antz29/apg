import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgPlanDoneInput = { project: string; task: string }

export default Plugin.define({
  id: "apg_plan_done",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_done",
        options: { codemode: false },
        description:
          "Mark a plan task done: `apg plan done <project> <task-fqn>`. The implementer's assertion ONLY — no promotion, no code-graph verification (the plan's planned nodes stay declared until the verify gate's coherence check). Use apg_plan_undone to reverse.",
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
          const input = raw as ApgPlanDoneInput
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
          const fqn = task.includes("/") ? task : `${project}/${task.replace(/^plan\./, "plan.")}`
          return { content: await runInProject(merged, ["plan", "done", project, fqn]) }
        },
      })
    })
  },
})
