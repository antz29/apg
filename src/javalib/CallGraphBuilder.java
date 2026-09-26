import java.nio.file.*;
import java.util.*;
public class CallGraphBuilder {
    public static void main(String[] args) throws Exception {
        Path dir = Paths.get(args[0]);
        // `--id-prefix <p>` (default "n") keeps opaque ids unique across
        // frontends when a scan merges multiple languages. The three
        // phase-02 task-11 hand-off flags are appended beside it on the
        // incremental path (the pinned target-set interface).
        String idPrefix = "n";
        List<String> excludePaths = new ArrayList<>();
        String targetsPath = null;
        String cacheDir = null;
        String cacheKey = null;
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
            } else if (args[i].equals("--cache-key") && i + 1 < args.length) {
                cacheKey = args[i + 1];
                i++;
            } else {
                excludePaths.add(args[i]);
            }
        }
        String prefix = idPrefix;

        List<Path> files = SourceDiscovery.collect(dir, excludePaths);

        // The target set is an EMISSION filter only (phase-02 task-11). An
        // absent flag or an empty file means NO filter — the byte-identical
        // full-scan path below. A non-empty list in force selects its packages
        // for re-emission (a list matching no walked file emits nothing).
        List<Path> targets = null;
        if (targetsPath != null) {
            targets = SourceDiscovery.readTargetList(Paths.get(targetsPath));
            if (targets.isEmpty()) targets = null;
        }
        if (targets == null) {
            // Phase-04 task-32: the pinned --cache-dir/--cache-key hand-off
            // reaches the full-scan path too (task-17 seeds the class cache);
            // the incremental dispatch and the emission filter are unchanged.
            ScanRunner.runFullScan(dir, files, prefix, cacheDir, cacheKey);
        } else {
            ScanRunner.runIncrementalScan(dir, files, targets, prefix, cacheDir, cacheKey);
        }
    }
}
