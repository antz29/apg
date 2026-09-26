#pragma once

#include <tree_sitter/api.h>
#include <filesystem>
#include <string>
#include <unordered_map>
#include <unordered_set>

// ── Global scanner state: opaque-id registry + target-set gate ───────
//
// This header is the single home for the scanner's mutable globals. The
// `fs` alias is the one filesystem alias for the whole translation unit
// (source_discovery.h and scan.h use it too), and `parser` is the tree-sitter
// handle that `reset_scan_state` clears and `scan.h` drives.

namespace fs = std::filesystem;

static TSParser *parser = nullptr;

// ── Global id state (SPEC §3) ────────────────────────────────────────
//
// nextId is the monotonic opaque-id counter. idPrefix (`--id-prefix`, default
// "n") keeps ids unique across frontends when a scan merges multiple
// languages, so `n1` from C++ and `n1` from another frontend never collide.
// structID/funcID map canonical keys to ids: structs by FQN, functions by
// `parent.name(params)` (so overloads stay distinct). funcIDByFqn maps the
// plain FQN to the first id for that name, used to resolve heuristic call
// targets.
static int nextId = 0;
static std::string idPrefix = "n";
static std::string newNodeID() {
    return idPrefix + std::to_string(++nextId);
}
static std::unordered_map<std::string, std::string> structID;
static std::unordered_map<std::string, std::string> funcID;
static std::unordered_map<std::string, std::string> funcIDByFqn;

// unresolvedSeen deduplicates unresolved node records by fqn.
static std::unordered_set<std::string> unresolvedSeen;

// ── Target-set emission filter (phase-02 task-13, the pinned hand-off) ──
//
// `--targets <file>` carries a UTF-8, newline-delimited list of absolute
// source-file paths. An absent flag or an empty file means NO filter — the
// byte-identical full-scan stream. Otherwise only the target files' facts are
// emitted; the full parse still supplies the resolution context
// (global.constraint.frontend-full-context: resolve against the full context,
// filter only emission). Directory modules and namespace module records carry
// no location and are not part of any per-file fact unit, so — like the Go
// frontend's module hierarchy — they are emitted verbatim for the whole graph
// (the ingestor's cached-fact splice relies on the full module set being
// present). Only the per-file node/edge/unresolved facts are filtered.
static bool targetFilterActive = false;
static std::unordered_set<std::string> targetFiles;  // normalized absolute paths
static std::unordered_set<std::string> emittedIds;   // ids of target-file decls
// id -> the canonical FQN the ingestor will render for that node (only built
// under a target set). Used when a cross-file edge endpoint is not emitted.
static std::unordered_map<std::string, std::string> idFqn;

// Normalizes a path for target-set comparison. Lexical only: both the hand-off
// and the walked paths are absolute, so no filesystem resolution is needed.
static std::string normalized_path(const std::string &p) {
    return fs::path(p).lexically_normal().string();
}

// True when `path` is in the target set, or when no filter is in force.
static bool is_target_file(const std::string &path) {
    if (!targetFilterActive) return true;
    return targetFiles.count(normalized_path(path)) > 0;
}

// The endpoint to use for an edge: the opaque id when the target node is part
// of the emitted (target) set, else the node's canonical FQN. The ingestor
// resolves a bare FQN against the reused cached unit, so a cross-file edge
// authored by a target file survives the incremental splice. With no filter in
// force this is always the id — byte-identical to the full scan.
static std::string edge_endpoint(const std::string &fqn, const std::string &id) {
    if (!targetFilterActive || emittedIds.count(id)) return id;
    auto it = idFqn.find(id);
    return it != idFqn.end() ? it->second : fqn;
}

// ── Unified-schema emission ──────────────────────────────────────────

// Resolves a project FQN (function or struct) to its opaque id, or "" if the
// symbol was never declared. structs take priority (a class fqn cannot also be
// a function fqn).
static std::string node_id(const std::string &fqn) {
    auto it = structID.find(fqn);
    if (it != structID.end()) return it->second;
    auto jt = funcIDByFqn.find(fqn);
    if (jt != funcIDByFqn.end()) return jt->second;
    return "";
}

// Clears the scanner's global state so scan_root can be driven more than once
// in-process (the --self-test harness). `idPrefix` is a caller setting and is
// left untouched.
static void reset_scan_state() {
    nextId = 0;
    structID.clear();
    funcID.clear();
    funcIDByFqn.clear();
    unresolvedSeen.clear();
    targetFilterActive = false;
    targetFiles.clear();
    emittedIds.clear();
    idFqn.clear();
    parser = nullptr;
}
