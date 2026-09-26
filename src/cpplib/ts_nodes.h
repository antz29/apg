#pragma once

#include <tree_sitter/api.h>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

// ── Generic tree-sitter node utilities (extracted from main.cpp) ─────

static std::string node_text(TSNode node, const std::string &source) {
    uint32_t start = ts_node_start_byte(node);
    uint32_t end = ts_node_end_byte(node);
    return source.substr(start, end - start);
}

// 1-based line number containing the byte at `off` (0-based). `off` may point
// at the byte just past the end of a span; callers pass the last byte instead.
static uint32_t line_of(const std::string &src, uint32_t off) {
    if (src.empty()) return 1;
    if (off >= src.size()) off = (uint32_t)src.size() - 1;
    uint32_t n = 1;
    for (uint32_t i = 0; i < off; i++) {
        if (src[i] == '\n') n++;
    }
    return n;
}

// Line number of the last byte of an exclusive-end span (end == 0 => 1).
static uint32_t line_end_of(const std::string &src, uint32_t end) {
    return end == 0 ? 1 : line_of(src, end - 1);
}

static std::string attr(TSNode node, const std::string &name, const std::string &source) {
    TSNode child = ts_node_child_by_field_name(node, name.c_str(), name.size());
    if (ts_node_is_null(child)) return "";
    return node_text(child, source);
}

static bool has_field(TSNode node, const std::string &name) {
    TSNode child = ts_node_child_by_field_name(node, name.c_str(), name.size());
    return !ts_node_is_null(child);
}

static void collect_nested(TSNode node, const std::string &source, std::vector<std::string> &out) {
    const char *kind = ts_node_type(node);
    if (strcmp(kind, "identifier") == 0 || strcmp(kind, "namespace_identifier") == 0 || strcmp(kind, "type_identifier") == 0) {
        out.push_back(node_text(node, source));
    } else if (strcmp(kind, "template_type") == 0) {
        TSNode base = ts_node_child_by_field_name(node, "name", 4);
        if (!ts_node_is_null(base)) collect_nested(base, source, out);
    } else if (strcmp(kind, "nested_identifier") == 0 || strcmp(kind, "qualified_identifier") == 0) {
        TSNode s = ts_node_child_by_field_name(node, "scope", 5);
        if (!ts_node_is_null(s)) collect_nested(s, source, out);
        TSNode n = ts_node_child_by_field_name(node, "name", 4);
        if (!ts_node_is_null(n)) collect_nested(n, source, out);
    }
}
