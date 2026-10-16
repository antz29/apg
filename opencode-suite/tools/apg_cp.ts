import { Plugin } from "@opencode/plugin"
import fs from "node:fs"
import path from "node:path"
import { fsScopeDecision, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_cp",
  async setup(ctx) {
    await ctx.tool.transform((editor) =>
      editor.add({
        name: "apg_cp",
        description:
          "Copy a filesystem path inside the acting agent's granted globs. Resolves the source and destination against the caller's project `directory`, then asserts the acting agent's `edit` permission on the resolved source and destination through opencode's permission engine — fail-closed: a missing permission surface or any denied path refuses the copy, and nothing is copied. Then a plain fs copy (no git).",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description:
                "Project root directory to resolve paths against. Defaults to the session's directory.",
            },
            from: {
              type: "string",
              description: "Source path (absolute, or relative to `directory`).",
            },
            to: {
              type: "string",
              description: "Destination path (absolute, or relative to `directory`).",
            },
          },
          required: ["from", "to"],
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
          const { from, to } = input
          if (!from || !to) return { content: "Error: from and to are required" }
          const directory = input.directory || ctx.location?.directory || ""

          // Both endpoints resolve against the caller's `directory` (the stored
          // argument contract), then the acting agent's `edit` permission is
          // asserted for BOTH resolved paths in ONE call, so opencode's
          // any-deny/else-any-ask/else-allow precedence across the whole set
          // decides the operation: a copy whose source is allowed but whose
          // destination is denied is refused.
          const srcAbs = path.resolve(directory, from)
          const dstAbs = path.resolve(directory, to)
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
