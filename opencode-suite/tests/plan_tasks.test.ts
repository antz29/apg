// Unit tests for the plan-task listing Cypher and page verdict (`tasksQuery`
// and `tasksPageNotice` in `opencode-suite/tools/apg_plan_tasks.ts`). Pure
// string/row composition — no database, no subprocess — so plain `bun test`
// runs them.
import { test, expect } from "bun:test"

import { tasksQuery, tasksPageNotice } from "../tools/apg_plan_tasks.ts"

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

/// `n` data rows (the CSV header is already stripped before the verdict).
function dataRows(n: number): string[][] {
  return Array.from({ length: n }, (_, i) => [`proj/plan.phase-01.task-${i + 1}`])
}

test("an exactly-limit page is complete: no truncation notice", () => {
  const limit = 2
  const rows = dataRows(limit)

  const page = tasksPageNotice("proj", undefined, limit, rows)

  expect(page.notice).toBeUndefined()
  expect(page.rows).toHaveLength(limit)
})

test("limit+1 rows truncate to exactly limit and the notice names that limit", () => {
  const limit = 2
  const rows = dataRows(limit + 1)

  const page = tasksPageNotice("proj", undefined, limit, rows)

  expect(page.rows).toHaveLength(limit)
  expect(page.rows).toEqual(rows.slice(0, limit))
  expect(page.notice).toBe(`Truncated to the first ${limit} tasks; raise \`limit\` to see more.`)
})

test("an unfiltered empty plan keeps the `No tasks in plan` message", () => {
  const page = tasksPageNotice("proj", undefined, 500, [])

  expect(page.rows).toHaveLength(0)
  expect(page.notice).toBe("No tasks in plan `proj`.")
})

test("zero-match appears only for a status-filtered query, distinct from an empty plan", () => {
  const filtered = tasksPageNotice("proj", "done", 500, [])

  expect(filtered.rows).toHaveLength(0)
  expect(filtered.notice).toBe("No tasks match status=done")
  expect(filtered.notice).not.toBe("No tasks in plan `proj`.")
})
