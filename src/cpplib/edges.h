#pragma once

#include <string>

// ── Edge emission + unresolved classification (extracted from main.cpp) ─

// Classify an unresolved symbol name. Qualified names are external symbols;
// bare identifiers are function pointers/locals (func-value) or, for types,
// unknown.
static std::string category_for(const std::string &name, bool is_type) {
    if (name.find('.') != std::string::npos) return "external";
    return is_type ? "unknown" : "func-value";
}

static void emit_edge(const std::string &type, const std::string &from, const std::string &to) {
    JsonBuilder jb;
    jb.field("type", type);
    jb.field("from", from);
    jb.field("to", to);
    emit_json(jb.done());
}

// Emits the unresolved node record for fqn on first encounter.
static void emit_unresolved(const std::string &fqn, const std::string &category) {
    if (fqn.empty() || unresolvedSeen.count(fqn)) return;
    unresolvedSeen.insert(fqn);
    JsonBuilder jb;
    jb.field("type", "unresolved");
    jb.field("fqn", fqn);
    jb.field("category", category);
    emit_json(jb.done());
}

// Resolved `uses` edge: source (function/struct) and target (struct) by id.
static void emit_use(const std::string &source_fqn, const std::string &target_fqn) {
    std::string sid = node_id(source_fqn);
    auto tit = structID.find(target_fqn);
    if (sid.empty() || tit == structID.end()) return;
    emit_edge("uses", sid, edge_endpoint(target_fqn, tit->second));
}

static void emit_u_use(const std::string &source_fqn, const std::string &target,
    const std::string &category)
{
    std::string sid = node_id(source_fqn);
    if (sid.empty() || target.empty()) return;
    emit_unresolved(target, category);
    emit_edge("unresolved_use", sid, target);
}

static void emit_u_call(const std::string &source_fqn, const std::string &target,
    const std::string &category)
{
    std::string sid = node_id(source_fqn);
    if (sid.empty() || target.empty()) return;
    emit_unresolved(target, category);
    emit_edge("unresolved_call", sid, target);
}

// Resolved `calls` edge: source (function) to target (function) by id. A
// target that never resolved to a declared function becomes an unresolved call
// so the dependency is not lost.
static void emit_call(const std::string &source_fqn, const std::string &target_fqn) {
    std::string sid = node_id(source_fqn);
    if (sid.empty()) return;
    auto it = funcIDByFqn.find(target_fqn);
    if (it != funcIDByFqn.end()) {
        emit_edge("calls", sid, edge_endpoint(target_fqn, it->second));
    } else {
        emit_u_call(source_fqn, target_fqn, category_for(target_fqn, false));
    }
}
