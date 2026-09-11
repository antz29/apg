import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Durable node-file edge mutations (`apg edge add|update|rm <kind> <from> <to> [--property k=v]*`). Writes BOTH endpoint files in one atomic, auto-committed mutation — the out half in the source's file, the matching in half in the target's. Kinds (SPEC §3.3): contains, drives, realised-by, implemented-by, calls, publishes, subscribes, depends-on, uses, represents, details. Endpoints are authored FQNs (`<layer>.<type>.<name>`) or, for implemented-by/details targets, code FQNs validated against the scanned graph (a planned code FQN is pending; a vanished one is drift and refused). Both endpoints must already exist. `add` refuses an identical `(kind,from,to)` already on the source (no implicit upsert); `update` is properties-only (kind/from/to are identity: it MERGEs `--property` and applies `--unset-property`, rewriting both halves to the same map) and refuses an absent edge; `rm` removes the edge from both endpoint files.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    action: tool.schema.string().describe('"add", "update", or "rm" (required).'),
    kind: tool.schema
      .string()
      .describe("Edge kind: contains, drives, realised-by, implemented-by, calls, publishes, subscribes, depends-on, uses, represents, details (required)."),
    from: tool.schema.string().describe("Source FQN (required)."),
    to: tool.schema.string().describe("Target FQN — an authored FQN or, for implemented-by/details, a code FQN (required)."),
    properties: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For add/update: `k=v` edge properties (repeatable), e.g. a coupling flavor on calls/publishes/subscribes. On update these MERGE (only the passed keys change on both halves)."),
    unsetProperties: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For update: property keys to delete explicitly (repeatable). Omitting this never drops a key."),
  },
  async execute(args, context) {
    const { action, kind, from, to } = args
    if (!action || !kind || !from || !to) return "Error: action, kind, from, and to are required"
    if (action !== "add" && action !== "update" && action !== "rm") {
      return 'Error: action must be "add", "update", or "rm"'
    }
    const cli = ["edge", action, kind, from, to]
    if (action === "add" || action === "update") {
      for (const p of args.properties ?? []) cli.push("--property", p)
    }
    if (action === "update") {
      for (const u of args.unsetProperties ?? []) cli.push("--unset-property", u)
    }
    return runCli(context, cli, args.directory)
  },
})
