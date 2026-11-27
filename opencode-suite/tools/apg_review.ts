import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgReviewInput = { project: string; target?: string }

export default Plugin.define({
  id: "apg_review",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review",
        options: { codemode: false },
        description:
          "List review feedback: every Feedback with its status/disposition and the node it reviews, or filtered to one target (`apg review list [<target-fqn>]`). Feedback is transient (branch-local apg/.trans mirrors); read-only.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            target: {
              type: "string",
              description: "Only list feedback reviewing this node FQN (a layer node, plan/phase/task, or code).",
            },
          },
          required: ["project"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgReviewInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const cli = ["review", "list"]
          if (input.target) cli.push(input.target)
          return { content: await runInProject(merged, cli) }
        },
      })
    })
  },
})
