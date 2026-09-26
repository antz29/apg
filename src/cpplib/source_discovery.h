#pragma once

#include <filesystem>
#include <algorithm>
#include <cstdio>
#include <fstream>
#include <string>
#include <unordered_set>
#include <vector>

// ── Filesystem discovery + target-list read (extracted from main.cpp) ─

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

struct CppModule {
    std::string name;
    fs::path dir;
};

// Recursively finds the shallowest directories that directly contain C++
// source files, skipping vendored/build noise dirs. Used as a fallback when the
// top-level scan finds nothing, so a repo whose C++ code lives below the first
// level (e.g. src/cpplib) still resolves a module (SPEC 0.9.1).
static void find_modules_deep(const fs::path &dir, std::vector<CppModule> &modules,
    int depth) {
    if (depth <= 0) return;
    if (dir_has_sources(dir)) {
        std::string name = dir.filename().string();
        if (name.empty()) name = "root";
        modules.push_back({name, dir});
        return;
    }
    for (const auto &entry : fs::directory_iterator(dir)) {
        if (!entry.is_directory()) continue;
        std::string name = entry.path().filename().string();
        if (name[0] == '.') continue;
        if (name == "vendor" || name == "third_party" || name == "external" ||
            name == "node_modules" || name == "target" || name == "build" ||
            name == "out" || name == "cmake-build-debug" ||
            name == "cmake-build-release") {
            continue;
        }
        find_modules_deep(entry.path(), modules, depth - 1);
    }
}

static void get_cpp_files(fs::path dir, std::vector<fs::path> &files,
    const std::vector<std::string> &excludes, bool recursive)
{
    if (!fs::exists(dir)) return;
    if (recursive) {
        // Skip hidden directories (basename begins with '.') entirely — notably
        // a nested project worktree under `apg/.worktrees/`. A plain
        // recursive_directory_iterator descends into them and picks up the
        // worktree's duplicate sources; the other frontends prune hidden names
        // (domain.entity.scan-exclusion: `.worktrees/` is never a scan root).
        for (auto it = fs::recursive_directory_iterator(dir);
             it != fs::recursive_directory_iterator(); ++it) {
            const fs::directory_entry &entry = *it;
            std::string name = entry.path().filename().string();
            if (entry.is_directory()) {
                if (!name.empty() && name[0] == '.') it.disable_recursion_pending();
                continue;
            }
            if (!entry.is_regular_file()) continue;
            if (!name.empty() && name[0] == '.') continue;
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

// Reads the pinned `--targets` list (phase-02 task-13): a UTF-8,
// newline-delimited file of absolute source-file paths, one per line, no
// header; blank (and whitespace-only) lines are ignored. A missing/unreadable
// file warns and yields an empty set, which the caller treats as "no filter".
static std::unordered_set<std::string> read_target_set(const std::string &path) {
    std::unordered_set<std::string> out;
    std::ifstream in(path);
    if (!in) {
        fprintf(stderr, "Warning: could not read targets %s\n", path.c_str());
        return out;
    }
    std::string line;
    while (std::getline(in, line)) {
        size_t a = line.find_first_not_of(" \t\r");
        if (a == std::string::npos) continue;
        size_t b = line.find_last_not_of(" \t\r");
        out.insert(normalized_path(line.substr(a, b - a + 1)));
    }
    return out;
}
