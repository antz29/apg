import java.io.IOException;
import java.nio.file.*;
import java.util.*;
public class CallGraphBuilder {
    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        // `--id-prefix <p>` (default "n") keeps opaque ids unique across
        // frontends when a scan merges multiple languages. The targeted-scan
        // hand-off flags are appended beside it on the incremental path (the
        // pinned target-set interface), and `--cache-dir <dir>` is
        // the per-worktree toolchain cache root. `--module <dir>` (repeatable)
        // is a scan-root-resolved subtree root: it SELECTS the walk, so it
        // never joins `excludePaths`.
        String idPrefix = "n";
        List<String> excludePaths = new ArrayList<>();
        List<Path> moduleRoots = new ArrayList<>();
        String targetsPath = null;
        String cacheDir = null;
        for (int i = 1; i < args.length; i++) {
            if (args[i].equals("--id-prefix") && i + 1 < args.length) {
                idPrefix = args[i + 1];
                i++;
            } else if (args[i].equals("--targets") && i + 1 < args.length) {
                targetsPath = args[i + 1];
                i++;
            } else if (args[i].equals("--cache-dir") && i + 1 < args.length) {
                cacheDir = args[i + 1];
                i++;
            } else if (args[i].equals("--module") && i + 1 < args.length) {
                // An absolute value passes through unchanged; a relative value
                // resolves against the scan root argument, never the process
                // working directory. Resolve the REAL path (before any lexical
                // normalize) so a symlinked value — or a `..` that crosses a
                // symlink — selects the subtree the non-following walk emits.
                Path joined = dir.resolve(args[i + 1]);
                Path physical;
                try {
                    physical = joined.toRealPath();
                } catch (IOException e) {
                    physical = joined.toAbsolutePath().normalize();
                }
                // The walk emits lexical paths, so re-express the physical
                // module root in the scan root's lexical spelling. A root
                // outside the scan root keeps its physical spelling: no walked
                // path can lie under it either way.
                Path moduleRoot = physical;
                try {
                    Path realDir = dir.toRealPath();
                    if (physical.startsWith(realDir)) {
                        moduleRoot = dir.toAbsolutePath().normalize()
                                .resolve(realDir.relativize(physical));
                    }
                } catch (IOException e) {
                    // Scan root unreadable: keep the physical spelling.
                }
                moduleRoots.add(moduleRoot);
                i++;
            } else {
                excludePaths.add(args[i]);
            }
        }
        String prefix = idPrefix;

        List<Path> files = SourceDiscovery.collect(dir, excludePaths, moduleRoots);

        // The target set is an EMISSION filter only. An
        // absent flag or an empty file means NO filter — the byte-identical
        // full-scan path below. A non-empty list in force selects its packages
        // for re-emission (a list matching no walked file emits nothing).
        List<Path> targets = null;
        if (targetsPath != null) {
            targets = SourceDiscovery.readTargetList(Paths.get(targetsPath));
            if (targets.isEmpty()) targets = null;
        }
        if (targets == null) {
            // The pinned --cache-dir hand-off reaches the full-scan path too
            // (the full scan seeds the class cache); the incremental dispatch
            // and the emission filter are unchanged.
            ScanRunner.runFullScan(dir, files, prefix, cacheDir);
        } else {
            ScanRunner.runIncrementalScan(dir, files, targets, prefix, cacheDir);
        }
    }
}
