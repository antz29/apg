import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Durable node-file edge mutations (`apg edge add|rm <kind> <from> <to> [--property k=v]*`). Writes BOTH endpoint files in one atomic, auto-committed mutation — the out half in the source's file, the matching in half in the target's. Kinds (SPEC §3.3): contains, drives, realised-by, implemented-by, calls, publishes, subscribes, depends-on, uses, represents, details. Endpoints are authored FQNs (`<layer>.<type>.<name>`) or, for implemented-by/details targets, code FQNs validated against the scanned graph (a planned code FQN is pending; a vanished one is drift and refused). Both endpoints must already exist.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    action: tool.schema.string().describe('"add" or "rm" (required).'),
    kind: tool.schema
      .string()
      .describe("Edge kind: contains, drives, realised-by, implemented-by, calls, publishes, subscribes, depends-on, uses, represents, details (required)."),
    from: tool.schema.string().describe("Source FQN (required)."),
    to: tool.schema.string().describe("Target FQN — an authored FQN or, for implemented-by/details, a code FQN (required)."),
    properties: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For add: `k=v` edge properties (repeatable), e.g. a coupling flavor on calls/publishes/subscribes."),
  },
  async execute(args, context) {
    const { action, kind, from, to } = args
    if (!action || !kind || !from || !to) return "Error: action, kind, from, and to are required"
    if (action !== "add" && action !== "rm") return 'Error: action must be "add" or "rm"'
    const cli = ["edge", action, kind, from, to]
    if (action === "add") {
      for (const p of args.properties ?? []) cli.push("--property", p)
    }
    return runCli(context, cli, args.directory)
  },
})
