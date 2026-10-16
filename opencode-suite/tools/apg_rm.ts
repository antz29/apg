import { Plugin } from "@opencode/plugin"
import fs from "node:fs"
import path from "node:path"
import { fsScopeDecision, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_rm",
  async setup(ctx) {
    await ctx.tool.transform((editor) =>
      editor.add({
        name: "apg_rm",
        options: { codemode: false },
        description:
          "Remove filesystem path(s) inside the acting agent's granted globs. Resolves every path against the caller's project `directory`, then asserts the acting agent's `edit` permission on ALL resolved paths through opencode's permission engine in ONE call — fail-closed: a missing permission surface or any denied path refuses the removal, and nothing is removed. Then a plain fs unlink (no git). Set `recursive` to remove a directory tree.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description:
                "Project root directory to resolve paths against. Defaults to the session's directory.",
            },
            paths: {
              type: "array",
              items: { type: "string" },
              description: "One or more paths (absolute, or relative to `directory`) to remove.",
            },
            recursive: {
              type: "boolean",
              description: "Remove directories and their contents recursively (default false).",
            },
          },
          required: ["paths"],
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
          const paths = input.paths ?? []
          if (paths.length === 0) return { content: "Error: at least one path is required" }
          const directory = input.directory || ctx.location?.directory || ""

          // Every path resolves against the caller's `directory` (the stored
          // argument contract), then the acting agent's `edit` permission is
          // asserted for ALL resolved paths in ONE call, so opencode's
          // any-deny/else-any-ask/else-allow precedence across the whole set
          // decides the operation: a removal whose paths are all allowed
          // proceeds, and any denied path refuses the whole removal.
          const absPaths = paths.map((p) => path.resolve(directory, p))
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
