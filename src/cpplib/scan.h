#pragma once

#include <tree_sitter/api.h>
#include <algorithm>
#include <cstdio>
#include <filesystem>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

// ── Scan orchestration + parser lifecycle (extracted from main.cpp) ──

extern "C" const TSLanguage *tree_sitter_cpp(void);

// The tree-sitter parser handle itself lives in symbol_table.h with the rest of
// the scanner's global state (reset_scan_state clears it); scan_root drives its
// lifecycle below.

// The scanning/emission core, split out of main so the self-test can run it
// repeatedly and capture its stream. Returns 0 on success, 1 when no modules
// are found.
static int scan_root(const fs::path &root_arg,
    const std::vector<std::string> &module_dirs,
    const std::string &targets_path,
    const std::vector<std::string> &excludes)
{
    fs::path root = fs::absolute(root_arg);
    reset_scan_state();

    // The target set is an EMISSION filter only. An absent flag or an empty
    // file means NO filter (the byte-identical full-scan path). A non-empty
    // list that matches no walked file selects nothing — never everything.
    if (!targets_path.empty()) {
        targetFiles = read_target_set(targets_path);
        targetFilterActive = !targetFiles.empty();
    }

    // Discover modules: explicit --module dirs, or top-level dirs under root
    // that contain source files. The root itself is a module if it has sources.
    std::vector<CppModule> modules;
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
            if (dir_has_sources(entry.path())) {
                modules.push_back({name, entry.path()});
            }
        }
        // No shallow module (sources nested below the first level) — search
        // deeper rather than declaring nothing to scan (SPEC 0.9.1).
        if (modules.empty()) {
            find_modules_deep(root, modules, /*depth=*/6);
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

    // Emit one file node per scanned file: parent module, 1..line-count
    // (SPEC §7). Under a target set only the target files' File nodes are
    // emitted; module records below stay global.
    for (const auto &af : ast_files) {
        if (!is_target_file(af.path)) continue;
        emit_json(JsonBuilder().field("type", "file").field("path", af.path)
            .field("parent", af.module)
            .field("start_line", (uint32_t)1)
            .field("end_line", line_of(af.source, (uint32_t)af.source.size())).done());
    }

    // Phase 1: collect declarations + base classes. The module name is pushed
    // onto the scope so every FQN is module-prefixed (module.namespace.Class),
    // which keeps FQNs unique across modules.
    std::vector<Decl> all_decls;
    std::vector<BaseRef> base_classes;
    std::vector<std::string> scope;

    for (auto &af : ast_files) {
        scope.clear();
        scope.push_back(af.module);
        collect_decls(ts_tree_root_node(af.tree), af.source, scope, af.path, all_decls, base_classes);
    }

    // The emission filter's node set: the opaque ids of the decls declared in
    // target files. A target file's edges may reference those ids directly; any
    // other endpoint is emitted by canonical FQN (edge_endpoint) for the
    // ingestor's cached-fact splice. idFqn records every node's rendered FQN so
    // that fallback matches what a full scan's id would normalize to — including
    // the ingestor's overload rendering, parent.name(params).
    if (targetFilterActive) {
        for (const auto &d : all_decls) {
            if (is_target_file(d.path)) emittedIds.insert(d.id);
            idFqn[d.id] = d.fqn;
        }
        std::unordered_map<std::string, int> funcGroups;
        for (const auto &d : all_decls) {
            if (d.kind == "method") funcGroups[d.parent + "\x01" + d.name]++;
        }
        for (const auto &d : all_decls) {
            if (d.kind != "method") continue;
            if (funcGroups[d.parent + "\x01" + d.name] > 1) {
                idFqn[d.id] = d.fqn + "(" + join_params(d.params) + ")";
            }
        }
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

    // Emit each module as a top-level module node.
    std::unordered_set<std::string> module_names;
    for (const auto &mod : modules) {
        module_names.insert(mod.name);
        emit_json(JsonBuilder().field("type", "module").field("fqn", mod.name).done());
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
        emit_json(JsonBuilder().field("type", "module").field("fqn", ns).done());
    }

    for (const auto &ns : ns_sorted) {
        auto last_dot = ns.rfind('.');
        if (last_dot != std::string::npos) {
            std::string parent = ns.substr(0, last_dot);
            emit_edge("contains", parent, ns);
        }
    }

    // Emit struct/function nodes + contains edges.
    for (const auto &d : all_decls) {
        // Under a target set, only the target files' declarations are emitted.
        if (targetFilterActive && !emittedIds.count(d.id)) continue;
        std::string path_str = d.path;
        if (d.kind == "class") {
            JsonBuilder jb;
            jb.field("type", "struct");
            jb.field("id", d.id);
            jb.field("parent", d.parent);
            jb.field("name", d.name);
            jb.field("path", path_str);
            jb.field("start", d.start);
            jb.field("end", d.end);
            jb.field("start_line", d.start_line);
            jb.field("end_line", d.end_line);
            emit_json(jb.done());
        } else {
            std::string paramsJson = "[";
            for (size_t i = 0; i < d.params.size(); i++) {
                if (i > 0) paramsJson += ",";
                paramsJson += "\"" + json_esc(d.params[i]) + "\"";
            }
            paramsJson += "]";
            JsonBuilder jb;
            jb.field("type", "function");
            jb.field("id", d.id);
            jb.field("parent", d.parent);
            jb.field("name", d.name);
            jb.raw("params", paramsJson);
            jb.field("file", path_str);
            jb.field("path", path_str);
            jb.field("start", d.start);
            jb.field("end", d.end);
            jb.field("start_line", d.start_line);
            jb.field("end_line", d.end_line);
            emit_json(jb.done());
        }

        // contains edge: methods and nested types stay directly under their
        // class; top-level classes and free functions are reached through the
        // file node instead of the module/namespace (SPEC §7). A parent class
        // outside the target set is referenced by canonical FQN so the cached
        // unit's node resolves it.
        auto it = structID.find(d.parent);
        if (it != structID.end()) {
            emit_edge("contains", edge_endpoint(d.parent, it->second), d.id);
        }
    }

    // Emit use edges for base classes. bc.base is the base type as written
    // ("Base" or "ns::Base" -> "ns.Base"); resolve it against the deriving
    // class's enclosing scope so the lookup hits structID's module-prefixed key
    // (bare "Base" misses "alpha.Base" — the bug the self-test fixtures). Emits
    // for the full scan and the target-set filter alike; edge_endpoint renders
    // the non-target endpoint by canonical FQN.
    for (const auto &bc : base_classes) {
        if (!is_target_file(bc.path)) continue;
        if (!structID.count(bc.derived)) continue;
        std::string scope_fqn = parent_of(bc.derived);
        std::vector<std::string> base_scope;
        for (size_t p = 0, dot; p < scope_fqn.size(); p = dot + 1) {
            dot = scope_fqn.find('.', p);
            if (dot == std::string::npos) dot = scope_fqn.size();
            base_scope.push_back(scope_fqn.substr(p, dot - p));
            if (dot == scope_fqn.size()) break;
        }
        std::string base_fqn = resolve_type_fqn(bc.base, base_scope, decl_fqns);
        if (!structID.count(base_fqn)) continue;
        emit_edge("uses", edge_endpoint(bc.derived, structID[bc.derived]),
            edge_endpoint(base_fqn, structID[base_fqn]));
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
        // Only the target files' references are emitted; resolution for them
        // still runs against the full declaration context built above.
        if (is_target_file(af.path)) {
            scope.clear();
            scope.push_back(af.module);
            resolve_refs(ts_tree_root_node(af.tree), af.source, scope, decl_fqns, name_map, af.module,
                "", nullptr, &class_methods);
        }
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
