// Unit tests for the plan-task listing Cypher (`tasksQuery` in
// `opencode-suite/tools/apg_plan_tasks.ts`). Pure string composition — no
// database, no subprocess — so plain `bun test` runs them.
import { test, expect } from "bun:test"

import { tasksQuery } from "../tools/apg_plan_tasks.ts"

/// The single WHERE clause of the composed query, from ` WHERE ` to the
/// `OPTIONAL MATCH` projection that follows it.
function whereClause(query: string): string {
  const start = query.indexOf("WHERE ")
  const end = query.indexOf("OPTIONAL MATCH", start)
  expect(start).toBeGreaterThan(-1)
  expect(end).toBeGreaterThan(start)
  return query.slice(start + "WHERE ".length, end).trim()
}

test("a supplied status joins the phase-prefix predicate in the one WHERE ahead of ORDER BY/LIMIT", () => {
  const query = tasksQuery("proj", "done", 25)

  expect(whereClause(query)).toBe("pp.fqn STARTS WITH 'proj/plan.' AND t.status = 'done'")
  // One WHERE only: the status predicate is not a post-query filter.
  expect(query.split("WHERE ")).toHaveLength(2)
  expect(query.indexOf("t.status = 'done'")).toBeLessThan(query.indexOf("ORDER BY t.fqn LIMIT 25"))
  expect(query.endsWith("ORDER BY t.fqn LIMIT 25")).toBe(true)
})

test("an unfiltered query omits the status predicate entirely", () => {
  const query = tasksQuery("proj", undefined, 500)

  expect(query.includes("t.status =")).toBe(false)
  expect(whereClause(query)).toBe("pp.fqn STARTS WITH 'proj/plan.'")
  expect(query.split("WHERE ")).toHaveLength(2)
  expect(query.endsWith("ORDER BY t.fqn LIMIT 500")).toBe(true)
})
