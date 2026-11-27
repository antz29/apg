import { Plugin } from "@opencode/plugin"
import fs from "node:fs"
import path from "node:path"
import { fsScopeDecision, resolveProjectWorktree, type ToolContext } from "../lib/apg.ts"

type ApgRmInput = { project: string; paths: string[]; recursive?: boolean }

export default Plugin.define({
  id: "apg_rm",
  async setup(ctx) {
    await ctx.tool.transform((editor) =>
      editor.add({
        name: "apg_rm",
        options: { codemode: false },
        description:
          "Remove filesystem path(s) inside the acting agent's granted globs. Resolves every path against the registered project's resolved worktree, then asserts the acting agent's `edit` permission on ALL resolved paths through opencode's permission engine in ONE call — fail-closed: a missing permission surface or any denied path refuses the removal, and nothing is removed. Then a plain fs unlink (no git). Set `recursive` to remove a directory tree.",
        input: {
          type: "object",
          properties: {
            project: {
              type: "string",
              description: "The registered project name (as recorded by `apg project start` / `apg init`). The tool resolves it to that project's worktree.",
            },
            paths: {
              type: "array",
              items: { type: "string" },
              description: "One or more paths (absolute, or relative to the project's worktree) to remove.",
            },
            recursive: {
              type: "boolean",
              description: "Remove directories and their contents recursively (default false).",
            },
          },
          required: ["project", "paths"],
          additionalProperties: false,
        },
        async execute(raw, context) {
          const input = raw as ApgRmInput
          const merged: ToolContext = {
            agent: context.agent,
            sessionID: context.sessionID,
            messageID: context.messageID,
            id: context.id,
            location: ctx.location,
          }
          merged.projectName = input.project
          merged.worktree = await resolveProjectWorktree(merged, input.project)
          const paths = input.paths ?? []
          if (paths.length === 0) return { content: "Error: at least one path is required" }
          const worktree = merged.worktree

          // Every path resolves against the registered project's resolved
          // worktree, then the acting agent's `edit` permission is asserted for
          // ALL resolved paths in ONE call, so opencode's
          // any-deny/else-any-ask/else-allow precedence across the whole set
          // decides the operation: a removal whose paths are all allowed
          // proceeds, and any denied path refuses the whole removal.
          const absPaths = paths.map((p) => path.resolve(worktree, p))
          const decision = await fsScopeDecision(ctx.permission, merged, absPaths, absPaths)
          // FAIL-CLOSED: a `deny` rule (or a rejected prompt) throws a `Tool.Error`
          // carrying the reason; propagate it so the model sees the refusal rather
          // than a removal that silently happened.
          if (!decision.allowed) throw new Error(decision.reason ?? "permission denied")

          try {
            for (const p of absPaths) fs.rmSync(p, { recursive: input.recursive ?? false })
          } catch (e) {
            return { content: `Error: ${(e as Error).message}` }
          }
          return { content: `Removed ${absPaths.length} path(s):\n${absPaths.join("\n")}` }
        },
      }),
    )
  },
})
