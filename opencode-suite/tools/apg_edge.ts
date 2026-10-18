import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_edge",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_edge",
        options: { codemode: false },
        description:
          "Durable node-file edge mutations (`apg edge add|update|rm <kind> <from> <to> [--property k=v]*`). A durable `apg edge` mutation requires a live session (`apg session start`): it is projected at admission into the session's write-back buffer and is not durable until `apg session save`, which writes BOTH endpoint files in one atomic `apg/layers/**` write and exactly one commit — the out half in the source's file, the matching in half in the target's; with no live session it refuses. Kinds (SPEC §4): requires, contains, drives, reads, writes, calls, publishes, subscribes, realises, is-role, uses, represents, implemented-by, details. Endpoints are authored FQNs (`<layer>.<type>.<name>`) or, for implemented-by/details targets, code FQNs validated against the scanned graph (a planned code FQN is pending; a vanished one is drift and refused). Both endpoints must already exist. `add` refuses an identical `(kind,from,to)` already on the source (no implicit upsert); `update` is properties-only (kind/from/to are identity: it MERGEs `--property` and applies `--unset-property` (an absent key is refused), rewriting both halves to the same map) and refuses an absent edge; `rm` removes the edge from both endpoint files.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            action: { type: "string", description: '"add", "update", or "rm" (required).' },
            kind: {
              type: "string",
              description:
                "Edge kind: requires, contains, drives, reads, writes, calls, publishes, subscribes, realises, is-role, uses, represents, implemented-by, details (required).",
            },
            from: { type: "string", description: "Source FQN (required)." },
            to: {
              type: "string",
              description: "Target FQN — an authored FQN or, for implemented-by/details, a code FQN (required).",
            },
            properties: {
              type: "array",
              items: { type: "string" },
              description:
                "For add/update: `k=v` edge properties (repeatable), e.g. a coupling flavor on calls/publishes/subscribes. On update these MERGE (only the passed keys change on both halves).",
            },
            unsetProperties: {
              type: "array",
              items: { type: "string" },
              description:
                "For update: property keys to delete explicitly (repeatable). Omitting this never drops a key.",
            },
          },
          required: ["action", "kind", "from", "to"],
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
          const { action, kind, from, to } = input
          if (!action || !kind || !from || !to) return { content: "Error: action, kind, from, and to are required" }
          if (action !== "add" && action !== "update" && action !== "rm") {
            return { content: 'Error: action must be "add", "update", or "rm"' }
          }
          const cli = ["edge", action, kind, from, to]
          if (action === "add" || action === "update") {
            for (const p of input.properties ?? []) cli.push("--property", p)
          }
          if (action === "update") {
            for (const u of input.unsetProperties ?? []) cli.push("--unset-property", u)
          }
          return { content: await runCli(merged, cli, input.directory) }
        },
      })
    })
  },
})
