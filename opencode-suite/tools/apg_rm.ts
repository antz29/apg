import { tool } from "@opencode-ai/plugin"
import fs from "node:fs"
import path from "node:path"
import { agentFsGlobs, canonicalPath, fsScopeDecision, mainCheckoutRoot } from "../lib/apg.ts"

export default tool({
  description:
    "Remove filesystem path(s) inside the acting agent's granted globs. Resolves each path against the caller's project `directory`, then SELF-ENFORCES scope: any path not matched by the acting agent's granted globs, or escaping the project/worktree boundary, is refused and NOTHING is removed. All paths are checked before any removal, then a plain fs unlink (no git). Set `recursive` to remove a directory tree.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory to resolve paths against. Defaults to the session's directory."),
    paths: tool.schema
      .array(tool.schema.string())
      .describe("One or more paths (absolute, or relative to `directory`) to remove."),
    recursive: tool.schema
      .boolean()
      .optional()
      .describe("Remove directories and their contents recursively (default false)."),
  },
  async execute(args, context) {
    const paths = args.paths ?? []
    if (paths.length === 0) return "Error: at least one path is required"
    const directory = args.directory || context.directory

    // The write boundary is anchored to the MAIN checkout, not the caller's
    // `directory`: a worktree's grants live in the main checkout's agent file
    // and are written against that root (`apg/.worktrees/<project>/<glob>`), so
    // the decision frame is resolved from the caller's directory but is NEVER
    // the caller's directory itself. Paths still resolve against the caller's
    // directory (the stored argument contract); only the SCOPE frame is main.
    // Each candidate is then canonicalised (via `canonicalPath`) into that same
    // symlink-resolved frame, so a `directory` spelled through a symlink and a
    // worktree-internal symlink pointing into main are judged on their real
    // target, not their textual spelling.
    const frame = await mainCheckoutRoot(context, directory)
    if (!frame) {
      return "Refused: could not resolve the main checkout root for the given directory (is it inside a git repository?) — nothing removed"
    }
    const granted = agentFsGlobs(context, frame.root, frame.root)

    const dir = canonicalPath(directory)
    // SCOPE is decided on BOTH the caller-NAMED entry (the directory entry the
    // op actually changes, `canonical parent + basename` — the link's OWN
    // location) AND the FULLY resolved candidate (following the final symlink).
    // The resolved check refuses a link whose TARGET is outside the grant; the
    // entry check refuses a link that LIVES outside the grant but points inside
    // it — e.g. a main-checkout link into the owned worktree, which would
    // otherwise be allowed (its target is in-grant) and delete the link's
    // location in main. The destructive op still targets the caller-NAMED path,
    // so POSIX symlink semantics hold — `rm link` unlinks the link, never the
    // file it points to.
    const targets = paths.map((p) => {
      const named = path.resolve(dir, p)
      const entry = path.join(canonicalPath(path.dirname(named)), path.basename(named))
      return { named, entry, resolved: canonicalPath(named) }
    })
    for (const { named, entry, resolved } of targets) {
      const entryDecision = fsScopeDecision(frame.root, entry, granted, frame.project)
      if (!entryDecision.allowed) return `Refused: ${named} — ${entryDecision.reason}`
      const resolvedDecision = fsScopeDecision(frame.root, resolved, granted, frame.project)
      if (!resolvedDecision.allowed) return `Refused: ${named} — ${resolvedDecision.reason}`
    }

    try {
      for (const { named } of targets) fs.rmSync(named, { recursive: args.recursive ?? false })
    } catch (e) {
      return `Error: ${(e as Error).message}`
    }
    return `Removed ${targets.length} path(s):\n${targets.map((t) => t.named).join("\n")}`
  },
})
