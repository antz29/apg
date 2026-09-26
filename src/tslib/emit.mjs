// apg unified JS/TS scanner frontend — unified-schema JSONL emission.
//
// Plain ESM sidecar (the `identity.mjs` precedent). Owns the single opaque-id
// counter (`nextId`), the emitted-edge dedup set (`seenEdges`) and the
// unresolved-record dedup set (`unresolvedSeen`) — each with exactly one owner,
// so ids stay deterministic and no edge/record is duplicated or spliced twice.

import { idToFqn, emittedID, config } from "./state.mjs";

const out = process.stdout;

// Emitted-edge dedup. The ingestor dedups edges, but avoid re-emitting identical
// edges from the broad syntax walk (a class referenced as a value, a type, and a
// new-target all resolve to the same symbol).
const seenEdges = new Set();

export function emitNode(type, fields) {
  out.write(JSON.stringify({ type, ...fields }) + "\n");
}

export function emitEdge(type, from, to) {
  // An edge to a declaration whose id is NOT part of the emitted stream carries
  // the target's canonical FQN instead of the opaque id, so a reference from an
  // emitted (target) file to a declaration in a non-emitted (cached) file
  // survives the ingestor's fact splice.
  const end = emittedID.has(to) ? to : idToFqn.get(to) || to;
  const k = type + "\0" + from + "\0" + end;
  if (seenEdges.has(k)) return;
  seenEdges.add(k);
  out.write(JSON.stringify({ type, from, to: end }) + "\n");
}

let nextId = 0;
export function newNodeID() {
  return config.idPrefix + ++nextId;
}

// unresolvedSeen dedups unresolved node records by fqn (first category wins).
const unresolvedSeen = new Set();
export function emitUnresolved(fqn, category) {
  if (!fqn || unresolvedSeen.has(fqn)) return;
  unresolvedSeen.add(fqn);
  emitNode("unresolved", { fqn, category });
}
