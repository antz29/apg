import { tool } from "@opencode-ai/plugin"
import { runCli } from "../lib/apg.ts"

export default tool({
  description:
    "Plan mutations (`apg plan <action> …`) — the strict add/update/rm surface (no implicit upsert). `action` defaults to \"add\". Plan record: `add <project> [--title T] [--strategy S]` creates the plan (refuses when it exists), `update <project> [--title T] [--strategy S]` MERGEs an existing plan, `rm <project> [--force]` removes it (refuses while it has phases unless `force`). Sub-entities via `kind`: `phase` (number), `task` (phaseNumber + taskNumber), `planned` (fqn). `add` authors a new entity (refuses an existing one); `update` edits in place and is edge-preserving (a phase update keeps its tasks, a task update keeps status + Reviews, a planned update repoints the parent Contains) — `update phase` also sets/replaces the phase's Satisfies and Gates sets via satisfies/prereq; `rm` deletes (refuses while dependents exist unless `force`). Phase: deliverable, prereq (phase numbers), satisfies (requirement NAMES from the layers store — `requirements.requirement.<name>`). Task: taskKind (source/test/gate/docs — owning role), tier (unit/int/e2e — required for test tasks only), verb (creates/modifies/deletes/renames/moves — the Task→Implementation verb), fqn (the Implementation FQN the verb applies to; for renames/moves the source FQN), to (the new FQN for renames/moves). Planned node: plannedKind (module/file/struct/function), fqn (the real code FQN where the code will land), optional name + parent. Mutations run guarded inside a project worktree on the project branch.",
  args: {
    directory: tool.schema
      .string()
      .optional()
      .describe("Project root directory (a worktree path to operate on). Defaults to the workspace root."),
    action: tool.schema
      .string()
      .optional()
      .describe('"add" (default), "update", or "rm".'),
    project: tool.schema.string().describe("Plan project (required)."),
    kind: tool.schema
      .string()
      .optional()
      .describe('Optional sub-entity: "phase", "task", or "planned". Omit it for the plan record itself.'),
    number: tool.schema.string().optional().describe("For phase: the phase number."),
    phaseNumber: tool.schema.string().optional().describe("For task: the phase number the task belongs to."),
    taskNumber: tool.schema.string().optional().describe("For task: the task number within the phase."),
    title: tool.schema.string().optional().describe("Phase or task title (required for add)."),
    strategy: tool.schema.string().optional().describe("For the plan record (add/update): strategy text (variants, tier routing, gates, execution method)."),
    deliverable: tool.schema.string().optional().describe("For phase: one-line deliverable."),
    prereq: tool.schema.array(tool.schema.string()).optional().describe("For phase: phase numbers this phase is gated on (prereqs). On update, passed values replace the phase's Gates set."),
    satisfies: tool.schema.array(tool.schema.string()).optional().describe("For phase: requirement names this phase delivers (Satisfies → requirements.requirement.<name>). On update, passed values replace the phase's Satisfies set."),
    taskKind: tool.schema
      .string()
      .optional()
      .describe("For task: the owning role — source (default), test, gate, or docs."),
    tier: tool.schema
      .string()
      .optional()
      .describe("For task: verification depth — unit, int, or e2e. Only valid with taskKind=test (required then)."),
    verb: tool.schema
      .string()
      .optional()
      .describe("For task: the Task→Implementation verb — creates, modifies, deletes, renames, or moves (default creates)."),
    fqn: tool.schema
      .string()
      .optional()
      .describe("For task: the Implementation FQN the verb applies to (for renames/moves, the source FQN). For planned node: the real code FQN where the code will land (required)."),
    to: tool.schema.string().optional().describe("For task renames/moves: the destination FQN."),
    plannedKind: tool.schema
      .string()
      .optional()
      .describe("For planned node: module, file, struct, or function (required for add)."),
    name: tool.schema.string().optional().describe("For planned node: the simple name."),
    parent: tool.schema.string().optional().describe("For planned node: the containing node FQN."),
    force: tool.schema
      .boolean()
      .optional()
      .describe("For rm: cascade removal of the entity and its dependents (plan-with-phases, phase-with-tasks, done/feedback task, creates-targeted planned)."),
  },
  async execute(args, context) {
    const { project, kind } = args
    if (!project) return "Error: project is required"
    const action = args.action ?? "add"
    if (action !== "add" && action !== "update" && action !== "rm") {
      return 'Error: action must be "add", "update", or "rm"'
    }
    if (kind && kind !== "phase" && kind !== "task" && kind !== "planned") {
      return 'Error: kind must be "phase", "task", or "planned"'
    }

    const cli = ["plan", action, project]

    if (action === "rm") {
      if (kind === "phase") {
        if (!args.number) return "Error: rm phase requires number"
        cli.push("phase", args.number)
      } else if (kind === "task") {
        if (!args.phaseNumber || !args.taskNumber) return "Error: rm task requires phaseNumber and taskNumber"
        cli.push("task", args.phaseNumber, args.taskNumber)
      } else if (kind === "planned") {
        if (!args.fqn) return "Error: rm planned requires fqn"
        cli.push("planned", args.fqn)
      }
      if (args.force) cli.push("--force")
      return runCli(context, cli, args.directory)
    }

    if (action === "update") {
      if (kind === "phase") {
        if (!args.number) return "Error: update phase requires number"
        cli.push("phase", args.number)
        if (args.title) cli.push("--title", args.title)
        if (args.deliverable) cli.push("--deliverable", args.deliverable)
        for (const p of args.prereq ?? []) cli.push("--prereq", p)
        for (const s of args.satisfies ?? []) cli.push("--satisfies", s)
      } else if (kind === "task") {
        if (!args.phaseNumber || !args.taskNumber) return "Error: update task requires phaseNumber and taskNumber"
        cli.push("task", args.phaseNumber, args.taskNumber)
        if (args.title) cli.push("--title", args.title)
        if (args.taskKind) cli.push("--kind", args.taskKind)
        if (args.tier) cli.push("--tier", args.tier)
        if (args.verb) cli.push("--verb", args.verb)
        if (args.fqn) cli.push("--fqn", args.fqn)
        if (args.to) cli.push("--to", args.to)
      } else if (kind === "planned") {
        if (!args.fqn) return "Error: update planned requires fqn"
        cli.push("planned", args.fqn)
        if (args.plannedKind) cli.push("--kind", args.plannedKind)
        if (args.name) cli.push("--name", args.name)
        if (args.parent) cli.push("--parent", args.parent)
      } else {
        if (args.title) cli.push("--title", args.title)
        if (args.strategy) cli.push("--strategy", args.strategy)
      }
      return runCli(context, cli, args.directory)
    }

    // action === "add"
    if (kind === "phase") {
      if (!args.number) return "Error: phase requires number"
      if (!args.title) return "Error: phase requires title"
      cli.push("phase", args.number, "--title", args.title)
      if (args.deliverable) cli.push("--deliverable", args.deliverable)
      for (const p of args.prereq ?? []) cli.push("--prereq", p)
      for (const s of args.satisfies ?? []) cli.push("--satisfies", s)
    } else if (kind === "task") {
      if (!args.phaseNumber || !args.taskNumber) return "Error: task requires phaseNumber and taskNumber"
      if (!args.title) return "Error: task requires title"
      cli.push("task", args.phaseNumber, args.taskNumber, "--title", args.title)
      if (args.taskKind) cli.push("--kind", args.taskKind)
      if (args.tier) cli.push("--tier", args.tier)
      if (args.verb) cli.push("--verb", args.verb)
      if (args.fqn) cli.push("--fqn", args.fqn)
      if (args.to) cli.push("--to", args.to)
    } else if (kind === "planned") {
      if (!args.plannedKind || !args.fqn) return "Error: planned node requires plannedKind and fqn"
      cli.push("planned", args.plannedKind, args.fqn)
      if (args.name) cli.push("--name", args.name)
      if (args.parent) cli.push("--parent", args.parent)
    } else {
      if (args.title) cli.push("--title", args.title)
      if (args.strategy) cli.push("--strategy", args.strategy)
    }
    return runCli(context, cli, args.directory)
  },
})
