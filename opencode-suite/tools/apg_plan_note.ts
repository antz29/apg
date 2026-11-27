import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgPlanNoteInput = { project: string; task: string; body: string }

export default Plugin.define({
  id: "apg_plan_note",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_note",
        options: { codemode: false },
        description:
          "Attach a task note: `apg plan note <project> <task-fqn> --body …`. An implementer records a concern or deviation that arose during execution. The note serializes into the transient plan JSONL (branch-local, survives until the verify gate) and is surfaced to the human at the merge handoff.",
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
            body: { type: "string", description: "The note body (required)." },
          },
          required: ["project", "task", "body"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgPlanNoteInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { project, task, body } = input
          if (!project || !task || !body) return { content: "Error: project, task, and body are required" }
          const fqn = task.includes("/") ? task : `${project}/${task.replace(/^plan\./, "plan.")}`
          return { content: await runInProject(merged, ["plan", "note", project, fqn, "--body", body]) }
        },
      })
    })
  },
})
