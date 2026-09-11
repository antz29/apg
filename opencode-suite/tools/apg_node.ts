import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Durable node-file mutations (`apg node add|update|rm <layer> <type> <name> …`). One file per node under apg/layers/<layer>/<type>/<name>.json; the file name IS the identity and the FQN is `<layer>.<type>.<name>` (no project prefix). Layers/types: requirements (stakeholder/user/requirement/note/constraint), domain (group/entity/value/service/note/constraint), solution (system/container/component/person/note/constraint), implementation (note/constraint — attach-only to code), global (constraint/note). `--body` carries the prose; `--property k=v` sets metadata (an entity requires kind=entity|event, a container takes kind=app|service|db|queue, a group takes attribute=core|supporting|generic and an optional root; a local constraint attaches with attaches-to=<fqn>). The name is identity and is never updatable: `update` takes `--body`/`--property` only and MERGEs them (omitting `--unset-property` never drops a key), preserving every incident in/out edge; `add` refuses an existing FQN (no implicit upsert — use `update` or `rm`); `rm` removes the node and strips its incident edges. Names are `[a-z0-9][a-z0-9-]*` (refused, never sanitized). Mutations are guarded — they only run inside a project worktree on the project branch — and auto-commit the affected files in one commit.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    action: tool.schema.string().describe('"add", "update", or "rm" (required).'),
    layer: tool.schema
      .string()
      .describe("Layer dir: requirements, domain, solution, implementation, or global (required)."),
    type: tool.schema
      .string()
      .describe("Node type within the layer, e.g. requirement, group, entity, value, service, system, container, component, person, note, constraint (required)."),
    name: tool.schema
      .string()
      .describe("Node name — the identity and the FQN's last segment, `[a-z0-9][a-z0-9-]*` (required). Immutable: never updatable."),
    body: tool.schema.string().optional().describe("For add/update: the node's prose body."),
    properties: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For add/update: `k=v` metadata pairs (repeatable), e.g. kind=event, attribute=core, attaches-to=<fqn>. On update these MERGE (only the passed keys change)."),
    unsetProperties: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For update: property keys to delete explicitly (repeatable). Omitting this never drops a key."),
  },
  async execute(args, context) {
    const { action, layer, type, name } = args
    if (!action || !layer || !type || !name) {
      return "Error: action, layer, type, and name are required"
    }
    if (action !== "add" && action !== "update" && action !== "rm") {
      return 'Error: action must be "add", "update", or "rm"'
    }
    const cli = ["node", action, layer, type, name]
    if (action === "add" || action === "update") {
      if (args.body) cli.push("--body", args.body)
      for (const p of args.properties ?? []) cli.push("--property", p)
    }
    if (action === "update") {
      for (const u of args.unsetProperties ?? []) cli.push("--unset-property", u)
    }
    return runCli(context, cli, args.directory)
  },
})
