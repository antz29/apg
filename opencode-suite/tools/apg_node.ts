import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Durable node-file mutations (`apg node add|rm <layer> <type> <name> …`). One file per node under apg/layers/<layer>/<type>/<name>.json; the file name IS the identity and the FQN is `<layer>.<type>.<name>` (no project prefix). Layers/types: requirements (stakeholder/user/requirement/note/constraint), domain (group/entity/value/service/note/constraint), solution (system/container/component/person/note/constraint), implementation (note/constraint — attach-only to code), global (constraint/note). `--body` carries the prose; `--property k=v` sets metadata: an entity requires kind=entity|event, a container takes kind=app|service|db|queue, a group takes attribute=core|supporting|generic and an optional root, and a local constraint attaches with property attaches-to=<fqn>. Names are `[a-z0-9][a-z0-9-]*` (refused, never sanitized). Mutations are guarded — they only run inside a project worktree on the project branch — and auto-commit the affected files in one commit.",
  args: {
    action: tool.schema.string().describe('"add" or "rm" (required).'),
    layer: tool.schema
      .string()
      .describe("Layer dir: requirements, domain, solution, implementation, or global (required)."),
    type: tool.schema
      .string()
      .describe("Node type within the layer, e.g. requirement, group, entity, value, service, system, container, component, person, note, constraint (required)."),
    name: tool.schema
      .string()
      .describe("Node name — the identity and the FQN's last segment, `[a-z0-9][a-z0-9-]*` (required)."),
    body: tool.schema.string().optional().describe("For add: the node's prose body."),
    properties: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For add: `k=v` metadata pairs (repeatable), e.g. kind=event, attribute=core, attaches-to=<fqn>."),
  },
  async execute(args, context) {
    const { action, layer, type, name } = args
    if (!action || !layer || !type || !name) {
      return "Error: action, layer, type, and name are required"
    }
    if (action !== "add" && action !== "rm") return 'Error: action must be "add" or "rm"'
    const cli = ["node", action, layer, type, name]
    if (action === "add") {
      if (args.body) cli.push("--body", args.body)
      for (const p of args.properties ?? []) cli.push("--property", p)
    }
    return runCli(context, cli)
  },
})
