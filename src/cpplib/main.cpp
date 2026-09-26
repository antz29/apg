// C++ scanner frontend — thin translation unit assembling the implementation
// headers. The scanner's units are #included in dependency order; the single
// translation unit is unchanged from the pre-decomposition build (same code,
// same call order), so the emitted JSONL stream is byte-identical.
#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "jsonl.h"
#include "ts_nodes.h"
#include "ast_names.h"
#include "naming.h"
#include "symbol_table.h"
#include "edges.h"
#include "declarations.h"
#include "references.h"
#include "source_discovery.h"
#include "scan.h"
#include "self_test.h"

int main(int argc, char **argv) {
    if (argc >= 2 && strcmp(argv[1], "--self-test") == 0) {
        return run_self_test();
    }
    if (argc < 2) {
        fprintf(stderr, "Usage: cppfrontend <dir> [--module <dir>]... [--targets <file>] [--cache-dir <dir>] [--cache-key <key>] [exclude...]\n");
        return 1;
    }

    // Parse --module <dir> pairs, --id-prefix, and the pinned target-set
    // hand-off flags (phase-02 task-13); remaining args are excludes.
    std::vector<std::string> module_dirs;
    std::vector<std::string> excludes;
    std::string targets_path, cache_dir, cache_key;
    for (int i = 2; i < argc; i++) {
        if (strcmp(argv[i], "--module") == 0 && i + 1 < argc) {
            module_dirs.push_back(argv[i + 1]);
            i++;
        } else if (strcmp(argv[i], "--id-prefix") == 0 && i + 1 < argc) {
            idPrefix = argv[i + 1];
            i++;
        } else if (strcmp(argv[i], "--targets") == 0 && i + 1 < argc) {
            targets_path = argv[i + 1];
            i++;
        } else if (strcmp(argv[i], "--cache-dir") == 0 && i + 1 < argc) {
            cache_dir = argv[i + 1];
            i++;
        } else if (strcmp(argv[i], "--cache-key") == 0 && i + 1 < argc) {
            cache_key = argv[i + 1];
            i++;
        } else {
            excludes.push_back(argv[i]);
        }
    }

    // C++ is heuristic/per-file, so it needs no native compiler cache: the
    // pinned per-language artifact location (<cache-dir>/cpp/<cache-key>/) is
    // unused. The flags are still parsed so they are never mistaken for
    // excludes.
    (void)cache_dir;
    (void)cache_key;

    return scan_root(argv[1], module_dirs, targets_path, excludes);
}
