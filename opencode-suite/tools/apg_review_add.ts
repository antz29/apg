import { Plugin } from "@opencode/plugin"
import { resolveProjectWorktree, runInProject, type ToolContext } from "../lib/apg.ts"

type ApgReviewAddInput = { project: string; target: string; body: string }

export default Plugin.define({
  id: "apg_review_add",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_review_add",
        options: { codemode: false },
        description:
          "Attach review feedback to an artifact node (`apg review add <target-fqn> --body ... [--project <p>]`): a durable layer node (`<layer>.<type>.<name>`, e.g. requirements.requirement.place-order), a plan/phase/task FQN (`<project>/plan…`), or a code FQN. Creates an open Feedback. Feedback is transient — it routes into the branch's apg/.trans mirror (plan targets into the plan store, durable/code targets into the tier mirror) and dies with the branch. The target must exist in the graph; durable-layer and code targets need --project to route the feedback.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            target: {
              type: "string",
              description: "FQN of the node being reviewed — a layer node (`requirements.requirement.x`), a plan/phase/task FQN, or a code FQN (required).",
            },
            body: { type: "string", description: "The review comment / issue (required)." },
          },
          required: ["project", "target", "body"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgReviewAddInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const { target, body } = input
          if (!target || !body) return { content: "Error: target and body are required" }
          const cli = ["review", "add", target, "--body", body]
          if (input.project) cli.push("--project", input.project)
          return { content: await runInProject(merged, cli) }
        },
      })
    })
  },
})
