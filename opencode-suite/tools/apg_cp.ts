import { Plugin } from "@opencode/plugin"
import fs from "node:fs"
import path from "node:path"
import { fsScopeDecision, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgCpInput = { project: string; from: string; to: string }

export default Plugin.define({
  id: "apg_cp",
  async setup(ctx) {
    await ctx.tool.transform((editor) =>
      editor.add({
        name: "apg_cp",
        options: { codemode: false },
        description:
          "Copy a filesystem path inside the acting agent's granted globs. Resolves the source and destination against the registered project's resolved worktree, then asserts the acting agent's `edit` permission on the resolved source and destination through opencode's permission engine — fail-closed: a missing permission surface or any denied path refuses the copy, and nothing is copied. Then a plain fs copy (no git).",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            from: {
              type: "string",
              description: "Source path (absolute, or relative to the project's worktree).",
            },
            to: {
              type: "string",
              description: "Destination path (absolute, or relative to the project's worktree).",
            },
          },
          required: ["project", "from", "to"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgCpInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          const { from, to } = input
          if (!from || !to) return { content: "Error: from and to are required" }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const worktree = merged.worktree

          // Both endpoints resolve against the registered project's resolved
          // worktree, then the acting agent's `edit` permission is asserted for
          // BOTH resolved paths in ONE call, so opencode's
          // any-deny/else-any-ask/else-allow precedence across the whole set
          // decides the operation: a copy whose source is allowed but whose
          // destination is denied is refused.
          const srcAbs = path.resolve(worktree, from)
          const dstAbs = path.resolve(worktree, to)
          const decision = await fsScopeDecision(ctx.permission, merged, [srcAbs, dstAbs], [srcAbs, dstAbs])
          // FAIL-CLOSED: a `deny` rule (or a rejected prompt) throws a `Tool.Error`
          // carrying the reason; propagate it so the model sees the refusal rather
          // than a copy that silently happened.
          if (!decision.allowed) throw new Error(decision.reason ?? "permission denied")

          try {
            fs.cpSync(srcAbs, dstAbs, { recursive: true })
          } catch (e) {
            return { content: `Error: ${(e as Error).message}` }
          }
          return { content: `Copied:\n${srcAbs}\n-> ${dstAbs}` }
        },
      }),
    )
  },
})
