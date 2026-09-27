import { tool } from "@opencode-ai/plugin"
import fs from "node:fs"
import path from "node:path"
import { agentFsGlobs, fsScopeDecision } from "../lib/apg.ts"

export default tool({
  description:
    "Copy a filesystem path inside the acting agent's granted globs. Resolves BOTH source and destination against the caller's project `directory`, then SELF-ENFORCES scope: if EITHER path is not matched by the acting agent's granted globs, or escapes the project/worktree boundary, the copy is refused and the tree is untouched. Then a plain fs copy (no git).",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory to resolve paths against. Defaults to the session's directory."),
    from: tool.schema.string().describe("Source path (absolute, or relative to `directory`)."),
    to: tool.schema.string().describe("Destination path (absolute, or relative to `directory`)."),
  },
  async execute(args, context) {
    const { from, to } = args
    if (!from || !to) return "Error: from and to are required"
    const root = args.directory || context.directory
    const granted = agentFsGlobs(context, root)

    const src = path.isAbsolute(from) ? path.resolve(from) : path.resolve(root, from)
    const dst = path.isAbsolute(to) ? path.resolve(to) : path.resolve(root, to)
    for (const [label, abs] of [
      ["source", src],
      ["destination", dst],
    ] as const) {
      const decision = fsScopeDecision(root, abs, granted)
      if (!decision.allowed) return `Refused: ${label} ${abs} — ${decision.reason}`
    }

    try {
      fs.cpSync(src, dst, { recursive: true })
    } catch (e) {
      return `Error: ${(e as Error).message}`
    }
    return `Copied:\n${src}\n-> ${dst}`
  },
})
