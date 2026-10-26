import { Plugin } from "@opencode/plugin"
import { PROJECT_WORKTREE_DIRECTORY_DESCRIPTION, runCli, type ToolContext } from "../lib/apg.ts"

const OPERATION_KINDS = ["module", "file", "struct", "function"]

/** The operation verb as its `--<kind>-<verb>` flag suffix. */
const OPERATION_VERB_FLAG: Record<string, string> = {
  creates: "create",
  modifies: "modify",
  deletes: "delete",
  renames: "rename",
  moves: "move",
}

/**
 * Append a task's per-kind operation edges to the CLI argument list.
 * Returns an error message for a malformed entry. On update, an empty
 * operation set is forwarded as a bare `--target` — the CLI's supplied-empty
 * marker, which replaces the stored edge set with a target-less activity.
 */
function appendOperations(
  cli: string[],
  input: { operations?: { kind: string; verb: string; target: string }[]; target?: string },
  updating: boolean,
): string | null {
  const operations = input.operations
  for (const op of operations ?? []) {
    const verb = OPERATION_VERB_FLAG[op.verb]
    if (!OPERATION_KINDS.includes(op.kind) || !verb || !op.target) {
      return "Error: each operation needs kind module|file|struct|function, verb creates|modifies|deletes|renames|moves, and a target FQN"
    }
    cli.push(`--${op.kind}-${verb}`, op.target)
  }
  if (input.target) cli.push("--target", input.target)
  else if (updating && operations !== undefined && operations.length === 0) cli.push("--target", "")
  return null
}

export default Plugin.define({
  id: "apg_plan_add",
  async setup(ctx) {
    await ctx.tool.transform((editor) => {
      editor.add({
        name: "apg_plan_add",
        options: { codemode: false },
        description:
          "Plan mutations (`apg plan <action> …`) — the strict add/update/rm surface (no implicit upsert). `action` defaults to \"add\". Plan record: `add <project> [--title T] [--strategy S]` creates the plan (refuses when it exists), `update <project> [--title T] [--strategy S]` MERGEs an existing plan, `rm <project> [--force]` removes it (refuses while it has phases unless `force`). Sub-entities via `kind`: `phase` (number), `task` (phaseNumber + taskNumber), `planned` (fqn). `add` authors a new entity (refuses an existing one); `update` edits in place and is edge-preserving (a phase update keeps its tasks, a task update keeps status + Reviews, a planned update repoints the parent Contains) — `update phase` also sets/replaces the phase's Satisfies and Gates sets via satisfies/prereq; `rm` deletes (refuses while dependents exist unless `force`). Phase: deliverable, prereq (phase numbers), satisfies (requirement NAMES from the layers store — `requirements.requirement.<name>`). Task: assignedTo (exactly one Agent FQN `agent.<file-stem>` for the assigned-to edge; it must be in the branch graph's Agent roster), implements (optional Component FQN from the solution layer for the implements edge; on update an empty string clears it), and operations — the per-kind operation edges, each `{kind, verb, target}` adding one `{kind}_{create|modify|delete|move|rename}` edge: `creates` targets a planned or still-absent FQN, `modifies`/`deletes` target live scanned code, and `renames`/`moves` name the live source FQN here with the destination in `target`. A task carries at most one edge per kind and verb; an empty operations array is a target-less activity, and `target` is only valid beside a renames/moves edge. On a task update, omitting assignedTo, implements or operations leaves that edge set untouched while a supplied one replaces it (an empty operations array clears the set). Planned node: plannedKind (module/file/struct/function), fqn (the real code FQN where the code will land), optional name + parent. Mutations run guarded inside a project worktree on the project branch.",
        input: {
          type: "object",
          properties: {
            directory: {
              type: "string",
              description: PROJECT_WORKTREE_DIRECTORY_DESCRIPTION,
            },
            action: { type: "string", description: '"add" (default), "update", or "rm".' },
            project: { type: "string", description: "Plan project (required)." },
            kind: {
              type: "string",
              description: 'Optional sub-entity: "phase", "task", or "planned". Omit it for the plan record itself.',
            },
            number: { type: "string", description: "For phase: the phase number." },
            phaseNumber: { type: "string", description: "For task: the phase number the task belongs to." },
            taskNumber: { type: "string", description: "For task: the task number within the phase." },
            title: { type: "string", description: "Phase or task title (required for add)." },
            strategy: {
              type: "string",
              description: "For the plan record (add/update): strategy text (variants, tier routing, gates, execution method).",
            },
            deliverable: { type: "string", description: "For phase: one-line deliverable." },
            prereq: {
              type: "array",
              items: { type: "string" },
              description:
                "For phase: phase numbers this phase is gated on (prereqs). On update, passed values replace the phase's Gates set.",
            },
            satisfies: {
              type: "array",
              items: { type: "string" },
              description:
                "For phase: requirement names this phase delivers (Satisfies → requirements.requirement.<name>). On update, passed values replace the phase's Satisfies set.",
            },
            assignedTo: {
              type: "string",
              description:
                "For task: the Agent FQN (`agent.<file-stem>`) the assigned-to edge names — exactly one, and it must be in the branch graph's Agent roster (an absent Agent is drift).",
            },
            implements: {
              type: "string",
              description:
                "For task: the optional Component FQN (solution layer) the implements edge names. On update, an empty string clears the edge.",
            },
            operations: {
              type: "array",
              description:
                "For task: the per-kind operation edges. Each entry adds one `{kind}_{create|modify|delete|move|rename}` edge, at most one per kind and verb. Omit on update to leave the stored set untouched; an empty array replaces it with a target-less activity.",
              items: {
                type: "object",
                properties: {
                  kind: { type: "string", description: '"module", "file", "struct", or "function".' },
                  verb: {
                    type: "string",
                    description: '"creates", "modifies", "deletes", "renames", or "moves".',
                  },
                  target: {
                    type: "string",
                    description:
                      "The FQN the verb applies to: planned or still-absent for creates, live scanned code for modifies/deletes, the live source FQN for renames/moves.",
                  },
                },
                required: ["kind", "verb", "target"],
                additionalProperties: false,
              },
            },
            target: {
              type: "string",
              description: "For task renames/moves operations: the destination FQN.",
            },
            fqn: {
              type: "string",
              description:
                "For planned node: the real code FQN where the code will land (required for add/update/rm).",
            },
            plannedKind: {
              type: "string",
              description: "For planned node: module, file, struct, or function (required for add).",
            },
            name: { type: "string", description: "For planned node: the simple name." },
            parent: { type: "string", description: "For planned node: the containing node FQN." },
            force: {
              type: "boolean",
              description:
                "For rm: cascade removal of the entity and its dependents (plan-with-phases, phase-with-tasks, done/feedback task, creates-targeted planned).",
            },
          },
          required: ["project"],
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
          const { project, kind } = input
          if (!project) return { content: "Error: project is required" }
          const action = input.action ?? "add"
          if (action !== "add" && action !== "update" && action !== "rm") {
            return { content: 'Error: action must be "add", "update", or "rm"' }
          }
          if (kind && kind !== "phase" && kind !== "task" && kind !== "planned") {
            return { content: 'Error: kind must be "phase", "task", or "planned"' }
          }

          const cli = ["plan", action, project]

          if (action === "rm") {
            if (kind === "phase") {
              if (!input.number) return { content: "Error: rm phase requires number" }
              cli.push("phase", input.number)
            } else if (kind === "task") {
              if (!input.phaseNumber || !input.taskNumber)
                return { content: "Error: rm task requires phaseNumber and taskNumber" }
              cli.push("task", input.phaseNumber, input.taskNumber)
            } else if (kind === "planned") {
              if (!input.fqn) return { content: "Error: rm planned requires fqn" }
              cli.push("planned", input.fqn)
            }
            if (input.force) cli.push("--force")
            return { content: await runCli(merged, cli, input.directory) }
          }

          if (action === "update") {
            if (kind === "phase") {
              if (!input.number) return { content: "Error: update phase requires number" }
              cli.push("phase", input.number)
              if (input.title) cli.push("--title", input.title)
              if (input.deliverable) cli.push("--deliverable", input.deliverable)
              for (const p of input.prereq ?? []) cli.push("--prereq", p)
              for (const s of input.satisfies ?? []) cli.push("--satisfies", s)
            } else if (kind === "task") {
              if (!input.phaseNumber || !input.taskNumber)
                return { content: "Error: update task requires phaseNumber and taskNumber" }
              cli.push("task", input.phaseNumber, input.taskNumber)
              if (input.title) cli.push("--title", input.title)
              if (input.assignedTo) cli.push("--assigned-to", input.assignedTo)
              if (input.implements !== undefined) cli.push("--implements", input.implements)
              const operationError = appendOperations(cli, input, true)
              if (operationError) return { content: operationError }
            } else if (kind === "planned") {
              if (!input.fqn) return { content: "Error: update planned requires fqn" }
              cli.push("planned", input.fqn)
              if (input.plannedKind) cli.push("--kind", input.plannedKind)
              if (input.name) cli.push("--name", input.name)
              if (input.parent) cli.push("--parent", input.parent)
            } else {
              if (input.title) cli.push("--title", input.title)
              if (input.strategy) cli.push("--strategy", input.strategy)
            }
            return { content: await runCli(merged, cli, input.directory) }
          }

          // action === "add"
          if (kind === "phase") {
            if (!input.number) return { content: "Error: phase requires number" }
            if (!input.title) return { content: "Error: phase requires title" }
            cli.push("phase", input.number, "--title", input.title)
            if (input.deliverable) cli.push("--deliverable", input.deliverable)
            for (const p of input.prereq ?? []) cli.push("--prereq", p)
            for (const s of input.satisfies ?? []) cli.push("--satisfies", s)
          } else if (kind === "task") {
            if (!input.phaseNumber || !input.taskNumber)
              return { content: "Error: task requires phaseNumber and taskNumber" }
            if (!input.title) return { content: "Error: task requires title" }
            if (!input.assignedTo) return { content: "Error: task requires assignedTo" }
            cli.push("task", input.phaseNumber, input.taskNumber, "--title", input.title)
            if (input.implements) cli.push("--implements", input.implements)
            const operationError = appendOperations(cli, input, false)
            if (operationError) return { content: operationError }
          } else if (kind === "planned") {
            if (!input.plannedKind || !input.fqn)
              return { content: "Error: planned node requires plannedKind and fqn" }
            cli.push("planned", input.plannedKind, input.fqn)
            if (input.name) cli.push("--name", input.name)
            if (input.parent) cli.push("--parent", input.parent)
          } else {
            if (input.title) cli.push("--title", input.title)
            if (input.strategy) cli.push("--strategy", input.strategy)
          }
          return { content: await runCli(merged, cli, input.directory) }
        },
      })
    })
  },
})
