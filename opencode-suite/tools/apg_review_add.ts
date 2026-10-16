import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_review_add",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_add",
        description:
          "Attach review feedback to an artifact node (`apg review add <target-fqn> --body ... [--project <p>]`): a durable layer node (`<layer>.<type>.<name>`, e.g. requirements.requirement.place-order), a plan/phase/task FQN (`<project>/plan…`), or a code FQN. Creates an open Feedback. Feedback is transient — it routes into the branch's apg/.trans mirror (plan targets into the plan store, durable/code targets into the tier mirror) and dies with the branch. The target must exist in the graph; durable-layer and code targets need --project to route the feedback.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            target: {
              type: "string",
              description: "FQN of the node being reviewed — a layer node (`requirements.requirement.x`), a plan/phase/task FQN, or a code FQN (required).",
            },
            body: { type: "string", description: "The review comment / issue (required)." },
            project: {
              type: "string",
              description: "Required when the target is a code FQN or a durable layer FQN (routes the feedback into the branch's .trans mirror).",
            },
          },
          required: ["target", "body"],
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
          const { target, body } = input
          if (!target || !body) return { content: "Error: target and body are required" }
          const cli = ["review", "add", target, "--body", body]
          if (input.project) cli.push("--project", input.project)
          return { content: await runCli(merged, cli, input.directory) }
        },
      })
    })
  },
})
