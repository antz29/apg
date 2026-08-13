#include <tree_sitter/api.h>
#include <algorithm>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

extern "C" const TSLanguage *tree_sitter_cpp(void);

namespace fs = std::filesystem;

static TSParser *parser = nullptr;

struct Decl {
    std::string kind;
    std::string fqn;
    std::string path;
    uint32_t start;
    uint32_t end;
};

static std::string node_text(TSNode node, const std::string &source) {
    uint32_t start = ts_node_start_byte(node);
    uint32_t end = ts_node_end_byte(node);
    return source.substr(start, end - start);
}

static void emit_json(const std::string &json) {
    printf("%s\n", json.c_str());
}

static std::string clean_fqn(const std::string &raw) {
    std::string out;
    for (char c : raw) {
        if (c == ',') continue;
        if (c == '\n' || c == '\r') continue;
        if (c == '"') continue;
        out += c;
    }
    while (!out.empty() && out.back() == '.') out.pop_back();
    while (!out.empty() && out.front() == '.') out.erase(out.begin());
    size_t i = 0;
    while (i + 1 < out.size()) {
        if (out[i] == '.' && out[i+1] == '.')
            out.erase(i, 1);
        else
            i++;
    }
    return out;
}

static std::string json_esc(const std::string &s) {
    std::string out;
    out.reserve(s.size() + 2);
    for (char c : s) {
        switch (c) {
            case '"': out += "\\\""; break;
            case '\\': out += "\\\\"; break;
            case '\n': out += "\\n"; break;
            case '\r': out += "\\r"; break;
            case '\t': out += "\\t"; break;
            default: out += c;
        }
    }
    return out;
}

struct JsonBuilder {
    std::string buf;
    JsonBuilder() { buf = "{"; }
    JsonBuilder &field(const std::string &key, const std::string &val) {
        if (buf.size() > 1) buf += ",";
        buf += "\"" + key + "\":\"" + json_esc(val) + "\"";
        return *this;
    }
    JsonBuilder &field(const std::string &key, uint32_t val) {
        if (buf.size() > 1) buf += ",";
        buf += "\"" + key + "\":" + std::to_string(val);
        return *this;
    }
    std::string done() { buf += "}"; return buf; }
};

static std::string attr(TSNode node, const std::string &name, const std::string &source) {
    TSNode child = ts_node_child_by_field_name(node, name.c_str(), name.size());
    if (ts_node_is_null(child)) return "";
    return node_text(child, source);
}

static std::string fqn_in_scope(const std::string &name, const std::vector<std::string> &scope) {
    if (scope.empty()) return name;
    std::string out;
    for (size_t i = 0; i < scope.size(); i++) {
        if (i > 0) out += ".";
        out += scope[i];
    }
    out += "." + name;
    return out;
}

static std::string resolve_name(const std::string &name,
    const std::vector<std::string> &scope,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_set<std::string> &fqn_set);
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
        uint32_t count = ts_node_named_child_count(node);
        for (uint32_t i = 0; i < count; i++) {
            TSNode child = ts_node_named_child(node, i);
            std::string result = type_node_to_fqn(child, source);
            if (!result.empty()) return result;
        }
        return "";
    }
    if (strcmp(kind, "struct_specifier") == 0 || strcmp(kind, "class_specifier") == 0) {
        TSNode name = ts_node_child_by_field_name(node, "name", 4);
        if (!ts_node_is_null(name)) return node_text(name, source);
        return "";
    }
    return "";
}

static std::string resolve_type_fqn(const std::string &type_name,
    const std::vector<std::string> &scope,
    const std::unordered_set<std::string> &fqn_set)
{
    if (type_name.empty()) return "";
    if (fqn_set.count(type_name)) return type_name;
    std::string scoped = fqn_in_scope(type_name, scope);
    if (fqn_set.count(scoped)) return scoped;
    return "";
}

static void emit_use(const std::string &source, const std::string &target) {
    JsonBuilder jb;
    jb.field("type", "use");
    jb.field("source", source);
    jb.field("target", target);
    emit_json(jb.done());
}

static void emit_u_call(const std::string &source, const std::string &target) {
    JsonBuilder jb;
    jb.field("type", "u_call");
    jb.field("source", source);
    jb.field("target", target);
    emit_json(jb.done());
}

static void emit_u_use(const std::string &source, const std::string &target) {
    JsonBuilder jb;
    jb.field("type", "u_use");
    jb.field("source", source);
    jb.field("target", target);
    emit_json(jb.done());
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

// ── Declaration collection ───────────────────────────────────────────

static void collect_decls(TSNode node, const std::string &source,
    std::vector<std::string> &scope, const std::string &path,
    std::vector<Decl> &decls,
    std::vector<std::pair<std::string, std::string>> &base_classes)
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
            decls.push_back({"class", fqn, path, ts_node_start_byte(node), ts_node_end_byte(node)});

            // Collect base classes
            TSNode base = ts_node_child_by_field_name(node, "base", 4);
            if (!ts_node_is_null(base)) {
                TSNode base_type = ts_node_child_by_field_name(base, "type", 4);
                if (!ts_node_is_null(base_type)) {
                    std::string base_fqn = type_node_to_fqn(base_type, source);
                    if (!base_fqn.empty()) {
                        base_classes.push_back({fqn, base_fqn});
                    }
                }
            }

            scope.push_back(name);
            TSNode body = ts_node_child_by_field_name(node, "body", 4);
            if (!ts_node_is_null(body)) {
                uint32_t count = ts_node_named_child_count(body);
                for (uint32_t i = 0; i < count; i++) {
                    collect_decls(ts_node_named_child(body, i), source, scope, path, decls, base_classes);
                }
            }
            scope.pop_back();
        }
        return;
    }

    if (strcmp(kind, "enum_specifier") == 0) {
        std::string name = clean_fqn(attr(node, "name", source));
        if (!name.empty()) {
            std::string fqn = clean_fqn(fqn_in_scope(name, scope));
            decls.push_back({"class", fqn, path, ts_node_start_byte(node), ts_node_end_byte(node)});
            scope.push_back(name);
            TSNode body = ts_node_child_by_field_name(node, "body", 4);
            if (!ts_node_is_null(body)) {
                uint32_t count = ts_node_named_child_count(body);
                for (uint32_t i = 0; i < count; i++) {
                    collect_decls(ts_node_named_child(body, i), source, scope, path, decls, base_classes);
                }
            }
            scope.pop_back();
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
                decls.push_back({"method", clean_fqn(fqn), path, ts_node_start_byte(node), ts_node_end_byte(node)});
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

// ── Type-aware reference resolution (use edges + local var tracking) ─

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
    if (!resolved.empty()) {
        emit_use(source_fqn, resolved);
    } else {
        emit_u_use(source_fqn, type_name);
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

            // Emit use edges for base classes
            TSNode base = ts_node_child_by_field_name(node, "base", 4);
            if (!ts_node_is_null(base)) {
                TSNode base_type = ts_node_child_by_field_name(base, "type", 4);
                if (!ts_node_is_null(base_type)) {
                    emit_use_from_type(base_type, source, scope, fqn_set, class_fqn);
                }
            }

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
                    JsonBuilder jb;
                    jb.field("type", "call");
                    jb.field("source", source_fn);
                    jb.field("target", target);
                    emit_json(jb.done());
                } else {
                    emit_u_call(source_fn, name);
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
                                                JsonBuilder jb;
                                                jb.field("type", "call");
                                                jb.field("source", source_fn);
                                                jb.field("target", candidate);
                                                emit_json(jb.done());
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
                            JsonBuilder jb;
                            jb.field("type", "call");
                            jb.field("source", source_fn);
                            jb.field("target", it->second[0]);
                            emit_json(jb.done());
                        } else {
                            emit_u_call(source_fn, method);
                        }
                    }
                }
            } else if (strcmp(fk, "qualified_identifier") == 0) {
                std::string text = node_text(func, source);
                for (size_t p = 0; (p = text.find("::", p)) != std::string::npos; p += 1) {
                    text.replace(p, 2, ".");
                }
                if (fqn_set.count(text)) {
                    JsonBuilder jb;
                    jb.field("type", "call");
                    jb.field("source", source_fn);
                    jb.field("target", text);
                    emit_json(jb.done());
                } else {
                    std::string scoped = fqn_in_scope(text, scope);
                    if (fqn_set.count(scoped)) {
                        JsonBuilder jb;
                        jb.field("type", "call");
                        jb.field("source", source_fn);
                        jb.field("target", scoped);
                        emit_json(jb.done());
                    } else {
                        emit_u_call(source_fn, text);
                    }
                }
            } else {
                std::string tgt = extract_call_target(func, source, scope, name_map, fqn_set);
                if (!tgt.empty()) {
                    JsonBuilder jb;
                    jb.field("type", "call");
                    jb.field("source", source_fn);
                    jb.field("target", tgt);
                    emit_json(jb.done());
                } else {
                    emit_u_call(source_fn, node_text(func, source));
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

static std::string resolve_name(const std::string &name,
    const std::vector<std::string> &scope,
    const std::unordered_map<std::string, std::vector<std::string>> &name_map,
    const std::unordered_set<std::string> &fqn_set)
{
    auto it = name_map.find(name);
    if (it == name_map.end()) return "";
    const auto &candidates = it->second;

    for (int i = (int)scope.size(); i >= 0; i--) {
        std::string prefix;
        for (int j = 0; j < i; j++) {
            if (j > 0) prefix += ".";
            prefix += scope[j];
        }
        for (const auto &candidate : candidates) {
            if (prefix.empty()) {
                if (candidate.find('.') == std::string::npos || candidate == name) {
                    return candidate;
                }
            } else {
                std::string expected = prefix + "." + name;
                if (candidate == expected) return candidate;
            }
        }
    }
    return "";
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

static bool is_cpp_ext(const std::string &ext) {
    return ext == ".cpp" || ext == ".cc" || ext == ".cxx" || ext == ".c++" ||
           ext == ".h" || ext == ".hpp" || ext == ".hh" || ext == ".hxx" ||
           ext == ".tpp" || ext == ".ipp";
}

static bool dir_has_sources(const fs::path &dir) {
    if (!fs::exists(dir)) return false;
    // Non-recursive: only files directly in this dir count, so subdirs that
    // are their own modules don't make the parent a module.
    for (const auto &entry : fs::directory_iterator(dir)) {
        if (!fs::is_regular_file(entry)) continue;
        std::string name = entry.path().filename().string();
        if (name[0] == '.') continue;
        if (is_cpp_ext(entry.path().extension().string())) return true;
    }
    return false;
}

static void get_cpp_files(fs::path dir, std::vector<fs::path> &files,
    const std::vector<std::string> &excludes, bool recursive)
{
    if (!fs::exists(dir)) return;
    if (recursive) {
        for (const auto &entry : fs::recursive_directory_iterator(dir)) {
            if (!fs::is_regular_file(entry)) continue;
            std::string name = entry.path().filename().string();
            if (name[0] == '.') continue;
            if (name == "node_modules" || name == "target") continue;
            std::string path_str = entry.path().string();
            bool excluded = false;
            for (const auto &pat : excludes) {
                if (path_str.find(pat) != std::string::npos) { excluded = true; break; }
            }
            if (excluded) continue;
            if (is_cpp_ext(entry.path().extension().string())) {
                files.push_back(entry.path());
            }
        }
    } else {
        for (const auto &entry : fs::directory_iterator(dir)) {
            if (!fs::is_regular_file(entry)) continue;
            std::string name = entry.path().filename().string();
            if (name[0] == '.') continue;
            if (name == "node_modules" || name == "target") continue;
            std::string path_str = entry.path().string();
            bool excluded = false;
            for (const auto &pat : excludes) {
                if (path_str.find(pat) != std::string::npos) { excluded = true; break; }
            }
            if (excluded) continue;
            if (is_cpp_ext(entry.path().extension().string())) {
                files.push_back(entry.path());
            }
        }
    }
    std::sort(files.begin(), files.end());
}

static std::string read_file(const fs::path &path) {
    FILE *f = fopen(path.c_str(), "rb");
    if (!f) return "";
    fseek(f, 0, SEEK_END);
    long len = ftell(f);
    fseek(f, 0, SEEK_SET);
    std::string out((size_t)len, '\0');
    fread(&out[0], 1, (size_t)len, f);
    fclose(f);
    return out;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "Usage: cppfrontend <dir> [--module <dir>]... [exclude...]\n");
        return 1;
    }

    fs::path root = fs::absolute(argv[1]);

    // Parse --module <dir> pairs; remaining args are excludes.
    std::vector<std::string> module_dirs;
    std::vector<std::string> excludes;
    for (int i = 2; i < argc; i++) {
        if (strcmp(argv[i], "--module") == 0 && i + 1 < argc) {
            module_dirs.push_back(argv[i + 1]);
            i++;
        } else {
            excludes.push_back(argv[i]);
        }
    }

    // Discover modules: explicit --module dirs, or top-level dirs under root
    // that contain source files. The root itself is a module if it has sources.
    struct Module {
        std::string name;
        fs::path dir;
    };
    std::vector<Module> modules;
    if (!module_dirs.empty()) {
        for (const auto &d : module_dirs) {
            fs::path dir = d == "." ? root : fs::absolute(d);
            if (!fs::exists(dir)) {
                fprintf(stderr, "Warning: module dir %s does not exist\n", d.c_str());
                continue;
            }
            std::string name = dir.filename().string();
            if (name.empty()) name = root.filename().string();
            modules.push_back({name, dir});
        }
    } else {
        if (dir_has_sources(root)) {
            modules.push_back({root.filename().string(), root});
        }
        for (const auto &entry : fs::directory_iterator(root)) {
            if (!entry.is_directory()) continue;
            std::string name = entry.path().filename().string();
            if (name[0] == '.') continue;
            if (name == "node_modules" || name == "target" || name == "build") continue;
            if (dir_has_sources(entry.path())) {
                modules.push_back({name, entry.path()});
            }
        }
    }
    if (modules.empty()) {
        fprintf(stderr, "Error: no C++ modules found under %s\n", root.string().c_str());
        return 1;
    }

    parser = ts_parser_new();
    const TSLanguage *lang = tree_sitter_cpp();
    if (!lang) {
        fprintf(stderr, "Failed to load C++ grammar\n");
        return 1;
    }
    ts_parser_set_language(parser, lang);

    struct AstFile {
        std::string path;
        std::string source;
        TSTree *tree;
        std::string module;
    };
    std::vector<AstFile> ast_files;

    for (const auto &mod : modules) {
        std::vector<fs::path> files;
        get_cpp_files(mod.dir, files, excludes, true);
        for (const auto &path : files) {
            std::string source = read_file(path);
            if (source.empty()) continue;
            TSTree *tree = ts_parser_parse_string(parser, nullptr, source.c_str(), source.size());
            if (tree) {
                ast_files.push_back({path.string(), std::move(source), tree, mod.name});
            }
        }
    }

    size_t total = ast_files.size();

    // Phase 1: collect declarations + base classes. The module name is pushed
    // onto the scope so every FQN is module-prefixed (module.namespace.Class),
    // which keeps FQNs unique across modules.
    std::vector<Decl> all_decls;
    std::vector<std::pair<std::string, std::string>> base_classes;
    std::vector<std::string> scope;

    for (auto &af : ast_files) {
        scope.clear();
        scope.push_back(af.module);
        collect_decls(ts_tree_root_node(af.tree), af.source, scope, af.path, all_decls, base_classes);
    }

    // Sort by FQN
    std::sort(all_decls.begin(), all_decls.end(), [](const Decl &a, const Decl &b) {
        return a.fqn < b.fqn;
    });

    std::unordered_set<std::string> decl_fqns;
    for (const auto &d : all_decls) decl_fqns.insert(d.fqn);

    // Build class → methods map for type-aware call resolution
    std::unordered_map<std::string, std::vector<std::string>> class_methods;
    for (const auto &d : all_decls) {
        if (d.kind != "method") continue;
        auto pos = d.fqn.rfind('.');
        if (pos != std::string::npos) {
            std::string parent = d.fqn.substr(0, pos);
            // Only if parent is a declared struct/class
            if (decl_fqns.count(parent)) {
                class_methods[parent].push_back(d.fqn);
            }
        }
    }

    // Emit each module as a top-level pkg node.
    std::unordered_set<std::string> module_names;
    for (const auto &mod : modules) {
        module_names.insert(mod.name);
        emit_json(JsonBuilder().field("type", "pkg").field("fqn", mod.name).done());
    }

    // Emit namespace packages (module.namespace chains).
    std::unordered_set<std::string> all_ns;
    for (const auto &d : all_decls) {
        auto pos = d.fqn.rfind('.');
        if (pos != std::string::npos) {
            std::string ns = d.fqn.substr(0, pos);
            auto pos2 = ns.rfind('.');
            while (true) {
                if (!decl_fqns.count(ns) && !module_names.count(ns)) {
                    all_ns.insert(ns);
                }
                pos2 = ns.rfind('.');
                if (pos2 == std::string::npos) break;
                ns = ns.substr(0, pos2);
            }
        }
    }
    for (const auto &d : all_decls) {
        auto pos = d.fqn.rfind('.');
        if (pos != std::string::npos) {
            std::string ns = d.fqn.substr(0, pos);
            while (true) {
                pos = ns.rfind('.');
                if (pos == std::string::npos) break;
                std::string parent = ns.substr(0, pos);
                if (!decl_fqns.count(parent) && !module_names.count(parent) && parent.find('.') != std::string::npos) {
                    all_ns.insert(parent);
                }
                ns = parent;
            }
        }
    }

    std::vector<std::string> ns_sorted(all_ns.begin(), all_ns.end());
    std::sort(ns_sorted.begin(), ns_sorted.end());

    for (const auto &ns : ns_sorted) {
        emit_json(JsonBuilder().field("type", "pkg").field("fqn", ns).done());
    }

    for (const auto &ns : ns_sorted) {
        auto last_dot = ns.rfind('.');
        if (last_dot != std::string::npos) {
            std::string parent = ns.substr(0, last_dot);
            emit_json(JsonBuilder().field("type", "contains").field("parent", parent).field("child", ns).done());
        }
    }

    for (const auto &d : all_decls) {
        std::string path_str = d.path;
        JsonBuilder jb;
        jb.field("type", "decl");
        jb.field("kind", d.kind);
        jb.field("fqn", d.fqn);
        jb.field("path", path_str);
        jb.field("start", d.start);
        jb.field("end", d.end);
        emit_json(jb.done());

        auto pos = d.fqn.rfind('.');
        if (pos != std::string::npos) {
            std::string parent = d.fqn.substr(0, pos);
            emit_json(JsonBuilder().field("type", "contains").field("parent", parent).field("child", d.fqn).done());
        }
    }

    // Emit use edges for base classes
    for (const auto &bc : base_classes) {
        if (decl_fqns.count(bc.first) && decl_fqns.count(bc.second)) {
            emit_use(bc.first, bc.second);
        }
    }

    // Build name map (only methods/functions)
    std::unordered_map<std::string, std::vector<std::string>> name_map;
    for (const auto &d : all_decls) {
        if (d.kind != "method") continue;
        auto pos = d.fqn.rfind('.');
        std::string name = (pos == std::string::npos) ? d.fqn : d.fqn.substr(pos + 1);
        name_map[name].push_back(d.fqn);
    }

    // Phase 2: resolve references with type awareness. The module name is
    // pushed onto the scope so resolution stays within the current module
    // first, matching the module-prefixed FQNs.
    size_t scan_done = 0;
    for (auto &af : ast_files) {
        scope.clear();
        scope.push_back(af.module);
        resolve_refs(ts_tree_root_node(af.tree), af.source, scope, decl_fqns, name_map, af.module,
            "", nullptr, &class_methods);
        scan_done++;
        fprintf(stderr, "\rScanning: %zu%% (%zu/%zu)", scan_done * 100 / total, scan_done, total);
    }
    fprintf(stderr, "\n");

    // Cleanup
    for (auto &af : ast_files) {
        ts_tree_delete(af.tree);
    }
    ts_parser_delete(parser);

    return 0;
}
