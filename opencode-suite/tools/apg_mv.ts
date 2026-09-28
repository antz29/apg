import { tool } from "@opencode-ai/plugin"
import fs from "node:fs"
import path from "node:path"
import { agentFsGlobs, canonicalPath, fsScopeDecision, mainCheckoutRoot } from "../lib/apg.ts"

export default tool({
  description:
    "Move/rename a filesystem path inside the acting agent's granted globs. Resolves BOTH source and destination against the caller's project `directory`, then SELF-ENFORCES scope: if EITHER path is not matched by the acting agent's granted globs, or escapes the project/worktree boundary, the move is refused and the tree is untouched. Then a plain fs rename (no git).",
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
    const directory = args.directory || context.directory

    // The write boundary is anchored to the MAIN checkout, not the caller's
    // `directory`: a worktree's grants live in the main checkout's agent file
    // and are written against that root (`apg/.worktrees/<project>/<glob>`), so
    // the decision frame is resolved from the caller's directory but is NEVER
    // the caller's directory itself. BOTH endpoints still resolve against the
    // caller's directory (the stored argument contract); only the SCOPE frame is
    // main. BOTH endpoints are then canonicalised (via `canonicalPath`) into
    // that same symlink-resolved frame (deepest existing ancestor realpath'd,
    // tail re-appended), so a symlinked `directory` and a symlink pointing into
    // main are judged on their real target, not their textual spelling.
    const frame = await mainCheckoutRoot(context, directory)
    if (!frame) {
      return "Refused: could not resolve the main checkout root for the given directory (is it inside a git repository?) — nothing moved"
    }
    const granted = agentFsGlobs(context, frame.root, frame.root)

    const dir = canonicalPath(directory)
    const src = canonicalPath(path.resolve(dir, from))
    const dst = canonicalPath(path.resolve(dir, to))
    for (const [label, abs] of [
      ["source", src],
      ["destination", dst],
    ] as const) {
      const decision = fsScopeDecision(frame.root, abs, granted, frame.project)
      if (!decision.allowed) return `Refused: ${label} ${abs} — ${decision.reason}`
    }

    try {
      fs.renameSync(src, dst)
    } catch (e) {
      return `Error: ${(e as Error).message}`
    }
    return `Moved:\n${src}\n-> ${dst}`
  },
})
