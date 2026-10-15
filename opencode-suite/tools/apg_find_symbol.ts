import { Plugin } from "@opencode/plugin"
import { runCypher, lit, codeTypeCondition, findSymbolRebaseColumns, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_find_symbol",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_find_symbol",
        description:
          "Search the project's code graph for symbols whose FQN contains a string. Returns matching Modules, Files, Structs, and Functions with their locations. Use to look up an exact FQN when you only know part of a name.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            name: { type: "string", description: "Substring to match against node FQNs (required)" },
            kind: {
              type: "string",
              description: "Restrict to a node kind: Module, File, Struct, Function, UnresolvedTarget",
            },
            codeType: {
              type: "string",
              description: 'Code type filter: "src", "test", "generated", "external". Defaults to "all".',
            },
            limit: { type: "string", description: "Max results (default 50, max 500)" },
          },
          required: ["name"],
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
          const name = input.name
          if (!name) return { content: "Error: name is required" }
          const kind = input.kind || ""
          const limit = input.limit ? Math.max(1, Math.min(500, Number(input.limit))) : 50

          let cypher = `MATCH (n) WHERE n.fqn CONTAINS ${lit(name)}`
          const conds: string[] = []
          if (kind) {
            conds.push(`labels(n) = ${lit(kind)}`)
          }
          const ctCond = codeTypeCondition("n", input.codeType)
          if (ctCond) conds.push(ctCond)
          if (conds.length) {
            cypher += ` AND ${conds.join(" AND ")}`
          }
          cypher += ` RETURN labels(n) as kind, n.fqn, n.path, n.start_line, n.end_line ORDER BY n.fqn LIMIT ${limit}`
          // The stored repo-relative identity sits in a kind-dependent cell: a File's
          // identity is its `n.fqn` (column 1; File nodes have no `path`), while a
          // Struct/Function's identity is its `n.path` (column 2) and its fqn is a
          // symbol name. `findSymbolRebaseColumns` rebases the right cell per row —
          // a blind column rebase would turn a symbol fqn into a bogus path.
          return {
            content: await runCypher(merged, cypher, input.directory, { rebaseColumns: findSymbolRebaseColumns }),
          }
        },
      })
    })
  },
})
