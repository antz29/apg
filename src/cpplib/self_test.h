#pragma once

#include <unistd.h>
#include <algorithm>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <string>

// ── --self-test: the target-set emission-exactness fixture ───────────
//
// src/cpplib has no test harness of its own (build.rs compiles main.cpp into
// the cppfrontend binary and nothing else), so this mode embeds a ≥2-file
// fixture, runs the real scan_root over it three times — no filter, all files
// as targets, and one file as the target — and asserts the target-set
// emission contract:
//   (a) an all-targets run is byte-identical to the no-filter full scan;
//   (b) a cross-file `calls`/base-class `uses` edge into a NON-target file is
//       emitted with the ingestor's canonical FQN, not an opaque id, while the
//       non-target file's own facts are absent;
//   (c) the cross-file endpoint of an overloaded group carries the ingestor's
//       `parent.name(params)` rendering (the overload suffix).
// It also proves a relative --module value resolves against the scan-root
// argument: it selects exactly the same subtree (the same record set) as its
// absolute form, regardless of the process working directory.
// It also pins the module identity: each source-holding directory's ABSOLUTE
// path, with the module record, the file parent and the declaration scope in
// agreement — a base-level source, a nested source-holding directory beside it,
// a dotted directory name (one identity, no phantom ancestor) and a source
// directory more than six levels down (the formerly depth-capped walk).
static bool json_has(const std::string &hay, const std::string &needle) {
    return hay.find(needle) != std::string::npos;
}

static int run_self_test() {
    int failures = 0;
    auto check = [&](bool ok, const char *what) {
        fprintf(stderr, "%s: %s\n", ok ? "ok  " : "FAIL", what);
        if (!ok) failures++;
    };

    fs::path dir = fs::temp_directory_path() /
        ("cppfrontend_selftest_" + std::to_string((long)getpid()));
    std::error_code ec;
    fs::remove_all(dir, ec);
    fs::create_directories(dir / "alpha", ec);
    fs::create_directories(dir / "nested", ec);
    fs::create_directories(dir / "foo.bar", ec);

    auto write_file = [](const fs::path &p, const std::string &body) {
        std::ofstream out(p, std::ios::binary);
        out << body;
    };

    // Non-target file: a base class plus an overloaded free-function group.
    fs::path base = dir / "alpha" / "base.h";
    write_file(base,
        "#pragma once\n"
        "struct Base {\n"
        "    int seed;\n"
        "};\n"
        "inline int helper(int x) { return x; }\n"
        "inline int helper(double x) { return (int)x; }\n");

    // Target file: derives from Base and calls the overloaded helper.
    fs::path mainf = dir / "alpha" / "main.cpp";
    write_file(mainf,
        "#include \"base.h\"\n"
        "struct Derived : public Base {\n"
        "    int add(int a) { return helper(a); }\n"
        "};\n");

    // A base-level source: the scan root itself is a module of just its direct
    // files.
    fs::path rooted = dir / "rooted.cpp";
    write_file(rooted, "struct Rooted { int x; };\n");

    // A nested source-holding directory beside the base-level source: it keeps
    // its own identity, and its real namespace chain still derives only beyond
    // that identity.
    fs::path nested = dir / "nested" / "nested.cpp";
    write_file(nested, "namespace inner { struct Nested { int x; }; }\n");

    // A directory whose name contains a dot: ONE identity, no phantom ancestor.
    fs::path dotted = dir / "foo.bar" / "dotted.cpp";
    write_file(dotted, "struct Dotted { int x; };\n");

    fs::path all_targets = dir / "all.txt";
    write_file(all_targets, base.string() + "\n" + mainf.string() + "\n" +
        rooted.string() + "\n" + nested.string() + "\n" + dotted.string() + "\n");
    fs::path one_target = dir / "one.txt";
    write_file(one_target, mainf.string() + "\n");

    std::string full, all, filtered;
    captureOut = &full;
    scan_root(dir, {}, "", {});
    captureOut = &all;
    scan_root(dir, {}, all_targets.string(), {});
    captureOut = &filtered;
    scan_root(dir, {}, one_target.string(), {});
    captureOut = nullptr;

    std::string alpha_id = (dir / "alpha").string();
    std::string base_id = dir.string();
    std::string nested_id = (dir / "nested").string();
    std::string dotted_id = (dir / "foo.bar").string();

    // (a) all targets ⇒ byte-identical to the no-filter stream.
    check(full == all, "(a) all-targets stream is byte-identical to the full scan");
    if (full != all) {
        size_t n = std::min(full.size(), all.size()), i = 0;
        while (i < n && full[i] == all[i]) i++;
        fprintf(stderr, "  first divergence at byte %zu\n", i);
    }

    // (c) the full scan carries both overload declarations.
    check(json_has(full, "\"name\":\"helper\",\"params\":[\"int\"]"),
        "(c) full scan declares helper(int)");
    check(json_has(full, "\"name\":\"helper\",\"params\":[\"double\"]"),
        "(c) full scan declares helper(double)");

    // The module identity is the directory's absolute path, in agreement across
    // the module record, the file parent and the declaration scope.
    check(json_has(full, "\"type\":\"module\",\"fqn\":\"" + base_id + "\""),
        "a base-level source makes the scan root its own module");
    check(json_has(full, "\"type\":\"file\",\"path\":\"" + rooted.string() +
            "\",\"parent\":\"" + base_id + "\""),
        "the base-level file is parented by the base module");
    check(json_has(full, "\"parent\":\"" + base_id + "\",\"name\":\"Rooted\""),
        "the base-level declaration is scoped to the base module");

    check(json_has(full, "\"type\":\"module\",\"fqn\":\"" + nested_id + "\""),
        "a nested source-holding dir is its own module beside the base one");
    check(json_has(full, "\"type\":\"file\",\"path\":\"" + nested.string() +
            "\",\"parent\":\"" + nested_id + "\""),
        "the nested file is parented by its own module, not the base");
    check(json_has(full, "\"type\":\"module\",\"fqn\":\"" + nested_id + ".inner\""),
        "a real namespace inside a module still derives a namespace module");
    check(json_has(full, "\"type\":\"contains\",\"from\":\"" + nested_id +
            "\",\"to\":\"" + nested_id + ".inner\""),
        "the namespace module is contained by its module identity");
    check(json_has(full, "\"parent\":\"" + nested_id + ".inner\",\"name\":\"Nested\""),
        "the namespaced declaration is scoped strictly beyond the module identity");

    check(json_has(full, "\"type\":\"module\",\"fqn\":\"" + dotted_id + "\""),
        "a dotted directory name is one module identity");
    check(json_has(full, "\"type\":\"file\",\"path\":\"" + dotted.string() +
            "\",\"parent\":\"" + dotted_id + "\""),
        "the dotted dir's file is parented by that one identity");
    check(json_has(full, "\"parent\":\"" + dotted_id + "\",\"name\":\"Dotted\""),
        "the dotted dir's declaration is scoped to that one identity");
    check(!json_has(full, "\"type\":\"module\",\"fqn\":\"" + (dir / "foo").string() + "\""),
        "no phantom module is fabricated at the dot in a directory name");

    // (b)/(c) filtered run: cross-file call into the non-target overload is the
    // canonical parent.name(params) FQN.
    check(json_has(filtered, "\"type\":\"calls\"") &&
            json_has(filtered, "\"to\":\"" + alpha_id + ".helper("),
        "(b/c) cross-file calls edge into a non-target overload uses the canonical parent.name(params) FQN");

    // (b) filtered run: base-class `uses` into the non-target file is the FQN.
    check(json_has(filtered, "\"type\":\"uses\"") &&
            json_has(filtered, "\"to\":\"" + alpha_id + ".Base\""),
        "(b) base-class uses edge into a non-target file uses the canonical FQN");

    // (b) the non-target file's own facts are not emitted, but the target
    // file's are.
    check(!json_has(filtered, "\"name\":\"helper\"") &&
            !json_has(filtered, "base.h\""),
        "(b) non-target file's facts are absent from the filtered stream");
    check(json_has(filtered, "\"name\":\"Derived\"") &&
            json_has(filtered, "\"name\":\"add\""),
        "(b) target file's declarations are still emitted");

    // A relative --module value resolves against the scan-root argument, so it
    // selects the same subtree as its absolute form whatever the process
    // working directory is (the self-test never changes directory).
    std::string rel_module, abs_module;
    captureOut = &rel_module;
    scan_root(dir, {"alpha"}, "", {});
    captureOut = &abs_module;
    scan_root(dir, {(dir / "alpha").string()}, "", {});
    captureOut = nullptr;
    check(rel_module == abs_module,
        "a relative --module selects the same record set as the absolute form");
    check(json_has(rel_module, "\"type\":\"file\",\"path\":\"" + base.string() + "\"") &&
            json_has(rel_module, "\"type\":\"file\",\"path\":\"" + mainf.string() + "\""),
        "the module selection admits the requested subtree's files");
    check(!json_has(rel_module, rooted.string()) &&
            !json_has(rel_module, nested.string()) &&
            !json_has(rel_module, dotted.string()),
        "the module selection admits no file outside the requested subtree");

    // The formerly depth-capped deep walk: a separate fixture root whose only
    // sources sit more than six levels down must be discovered.
    fs::path deep_root = fs::temp_directory_path() /
        ("cppfrontend_selftest_deep_" + std::to_string((long)getpid()));
    fs::remove_all(deep_root, ec);
    fs::path deep_dir = deep_root / "a" / "b" / "c" / "d" / "e" / "f" / "g";
    fs::create_directories(deep_dir, ec);
    write_file(deep_dir / "deep.cpp", "struct Deep { int x; };\n");

    std::string deep;
    captureOut = &deep;
    scan_root(deep_root, {}, "", {});
    captureOut = nullptr;
    check(json_has(deep, "\"type\":\"module\",\"fqn\":\"" + deep_dir.string() + "\""),
        "the unbounded walk finds a source dir more than six levels down");
    check(json_has(deep, "\"parent\":\"" + deep_dir.string() + "\",\"name\":\"Deep\""),
        "a deeply nested declaration is scoped to its own module identity");
    fs::remove_all(deep_root, ec);

    fs::remove_all(dir, ec);
    if (failures == 0) {
        printf("cppfrontend self-test PASS\n");
        return 0;
    }
    fprintf(stderr, "cppfrontend self-test FAILED (%d)\n", failures);
    return 1;
}
