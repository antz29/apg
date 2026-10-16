import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_review",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review",
        description:
          "List review feedback: every Feedback with its status/disposition and the node it reviews, or filtered to one target (`apg review list [<target-fqn>]`). Feedback is transient (branch-local apg/.trans mirrors); read-only.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            target: {
              type: "string",
              description: "Only list feedback reviewing this node FQN (a layer node, plan/phase/task, or code).",
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
          const cli = ["review", "list"]
          if (input.target) cli.push(input.target)
          return { content: await runCli(merged, cli, input.directory) }
        },
      })
    })
  },
})
