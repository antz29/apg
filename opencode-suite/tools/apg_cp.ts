import { tool } from "@opencode-ai/plugin"
import fs from "node:fs"
import path from "node:path"
import { agentFsGlobs, canonicalPath, fsScopeDecision, mainCheckoutRoot } from "../lib/apg.ts"

export default tool({
  description:
    "Copy a filesystem path inside the acting agent's granted globs. Resolves the source and destination against the caller's project `directory`, then SELF-ENFORCES scope on the DESTINATION (the write target): if it is not matched by the acting agent's granted globs, resolves into the main checkout, or escapes the project/worktree boundary, the copy is refused and the tree is untouched. Then a plain fs copy (no git).",
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
    // the caller's directory itself. Both endpoints still resolve against the
    // caller's directory (the stored argument contract); only the SCOPE frame is
    // main. A copy WRITES only at the destination, so that is what scope is
    // decided for (the source is read, not written). Both endpoints are
    // canonicalised (via `canonicalPath`) into the same symlink-resolved frame
    // `mainCheckoutRoot` resolved the root in, so a symlinked `directory` and a
    // symlink pointing into main are judged on their real target, not their
    // textual spelling.
    const frame = await mainCheckoutRoot(context, directory)
    if (!frame) {
      return "Refused: could not resolve the main checkout root for the given directory (is it inside a git repository?) — nothing copied"
    }
    const granted = agentFsGlobs(context, frame.root, frame.root)

    const dir = canonicalPath(directory)
    // The destination WRITE is decided on the FULLY resolved path (a destination
    // symlink pointing into the main checkout, or outside the grant, is refused
    // before any write); the copy then acts on the caller-NAMED paths, so a
    // destination symlink is dereferenced at most to the resolved path the scope
    // check already allowed — never silently to one it refused.
    const srcNamed = path.resolve(dir, from)
    const dstNamed = path.resolve(dir, to)
    const dstResolved = canonicalPath(dstNamed)
    const decision = fsScopeDecision(frame.root, dstResolved, granted, frame.project)
    if (!decision.allowed) return `Refused: destination ${dstResolved} — ${decision.reason}`

    try {
      fs.cpSync(srcNamed, dstNamed, { recursive: true })
    } catch (e) {
      return `Error: ${(e as Error).message}`
    }
    return `Copied:\n${srcNamed}\n-> ${dstNamed}`
  },
})
