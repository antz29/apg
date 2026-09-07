import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Add a node to a spec project's graph (`apg spec add <project> <kind> …`): requirement (id, title, body, feature, depends-on, anchor), future (kind + target FQN for planned code), phase (number, title, gates), decision (id, summary), non-goal / acceptance-criterion / verification (body), or note (body, kind, on). Also authors the tier-1/2/3 spec-family nodes (GraphModel-SPEC): stakeholder, domain (or bounded-context), subdomain (kind: core/supporting/generic), entity, value-object, aggregate (root), domain-event, domain-process, domain-rule, actor, system, container (kind: app/service/db/queue), component — each takes a name and optional body. `--parent` places the node in the DDD/C4 hierarchy (`Domain ⊃ Subdomain ⊃ Aggregate ⊃ Entity/ValueObject`, `System ⊃ Container ⊃ Component`); stakeholder/domain/system default under the spec root, while nested kinds (subdomain, entity, value-object, aggregate, domain-event, domain-process, domain-rule, actor, container, component) require `--parent` to their hierarchy parent (a Domain ⊃ Aggregate edge is rejected). Upsert by FQN — safe to re-run. Anchors accept only resolved code FQNs or existing Future-node/… FQNs (never auto-created).",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
    kind: tool.schema
      .string()
      .describe("Node kind: requirement, future, phase, decision, non-goal, acceptance-criterion, verification, note, stakeholder, domain, subdomain, entity, value-object, aggregate, domain-event, domain-process, domain-rule, actor, system, container, or component."),
    id: tool.schema.string().optional().describe("For requirement/decision: the id (e.g. R1)."),
    name: tool.schema.string().optional().describe("For future and tier-1/2/3 nodes: the node's short name."),
    number: tool.schema.string().optional().describe("For phase: the phase number."),
    title: tool.schema.string().optional().describe("For requirement/phase: a short title."),
    body: tool.schema.string().optional().describe("For requirement/non-goal/acceptance-criterion/verification/note and tier-1/2/3 nodes: the body text."),
    feature: tool.schema.string().optional().describe("For requirement: grouping feature (e.g. feature-a)."),
    dependsOn: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For requirement: requirement ids this one consumes (`R4` same-project, or `other-proj/R9` cross-project)."),
    anchor: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For requirement: code FQN or <project>/<name> FQN to anchor to."),
    kindOfFuture: tool.schema
      .string()
      .optional()
      .describe('For future: function, struct, container, component, system, service, rpc, endpoint, or other (required for future).'),
    target: tool.schema.string().optional().describe("For future: the intended real FQN once implemented."),
    gate: tool.schema.array(tool.schema.string()).optional().describe("For phase: phase numbers this phase is gated on."),
    summary: tool.schema.string().optional().describe("For decision: one-line summary."),
    kindOfTier: tool.schema
      .string()
      .optional()
      .describe("For subdomain (core/supporting/generic) or container (app/service/db/queue): the kind."),
    root: tool.schema.string().optional().describe("For aggregate: the aggregate-root entity name."),
    parent: tool.schema
      .string()
      .optional()
      .describe("For tier-1/2/3 nodes: the FQN of the containing node (default: the spec root), e.g. <project>/domain.Auth."),
    noteKind: tool.schema
      .string()
      .optional()
      .describe("For note: background, design, error-handling, open-question, decision, comment, or misc."),
    on: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For note: FQNs the note details (code or spec/future)."),
  },
  async execute(args, context) {
    const { project, kind } = args
    if (!project || !kind) return "Error: project and kind are required"
    const cli = ["spec", "add", project, kind]

    switch (kind) {
      case "requirement": {
        if (!args.id) return "Error: requirement requires id"
        cli.push(args.id)
        if (args.title) cli.push("--title", args.title)
        if (args.body) cli.push("--body", args.body)
        if (args.feature) cli.push("--feature", args.feature)
        for (const d of args.dependsOn ?? []) cli.push("--depends-on", d)
        for (const a of args.anchor ?? []) cli.push("--anchor", a)
        break
      }
      case "future": {
        if (!args.name) return "Error: future requires name"
        if (!args.kindOfFuture) return "Error: future requires kindOfFuture (function/struct/container/component/system/service/rpc/endpoint/other)"
        cli.push(args.name, "--kind", args.kindOfFuture)
        if (args.target) cli.push("--target", args.target)
        break
      }
      case "phase": {
        if (!args.number) return "Error: phase requires number"
        cli.push(args.number)
        if (args.title) cli.push("--title", args.title)
        for (const g of args.gate ?? []) cli.push("--gate", g)
        break
      }
      case "decision": {
        if (!args.id || !args.summary) return "Error: decision requires id and summary"
        cli.push(args.id, "--summary", args.summary)
        break
      }
      case "non-goal":
      case "acceptance-criterion":
      case "verification": {
        if (!args.body) return `Error: ${kind} requires body`
        cli.push("--body", args.body)
        break
      }
      case "note": {
        if (!args.body) return "Error: note requires body"
        cli.push("--body", args.body)
        if (args.noteKind) cli.push("--kind", args.noteKind)
        for (const o of args.on ?? []) cli.push("--on", o)
        break
      }
      case "stakeholder":
      case "domain":
      case "bounded-context":
      case "subdomain":
      case "entity":
      case "value-object":
      case "aggregate":
      case "domain-event":
      case "domain-process":
      case "domain-rule":
      case "actor":
      case "system":
      case "container":
      case "component": {
        if (!args.name) return `Error: ${kind} requires name`
        cli.push(args.name)
        if (args.body) cli.push("--body", args.body)
        if (args.kindOfTier && (kind === "subdomain" || kind === "container")) cli.push("--kind", args.kindOfTier)
        if (args.root && kind === "aggregate") cli.push("--root", args.root)
        if (args.parent) cli.push("--parent", args.parent)
        break
      }
      default:
        return `Error: unknown kind \`${kind}\`. Use requirement, future, phase, decision, non-goal, acceptance-criterion, verification, note, stakeholder, domain, subdomain, entity, value-object, aggregate, domain-event, domain-process, domain-rule, actor, system, container, or component.`
    }
    return runCli(context, cli)
  },
})