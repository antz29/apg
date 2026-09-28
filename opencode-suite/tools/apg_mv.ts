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
    // SCOPE is decided on BOTH the caller-NAMED entry (the directory entry the
    // rename actually changes, `canonical parent + basename` — the link's OWN
    // location) AND the FULLY resolved endpoint (following the final symlink),
    // for BOTH source and destination. The resolved check refuses a symlink
    // whose TARGET is outside the grant; the entry check refuses a symlink that
    // LIVES outside the grant but points inside it — e.g. a main-checkout link
    // into the owned worktree, whose rename would move the link out of main. The
    // rename acts on the caller-NAMED paths, so POSIX symlink semantics hold —
    // renaming a link renames the link, never the file it points to.
    const srcNamed = path.resolve(dir, from)
    const dstNamed = path.resolve(dir, to)
    for (const [label, named] of [
      ["source", srcNamed],
      ["destination", dstNamed],
    ] as const) {
      const entry = path.join(canonicalPath(path.dirname(named)), path.basename(named))
      const entryDecision = fsScopeDecision(frame.root, entry, granted, frame.project)
      if (!entryDecision.allowed) return `Refused: ${label} ${named} — ${entryDecision.reason}`
      const resolved = canonicalPath(named)
      const resolvedDecision = fsScopeDecision(frame.root, resolved, granted, frame.project)
      if (!resolvedDecision.allowed) return `Refused: ${label} ${named} — ${resolvedDecision.reason}`
    }

    try {
      fs.renameSync(srcNamed, dstNamed)
    } catch (e) {
      return `Error: ${(e as Error).message}`
    }
    return `Moved:\n${srcNamed}\n-> ${dstNamed}`
  },
})
