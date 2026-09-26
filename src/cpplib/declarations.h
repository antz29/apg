#pragma once

#include <tree_sitter/api.h>
#include <cstdint>
#include <cstring>
#include <string>
#include <vector>

// ── Declaration records + collection (extracted from main.cpp) ───────

struct Decl {
    std::string kind;      // "class" | "method"
    std::string id;        // opaque id (SPEC §3)
    std::string fqn;       // canonical FQN (parent.name), no params
    std::string name;      // simple name
    std::string parent;    // enclosing scope FQN (module/namespace or class)
    std::string path;
    uint32_t start;
    uint32_t end;
    uint32_t start_line;
    uint32_t end_line;
    std::vector<std::string> params;  // method parameter type names (best-effort)
};

// A class/struct's base-class reference, tagged with the deriving declaration's
// file so the target-set emission filter (phase-02 task-13) can attribute the
// `uses` edge to the file that authored it.
struct BaseRef {
    std::string derived;
    std::string base;
    std::string path;
};

static void collect_decls(TSNode node, const std::string &source,
    std::vector<std::string> &scope, const std::string &path,
    std::vector<Decl> &decls,
    std::vector<BaseRef> &base_classes)
{
    const char *kind = ts_node_type(node);
    if (strcmp(kind, "namespace_definition") == 0) {
        std::string name = clean_fqn(attr(node, "name", source));
        if (!name.empty()) {
            scope.push_back(name);
        }
        TSNode body = ts_node_child_by_field_name(node, "body", 4);
        if (!ts_node_is_null(body)) {
            uint32_t count = ts_node_named_child_count(body);
            for (uint32_t i = 0; i < count; i++) {
                collect_decls(ts_node_named_child(body, i), source, scope, path, decls, base_classes);
            }
        }
        if (!name.empty()) scope.pop_back();
        return;
    }

    if (strcmp(kind, "class_specifier") == 0 || strcmp(kind, "struct_specifier") == 0 ||
        strcmp(kind, "union_specifier") == 0)
    {
        std::string name = clean_fqn(attr(node, "name", source));
        if (!name.empty()) {
            std::string fqn = clean_fqn(fqn_in_scope(name, scope));
            // Skip body-less specifiers (forward declarations like
            // `struct Foo;`): they carry no members and would otherwise
            // collide with the real definition's FQN (e.g. amalgamated
            // headers list the forward decl and the definition separately).
            // Only the first full declaration of an FQN is kept; later
            // redefinitions are dropped rather than colliding in the ingestor.
            TSNode body = ts_node_child_by_field_name(node, "body", 4);
            if (!ts_node_is_null(body) && !structID.count(fqn)) {
                std::string id = newNodeID();
                structID[fqn] = id;
                decls.push_back({"class", id, fqn, name, parent_of(fqn), path,
                                 ts_node_start_byte(node), ts_node_end_byte(node),
                                 line_of(source, ts_node_start_byte(node)),
                                 line_end_of(source, ts_node_end_byte(node)), {}});

                // Collect base classes. The base type is stored as written
                // ("Base") and resolved against the deriving class's enclosing
                // scope at emission time (see main), once the module-prefixed
                // struct-FQN set is known — structID is keyed by that FQN.
                std::string base_fqn = base_type_text(node, source);
                if (!base_fqn.empty()) {
                    base_classes.push_back({fqn, base_fqn, path});
                }
            }

            if (!ts_node_is_null(body)) {
                scope.push_back(name);
                uint32_t count = ts_node_named_child_count(body);
                for (uint32_t i = 0; i < count; i++) {
                    collect_decls(ts_node_named_child(body, i), source, scope, path, decls, base_classes);
                }
                scope.pop_back();
            }
        }
        return;
    }

    if (strcmp(kind, "enum_specifier") == 0) {
        std::string name = clean_fqn(attr(node, "name", source));
        if (!name.empty()) {
            std::string fqn = clean_fqn(fqn_in_scope(name, scope));
            // Same forward-declaration/dedupe rule as class/struct specifiers.
            TSNode body = ts_node_child_by_field_name(node, "body", 4);
            if (!ts_node_is_null(body) && !structID.count(fqn)) {
                std::string id = newNodeID();
                structID[fqn] = id;
                decls.push_back({"class", id, fqn, name, parent_of(fqn), path,
                                 ts_node_start_byte(node), ts_node_end_byte(node),
                                 line_of(source, ts_node_start_byte(node)),
                                 line_end_of(source, ts_node_end_byte(node)), {}});
                scope.push_back(name);
                if (!ts_node_is_null(body)) {
                    uint32_t count = ts_node_named_child_count(body);
                    for (uint32_t i = 0; i < count; i++) {
                        collect_decls(ts_node_named_child(body, i), source, scope, path, decls, base_classes);
                    }
                }
                scope.pop_back();
            }
        }
        return;
    }

    if (strcmp(kind, "function_definition") == 0) {
        TSNode declarator = ts_node_child_by_field_name(node, "declarator", 10);
        if (!ts_node_is_null(declarator)) {
            std::vector<std::string> segments;
            if (fn_name_segments(declarator, source, segments)) {
                std::string fqn;
                for (size_t i = 0; i < segments.size(); i++) {
                    if (i > 0) fqn += ".";
                    fqn += clean_fqn(segments[i]);
                }
                if (!scope.empty()) {
                    fqn = fqn_in_scope(fqn, scope);
                }
                fqn = clean_fqn(fqn);
                std::vector<std::string> params = decl_params(declarator, source);
                std::string name = segments.back();
                std::string parent = parent_of(fqn);
                std::string key = parent + "." + name + "(" + join_params(params) + ")";
                // The scanner is preprocessor-blind: mutually-exclusive #if/#elif
                // branches can each define the same signature (e.g. a GCC and an
                // MSVC implementation). Only one compiles per platform, so keep
                // the first declaration and drop later duplicates rather than
                // colliding in the ingestor.
                if (funcID.count(key)) return;
                std::string id = newNodeID();
                funcID[key] = id;
                if (!funcIDByFqn.count(fqn)) funcIDByFqn[fqn] = id;
                decls.push_back({"method", id, fqn, name, parent, path,
                                 ts_node_start_byte(node), ts_node_end_byte(node),
                                 line_of(source, ts_node_start_byte(node)),
                                 line_end_of(source, ts_node_end_byte(node)), params});
            }
        }
        return;
    }

    if (strcmp(kind, "template_declaration") == 0) {
        uint32_t count = ts_node_child_count(node);
        for (uint32_t i = 0; i < count; i++) {
            TSNode child = ts_node_child(node, i);
            const char *ck = ts_node_type(child);
            if (strcmp(ck, "class_specifier") == 0 || strcmp(ck, "struct_specifier") == 0 ||
                strcmp(ck, "union_specifier") == 0 || strcmp(ck, "enum_specifier") == 0 ||
                strcmp(ck, "function_definition") == 0)
            {
                collect_decls(child, source, scope, path, decls, base_classes);
            }
        }
        return;
    }

    if (strcmp(kind, "template_instantiation") == 0) {
        return;
    }

    if (strcmp(kind, "declaration") == 0 || strcmp(kind, "translation_unit") == 0 || strcmp(kind, "linkage_specification") == 0) {
        uint32_t count = ts_node_named_child_count(node);
        for (uint32_t i = 0; i < count; i++) {
            collect_decls(ts_node_named_child(node, i), source, scope, path, decls, base_classes);
        }
        return;
    }

    uint32_t count = ts_node_named_child_count(node);
    for (uint32_t i = 0; i < count; i++) {
        collect_decls(ts_node_named_child(node, i), source, scope, path, decls, base_classes);
    }
}
