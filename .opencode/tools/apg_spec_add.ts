import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Add a node to a spec project (`apg spec add <project> <kind> …`): requirement (id, title, body, feature, depends-on, anchor), future (kind + target FQN for planned code), phase (number, title, gates), decision (id, summary), non-goal / acceptance-criterion / verification (body), or note (body, kind, on). Upsert by id — safe to re-run. Anchors accept only resolved code FQNs or existing future/… FQNs (never auto-created).",
  args: {
    project: tool.schema.string().describe("Spec project, e.g. workitem-timer (required)."),
    kind: tool.schema
      .string()
      .describe("Node kind: requirement, future, phase, decision, non-goal, acceptance-criterion, verification, or note."),
    id: tool.schema.string().optional().describe("For requirement/decision: the id (e.g. R1)."),
    name: tool.schema.string().optional().describe("For future: the future's short name."),
    number: tool.schema.string().optional().describe("For phase: the phase number."),
    title: tool.schema.string().optional().describe("For requirement/phase: a short title."),
    body: tool.schema.string().optional().describe("For requirement/non-goal/acceptance-criterion/verification/note: the body text."),
    feature: tool.schema.string().optional().describe("For requirement: grouping feature (e.g. feature-a)."),
    dependsOn: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For requirement: requirement ids this one consumes (`R4` same-project, or `other-proj/R9` cross-project)."),
    anchor: tool.schema
      .array(tool.schema.string())
      .optional()
      .describe("For requirement: code FQN or future/<project>/<name> FQN to anchor to."),
    kindOfFuture: tool.schema
      .string()
      .optional()
      .describe('For future: function, struct, service, rpc, endpoint, or other (required for future).'),
    target: tool.schema.string().optional().describe("For future: the intended real FQN once implemented."),
    gate: tool.schema.array(tool.schema.string()).optional().describe("For phase: phase numbers this phase is gated on."),
    summary: tool.schema.string().optional().describe("For decision: one-line summary."),
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
        if (!args.kindOfFuture) return "Error: future requires kindOfFuture (function/struct/service/rpc/endpoint/other)"
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
      default:
        return `Error: unknown kind \`${kind}\`. Use requirement, future, phase, decision, non-goal, acceptance-criterion, verification, or note.`
    }
    return runCli(context, cli)
  },
})