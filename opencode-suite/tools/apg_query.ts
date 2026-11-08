import { Plugin } from "@opencode/plugin"
import { runCypher, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_query",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_query",
        options: { codemode: false },
        description:
          "Execute a read-only Cypher query on the project's LadybugDB program graph (apg/.trans/db.lbug). CSV output, header row included. Graph schema: Module ->(Contains)-> File ->(Contains)-> Struct/Function; Struct ->(Contains)-> Struct/Function for nested types and methods; Calls/Uses/Unresolved* edges link functions and types. Struct/Function/File nodes carry start_line/end_line (1-based) plus path — use them to join against diffs/hunks. MATCH/RETURN only; read-only, no modifications. Dialect: backtick a parser-reserved word (n.`end`, or `group` when it names a property/label); ordinary keywords (`limit`, `match`, `return`, `set`, `count`, `skip`) need none. Grouping is implicit — no `GROUP BY` (a parse error); `collect()` over zero rows is `[]`. No `JOIN` keyword (join via shared variables), no free-standing subquery, no `CALL {}`; `EXISTS { … }`/`COUNT { … }` pattern subqueries and read-only `CALL` table functions (`show_tables`, `table_info`, `show_functions`, `db_version`, `current_setting`) work. No `OFFSET` (use `SKIP`), no `NULLS FIRST/LAST`; `ORDER BY` takes an alias or expression, never a position (`ORDER BY 2` sorts the constant), and after `WITH` needs `SKIP`/`LIMIT`. List indexing is 1-based; slices are inclusive at both ends (`[a:b]` or `[a..b]`); `list_has_any`/`list_min`/`list_max` exist. `!=` is a parse error (use `<>`); `PROFILE`/`EXPLAIN` work (PROFILE executes).",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            query: {
              type: "string",
              description:
                "Cypher query, e.g. MATCH (f:File {fqn:'/abs/Graph.java'})-[:Contains]->(n) RETURN labels(n), n.fqn, n.start_line, n.end_line",
            },
          },
          required: ["query"],
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
          return { content: await runCypher(merged, input.query, input.directory) }
        },
      })
    })
  },
})
