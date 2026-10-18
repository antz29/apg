import { Plugin } from "@opencode/plugin"
import { runCli, type ToolContext } from "../lib/apg.ts"

export default Plugin.define({
  id: "apg_node",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_node",
        options: { codemode: false },
        description:
          "Durable node-file mutations (`apg node add|update|rm <layer> <type> <name> …`). One file per node under apg/layers/<layer>/<type>/<name>.json; the file name IS the identity and the FQN is `<layer>.<type>.<name>` (no project prefix). Layers/types: requirements (stakeholder/user/requirement/constraint), domain (group/actor/entity/value/service/constraint), solution (system/container/component/person/role/constraint), implementation (note/constraint — attach-only to code), global (constraint). `--body` carries the prose; `--property k=v` sets metadata (an entity requires kind=entity|event, a container takes kind=app|service|db|queue, a group takes attribute=core|supporting|generic and an optional root). The name is identity and is never updatable: `update` takes `--body`/`--property` only and MERGEs them (omitting `--unset-property` never drops a key; unsetting a key the node does not carry is refused, naming the present keys), preserving every incident in/out edge; `add` refuses an existing FQN (no implicit upsert — use `update` or `rm`); `rm` removes the node and strips its incident edges. Names are `[a-z0-9][a-z0-9-]*` (refused, never sanitized). Mutations are guarded — they only run inside a project worktree on the project branch. A durable `apg node` mutation requires a live session (`apg session start`): it is projected at admission into the session's write-back buffer and is not durable until `apg session save`, which makes one atomic `apg/layers/**` write and exactly one commit; with no live session it refuses.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: "Project root directory (a worktree path to operate on). Defaults to the workspace root.",
            },
            action: { type: "string", description: '"add", "update", or "rm" (required).' },
            layer: {
              type: "string",
              description: "Layer dir: requirements, domain, solution, implementation, or global (required).",
            },
            type: {
              type: "string",
              description:
                "Node type within the layer, e.g. requirement, group, actor, entity, value, service, system, container, component, person, role, note, constraint (required).",
            },
            name: {
              type: "string",
              description:
                "Node name — the identity and the FQN's last segment, `[a-z0-9][a-z0-9-]*` (required). Immutable: never updatable.",
            },
            body: { type: "string", description: "For add/update: the node's prose body." },
            properties: {
              type: "array",
              items: { type: "string" },
              description:
                "For add/update: `k=v` metadata pairs (repeatable), e.g. kind=event, attribute=core. On update these MERGE (only the passed keys change).",
            },
            unsetProperties: {
              type: "array",
              items: { type: "string" },
              description: "For update: property keys to delete explicitly (repeatable). Omitting this never drops a key.",
            },
          },
          required: ["action", "layer", "type", "name"],
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
          const { action, layer, type, name } = input
          if (!action || !layer || !type || !name) {
            return { content: "Error: action, layer, type, and name are required" }
          }
          if (action !== "add" && action !== "update" && action !== "rm") {
            return { content: 'Error: action must be "add", "update", or "rm"' }
          }
          const cli = ["node", action, layer, type, name]
          if (action === "add" || action === "update") {
            if (input.body) cli.push("--body", input.body)
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
