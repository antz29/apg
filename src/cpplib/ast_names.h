#pragma once

#include <tree_sitter/api.h>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

// ── C++ grammar-shape name / type extraction (extracted from main.cpp) ─

static bool fn_name_segments(TSNode node, const std::string &source, std::vector<std::string> &out) {
    const char *kind = ts_node_type(node);
    if (strcmp(kind, "identifier") == 0 || strcmp(kind, "field_identifier") == 0) {
        out.push_back(node_text(node, source));
        return true;
    }
    if (strcmp(kind, "qualified_identifier") == 0) {
        TSNode scope = ts_node_child_by_field_name(node, "scope", 5);
        if (!ts_node_is_null(scope)) {
            collect_nested(scope, source, out);
            if (out.empty()) {
                out.push_back(node_text(scope, source));
            }
        }
        TSNode name = ts_node_child_by_field_name(node, "name", 4);
        if (!ts_node_is_null(name)) {
            size_t before = out.size();
            collect_nested(name, source, out);
            if (out.size() == before) {
                std::string n = node_text(name, source);
                std::string cleaned;
                int depth = 0;
                for (char c : n) {
                    if (c == '<') depth++;
                    else if (c == '>') { if (depth > 0) depth--; }
                    else if (c == ',' && depth > 0) { }
                    else if (c == ' ' && depth > 0) { }
                    else if (depth == 0) cleaned += c;
                }
                if (!cleaned.empty()) out.push_back(cleaned);
            }
        }
        return !out.empty();
    }
    if (strcmp(kind, "function_declarator") == 0 || strcmp(kind, "reference_declarator") == 0 || strcmp(kind, "pointer_declarator") == 0) {
        TSNode decl = ts_node_child_by_field_name(node, "declarator", 10);
        if (!ts_node_is_null(decl)) return fn_name_segments(decl, source, out);
        return false;
    }
    return false;
}

// ── Type extraction helpers ──────────────────────────────────────────

static std::string type_node_to_fqn(TSNode node, const std::string &source) {
    const char *kind = ts_node_type(node);
    if (strcmp(kind, "type_identifier") == 0) {
        return node_text(node, source);
    }
    if (strcmp(kind, "primitive_type") == 0) {
        return node_text(node, source);
    }
    if (strcmp(kind, "nested_identifier") == 0 || strcmp(kind, "qualified_identifier") == 0) {
        std::string text = node_text(node, source);
        for (size_t p = 0; (p = text.find("::", p)) != std::string::npos; p += 1)
            text.replace(p, 2, ".");
        return text;
    }
    if (strcmp(kind, "template_type") == 0) {
        TSNode base = ts_node_child_by_field_name(node, "name", 4);
        if (!ts_node_is_null(base)) return type_node_to_fqn(base, source);
        return "";
    }
    if (strcmp(kind, "sized_type_specifier") == 0) {
        std::string out;
        uint32_t count = ts_node_named_child_count(node);
        for (uint32_t i = 0; i < count; i++) {
            TSNode child = ts_node_named_child(node, i);
            std::string result = type_node_to_fqn(child, source);
            if (!result.empty()) {
                if (!out.empty()) out += " ";
                out += result;
            }
        }
        return out;
    }
    if (strcmp(kind, "struct_specifier") == 0 || strcmp(kind, "class_specifier") == 0) {
        TSNode name = ts_node_child_by_field_name(node, "name", 4);
        if (!ts_node_is_null(name)) return node_text(name, source);
        return "";
    }
    return "";
}

// Locates the `base_class_clause` of a class/struct/union specifier, or a null
// TSNode when the specifier has no base clause. In this vendored tree-sitter-cpp
// revision the base is an UNNAMED `base_class_clause` child (there is no
// `base`/`type` field on the specifier or on the clause — both production field
// maps are empty), so it is found positionally. The clause is either a direct
// child or one hidden declaration-item level down; both are checked. The class
// body (`field_declaration_list`) is never descended into, so a nested class's
// base is not mistaken for the enclosing class's.
static TSNode base_class_clause_of(TSNode node) {
    uint32_t count = ts_node_child_count(node);
    for (uint32_t i = 0; i < count; i++) {
        TSNode child = ts_node_child(node, i);
        const char *ck = ts_node_type(child);
        if (strcmp(ck, "base_class_clause") == 0) return child;
        if (strcmp(ck, "field_declaration_list") == 0) continue;
        uint32_t inner = ts_node_child_count(child);
        for (uint32_t j = 0; j < inner; j++) {
            TSNode g = ts_node_child(child, j);
            if (strcmp(ts_node_type(g), "base_class_clause") == 0) return g;
        }
    }
    return TSNode{};
}

// The base type as written ("Base", "ns::Base" -> "ns.Base"), or "" when the
// specifier has no base clause. The clause is
// `: [public|private|protected] [virtual] TYPE`; the first child that
// `type_node_to_fqn` recognises is the type, which skips the access and virtual
// specifiers.
static std::string base_type_text(TSNode node, const std::string &source) {
    TSNode clause = base_class_clause_of(node);
    if (ts_node_is_null(clause)) return "";
    uint32_t n = ts_node_named_child_count(clause);
    for (uint32_t i = 0; i < n; i++) {
        std::string t = type_node_to_fqn(ts_node_named_child(clause, i), source);
        if (!t.empty()) return t;
    }
    return "";
}

// ── Variable name extraction from declarator ─────────────────────────

static std::string declarator_name(TSNode node, const std::string &source) {
    const char *kind = ts_node_type(node);
    if (strcmp(kind, "identifier") == 0) {
        return node_text(node, source);
    }
    if (strcmp(kind, "field_identifier") == 0) {
        return node_text(node, source);
    }
    if (strcmp(kind, "reference_declarator") == 0 || strcmp(kind, "pointer_declarator") == 0 ||
        strcmp(kind, "function_declarator") == 0 || strcmp(kind, "array_declarator") == 0) {
        TSNode inner = ts_node_child_by_field_name(node, "declarator", 10);
        if (!ts_node_is_null(inner)) return declarator_name(inner, source);
        return "";
    }
    if (strcmp(kind, "init_declarator") == 0) {
        TSNode inner = ts_node_child_by_field_name(node, "declarator", 10);
        if (!ts_node_is_null(inner)) return declarator_name(inner, source);
        return "";
    }
    return "";
}

// Best-effort parameter type names of a function declarator, in declaration
// order (SPEC §2.3), used for overload disambiguation ingestor-side.
static std::vector<std::string> decl_params(TSNode declarator, const std::string &source) {
    std::vector<std::string> out;
    const char *kind = ts_node_type(declarator);
    if (strcmp(kind, "function_declarator") == 0) {
        TSNode params = ts_node_child_by_field_name(declarator, "parameters", 10);
        if (!ts_node_is_null(params)) {
            uint32_t count = ts_node_named_child_count(params);
            for (uint32_t i = 0; i < count; i++) {
                TSNode param = ts_node_named_child(params, i);
                const char *pk = ts_node_type(param);
                if (strcmp(pk, "parameter_declaration") == 0 || strcmp(pk, "optional_parameter_declaration") == 0) {
                    TSNode ptype = ts_node_child_by_field_name(param, "type", 4);
                    if (!ts_node_is_null(ptype)) {
                        out.push_back(type_node_to_fqn(ptype, source));
                    }
                }
            }
        }
    }
    // Recurse into nested declarators (e.g. pointer to function)
    TSNode inner = ts_node_child_by_field_name(declarator, "declarator", 10);
    if (!ts_node_is_null(inner)) {
        auto nested = decl_params(inner, source);
        out.insert(out.end(), nested.begin(), nested.end());
    }
    return out;
}
