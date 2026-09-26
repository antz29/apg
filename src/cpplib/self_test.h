#pragma once

#include <unistd.h>
#include <algorithm>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <string>

// ── --self-test: the phase-02 task-13 emission-exactness fixture ─────
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

    fs::path all_targets = dir / "all.txt";
    write_file(all_targets, base.string() + "\n" + mainf.string() + "\n");
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

    // (b)/(c) filtered run: cross-file call into the non-target overload is the
    // canonical parent.name(params) FQN.
    check(json_has(filtered, "\"type\":\"calls\"") &&
            json_has(filtered, "\"to\":\"alpha.helper("),
        "(b/c) cross-file calls edge into a non-target overload uses the canonical parent.name(params) FQN");

    // (b) filtered run: base-class `uses` into the non-target file is the FQN.
    check(json_has(filtered, "\"type\":\"uses\"") &&
            json_has(filtered, "\"to\":\"alpha.Base\""),
        "(b) base-class uses edge into a non-target file uses the canonical FQN");

    // (b) the non-target file's own facts are not emitted, but the target
    // file's are.
    check(!json_has(filtered, "\"name\":\"helper\"") &&
            !json_has(filtered, "base.h\""),
        "(b) non-target file's facts are absent from the filtered stream");
    check(json_has(filtered, "\"name\":\"Derived\"") &&
            json_has(filtered, "\"name\":\"add\""),
        "(b) target file's declarations are still emitted");

    fs::remove_all(dir, ec);
    if (failures == 0) {
        printf("cppfrontend self-test PASS\n");
        return 0;
    }
    fprintf(stderr, "cppfrontend self-test FAILED (%d)\n", failures);
    return 1;
}
