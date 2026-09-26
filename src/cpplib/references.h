#pragma once

#include <tree_sitter/api.h>
#include <cstring>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

// ── Type-aware reference / call resolution (extracted from main.cpp) ─

// Forward declarations: resolve_refs (above its definition) calls resolve_calls
// and resolve_calls calls extract_call_target, both defined below in this
// header; resolve_name lives in naming.h (included earlier).
static std::string extract_call_target(TSNode node, const std::string &source,
    const std::vector<std::string> &scope,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_set<std::string> &fqn_set);
static void resolve_calls(TSNode node, const std::string &source,
    const std::vector<std::string> &scope,
    const std::string &source_fn,
    const std::unordered_set<std::string> &fqn_set,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_map<std::string, std::string> *local_vars,
    const std::unordered_map<std::string, std::vector<std::string>> *class_methods);
static void resolve_refs(TSNode node, const std::string &source,
    std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::string &mod_path,
    const std::string &current_fqn = "",
    std::unordered_map<std::string, std::string> *local_vars = nullptr,
    const std::unordered_map<std::string, std::vector<std::string>> *class_methods = nullptr);

// ── Emit use from a type node ────────────────────────────────────────

static void emit_use_from_type(TSNode type_node, const std::string &source,
    const std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set,
    const std::string &source_fqn)
{
    if (source_fqn.empty()) return;
    std::string type_name = type_node_to_fqn(type_node, source);
    if (type_name.empty()) return;
    std::string resolved = resolve_type_fqn(type_name, scope, fqn_set);
    if (!resolved.empty() && structID.count(resolved)) {
        emit_use(source_fqn, resolved);
    } else if (!resolved.empty()) {
        // Resolved to a declared symbol that is not a struct (e.g. a method
        // name used as a type); a uses edge cannot target it, so record an
        // unresolved use to keep the dependency.
        emit_u_use(source_fqn, type_name, category_for(type_name, true));
    } else {
        emit_u_use(source_fqn, type_name, category_for(type_name, true));
    }
}

// ── Helper to process a declaration/field for type + var name ────────

static void process_type_decl(TSNode node, const std::string &source,
    const std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set,
    const std::string &current_fqn,
    std::unordered_map<std::string, std::string> *local_vars)
{
    TSNode type_n = ts_node_child_by_field_name(node, "type", 4);
    if (!ts_node_is_null(type_n)) {
        emit_use_from_type(type_n, source, scope, fqn_set, current_fqn);
        if (local_vars) {
            TSNode decl = ts_node_child_by_field_name(node, "declarator", 10);
            if (!ts_node_is_null(decl)) {
                std::string var_name = declarator_name(decl, source);
                if (!var_name.empty()) {
                    std::string resolved = resolve_type_fqn(type_node_to_fqn(type_n, source), scope, fqn_set);
                    if (!resolved.empty()) {
                        (*local_vars)[var_name] = resolved;
                    }
                }
            }
        }
    }
}

// ── Walk function declarator to process parameter types ──────────────

static void process_function_params(TSNode declarator, const std::string &source,
    const std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set,
    const std::string &fn_fqn,
    std::unordered_map<std::string, std::string> &local_vars)
{
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
                        emit_use_from_type(ptype, source, scope, fqn_set, fn_fqn);
                        std::string type_fqn = resolve_type_fqn(type_node_to_fqn(ptype, source), scope, fqn_set);
                        TSNode pdecl = ts_node_child_by_field_name(param, "declarator", 10);
                        if (!ts_node_is_null(pdecl) && !type_fqn.empty()) {
                            std::string pname = declarator_name(pdecl, source);
                            if (!pname.empty()) {
                                local_vars[pname] = type_fqn;
                            }
                        }
                    }
                }
            }
        }
    }
    // Recurse into nested declarators (e.g. pointer to function)
    TSNode inner = ts_node_child_by_field_name(declarator, "declarator", 10);
    if (!ts_node_is_null(inner)) {
        process_function_params(inner, source, scope, fqn_set, fn_fqn, local_vars);
    }
}

// ── resolve_refs implementation ──────────────────────────────────────

static void resolve_refs(TSNode node, const std::string &source,
    std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::string &mod_path,
    const std::string &current_fqn,
    std::unordered_map<std::string, std::string> *local_vars,
    const std::unordered_map<std::string, std::vector<std::string>> *class_methods)
{
    const char *kind = ts_node_type(node);

    if (strcmp(kind, "namespace_definition") == 0) {
        std::string name = clean_fqn(attr(node, "name", source));
        if (!name.empty()) scope.push_back(name);
        TSNode body = ts_node_child_by_field_name(node, "body", 4);
        if (!ts_node_is_null(body)) {
            uint32_t count = ts_node_named_child_count(body);
            for (uint32_t i = 0; i < count; i++) {
                resolve_refs(ts_node_named_child(body, i), source, scope, fqn_set, name_map, mod_path,
                    current_fqn, local_vars, class_methods);
            }
        }
        if (!name.empty()) scope.pop_back();
        return;
    }

    if (strcmp(kind, "class_specifier") == 0 || strcmp(kind, "struct_specifier") == 0 ||
        strcmp(kind, "union_specifier") == 0 || strcmp(kind, "enum_specifier") == 0)
    {
        std::string name = clean_fqn(attr(node, "name", source));
        if (!name.empty()) {
            std::string class_fqn = clean_fqn(fqn_in_scope(name, scope));

            // Base-class `uses` edges are NOT emitted here: they are emitted
            // once by the target-set-aware `base_classes` pass in main, which
            // resolves the base against the class's scope. (This branch used
            // to read a non-existent `base`/`type` field and never fired.)

            scope.push_back(name);
            TSNode body = ts_node_child_by_field_name(node, "body", 4);
            if (!ts_node_is_null(body)) {
                uint32_t count = ts_node_named_child_count(body);
                for (uint32_t i = 0; i < count; i++) {
                    TSNode child = ts_node_named_child(body, i);
                    const char *ck = ts_node_type(child);

                    // Track member variable types
                    if (strcmp(ck, "field_declaration") == 0) {
                        process_type_decl(child, source, scope, fqn_set, class_fqn, nullptr);
                    }

                    resolve_refs(child, source, scope, fqn_set, name_map, mod_path,
                        class_fqn, nullptr, class_methods);
                }
            }
            scope.pop_back();
        }
        return;
    }

    if (strcmp(kind, "function_definition") == 0) {
        TSNode declarator = ts_node_child_by_field_name(node, "declarator", 10);
        std::string fn_fqn;
        if (!ts_node_is_null(declarator)) {
            std::vector<std::string> segments;
            if (fn_name_segments(declarator, source, segments)) {
                for (size_t i = 0; i < segments.size(); i++) {
                    if (i > 0) fn_fqn += ".";
                    fn_fqn += segments[i];
                }
                if (!scope.empty()) {
                    fn_fqn = fqn_in_scope(fn_fqn, scope);
                }
            }
        }

        // Build local variable map for this function
        std::unordered_map<std::string, std::string> fn_vars;

        // Process return type
        TSNode ret_type = ts_node_child_by_field_name(node, "type", 4);
        if (!ts_node_is_null(ret_type)) {
            emit_use_from_type(ret_type, source, scope, fqn_set, fn_fqn);
        }

        // Process parameters
        if (!ts_node_is_null(declarator)) {
            process_function_params(declarator, source, scope, fqn_set, fn_fqn, fn_vars);
        }

        // Process body
        TSNode body = ts_node_child_by_field_name(node, "body", 4);
        if (!ts_node_is_null(body)) {
            uint32_t count = ts_node_named_child_count(body);
            for (uint32_t i = 0; i < count; i++) {
                TSNode child = ts_node_named_child(body, i);

                // Track local variable declarations
                const char *ck = ts_node_type(child);
                if (strcmp(ck, "declaration") == 0) {
                    process_type_decl(child, source, scope, fqn_set, fn_fqn, &fn_vars);
                }

                // Resolve calls with type info
                resolve_calls(child, source, scope, fn_fqn, fqn_set, name_map, &fn_vars, class_methods);

                // Recurse for nested blocks
                resolve_refs(child, source, scope, fqn_set, name_map, mod_path,
                    fn_fqn, &fn_vars, class_methods);
            }
        }
        return;
    }

    // Handle standalone declarations at file scope
    if (strcmp(kind, "declaration") == 0) {
        process_type_decl(node, source, scope, fqn_set, current_fqn, local_vars);
        // Fall through to recurse for nested declarators
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
                resolve_refs(child, source, scope, fqn_set, name_map, mod_path,
                    current_fqn, local_vars, class_methods);
            }
        }
        return;
    }

    if (strcmp(kind, "template_instantiation") == 0) {
        return;
    }

    uint32_t count = ts_node_named_child_count(node);
    for (uint32_t i = 0; i < count; i++) {
        resolve_refs(ts_node_named_child(node, i), source, scope, fqn_set, name_map, mod_path,
            current_fqn, local_vars, class_methods);
    }
}

// ── Call resolution with type awareness ──────────────────────────────

static void resolve_calls(TSNode node, const std::string &source,
    const std::vector<std::string> &scope,
    const std::string &source_fn,
    const std::unordered_set<std::string> &fqn_set,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_map<std::string, std::string> *local_vars,
    const std::unordered_map<std::string, std::vector<std::string>> *class_methods)
{
    const char *kind = ts_node_type(node);

    if (strcmp(kind, "call_expression") == 0) {
        TSNode func = ts_node_child_by_field_name(node, "function", 8);
        if (!ts_node_is_null(func)) {
            const char *fk = ts_node_type(func);

            if (strcmp(fk, "identifier") == 0) {
                std::string name = node_text(func, source);
                std::string target = resolve_name(name, scope, name_map, fqn_set);
                if (!target.empty()) {
                    emit_call(source_fn, target);
                } else {
                    emit_u_call(source_fn, name, category_for(name, false));
                }
            } else if (strcmp(fk, "field_expression") == 0) {
                TSNode field = ts_node_child_by_field_name(func, "field", 5);
                if (!ts_node_is_null(field)) {
                    std::string method = node_text(field, source);
                    bool resolved = false;

                    // Try type-aware resolution
                    if (class_methods && local_vars) {
                        // Extract the object expression
                        uint32_t nc = ts_node_named_child_count(func);
                        if (nc >= 2) {
                            TSNode obj = ts_node_named_child(func, 0);
                            const char *ok = ts_node_type(obj);
                            if (strcmp(ok, "identifier") == 0) {
                                std::string obj_name = node_text(obj, source);
                                auto vit = local_vars->find(obj_name);
                                if (vit != local_vars->end()) {
                                    const std::string &type_fn = vit->second;
                                    auto mit = class_methods->find(type_fn);
                                    if (mit != class_methods->end()) {
                                        // Look for method in this class
                                        for (const auto &candidate : mit->second) {
                                            auto pos = candidate.rfind('.');
                                            std::string mname = (pos == std::string::npos) ? candidate : candidate.substr(pos + 1);
                                            if (mname == method) {
                                                emit_call(source_fn, candidate);
                                                resolved = true;
                                                break;
                                            }
                                        }
                                    }
                                    // Receiver type known but method not found in
                                    // it: record a use edge on the receiver type
                                    // so the dependency is kept at type level.
                                    if (!resolved) {
                                        emit_use(source_fn, type_fn);
                                        resolved = true;
                                    }
                                }
                            }
                        }
                    }

                    // Fallback: only resolve if unambiguous; otherwise record
                    // an unresolved call so the dependency is not lost.
                    if (!resolved) {
                        auto it = name_map.find(method);
                        if (it != name_map.end() && it->second.size() == 1) {
                            emit_call(source_fn, it->second[0]);
                        } else {
                            emit_u_call(source_fn, method, category_for(method, false));
                        }
                    }
                }
            } else if (strcmp(fk, "qualified_identifier") == 0) {
                std::string text = node_text(func, source);
                for (size_t p = 0; (p = text.find("::", p)) != std::string::npos; p += 1) {
                    text.replace(p, 2, ".");
                }
                if (fqn_set.count(text)) {
                    emit_call(source_fn, text);
                } else {
                    std::string scoped = fqn_in_scope(text, scope);
                    if (fqn_set.count(scoped)) {
                        emit_call(source_fn, scoped);
                    } else {
                        emit_u_call(source_fn, text, category_for(text, false));
                    }
                }
            } else {
                std::string tgt = extract_call_target(func, source, scope, name_map, fqn_set);
                if (!tgt.empty()) {
                    emit_call(source_fn, tgt);
                } else {
                    emit_u_call(source_fn, node_text(func, source),
                        category_for(node_text(func, source), false));
                }
            }
        }
        return;
    }

    uint32_t count = ts_node_named_child_count(node);
    for (uint32_t i = 0; i < count; i++) {
        resolve_calls(ts_node_named_child(node, i), source, scope, source_fn, fqn_set, name_map,
            local_vars, class_methods);
    }
}

static std::string extract_call_target(TSNode node, const std::string &source,
    const std::vector<std::string> &scope,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_set<std::string> &fqn_set)
{
    const char *kind = ts_node_type(node);
    if (strcmp(kind, "identifier") == 0) {
        return resolve_name(node_text(node, source), scope, name_map, fqn_set);
    }
    if (strcmp(kind, "field_expression") == 0) {
        TSNode field = ts_node_child_by_field_name(node, "field", 5);
        if (!ts_node_is_null(field)) {
            std::string method = node_text(field, source);
            auto it = name_map.find(method);
            // Only resolve if unambiguous; otherwise leave unresolved.
            if (it != name_map.end() && it->second.size() == 1) return it->second[0];
        }
        return "";
    }
    if (strcmp(kind, "qualified_identifier") == 0) {
        std::string text = node_text(node, source);
        for (size_t p = 0; (p = text.find("::", p)) != std::string::npos; p += 1) {
            text.replace(p, 2, ".");
        }
        if (fqn_set.count(text)) return text;
        return "";
    }
    return "";
}
