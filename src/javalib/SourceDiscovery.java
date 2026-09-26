import com.sun.source.tree.CompilationUnitTree;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.*;

class SourceDiscovery {
    /** Basename of a compilation unit's source file. */
    static String fileBase(CompilationUnitTree u) {
        String p = u.getSourceFile().toUri().getPath();
        int s = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
        return s >= 0 ? p.substring(s + 1) : p;
    }

    /** True for a `module-info.java` module descriptor. */
    static boolean isModuleDescriptor(Path p) {
        return "module-info.java".equals(p.getFileName().toString());
    }

    /**
     * Splits a walked set into attributable sources and module descriptors.
     *
     * A descriptor is part of the WALKED/scanned file set — its `file` record
     * is a graph fact (feedback-138; domain.entity.source-file: every eligible
     * source file is included, filtering is by code_type, never by dropping the
     * file) — but it must never reach a javac task or a `-sourcepath` root: one
     * descriptor in a batch flips javac into NAMED-module attribution (`too
     * many module declarations found`), whose `requires` graph hides every JDK
     * module the descriptor does not read, degrading each such receiver to a
     * bare simple name. Both legs call this before building any batch, so the
     * descriptor's `file` record is always emitted from the walked set (see
     * Collector.emitWalkedFile) and never from a compilation unit.
     */
    static void partitionDescriptors(List<Path> files, List<Path> sources, List<Path> descriptors) {
        for (Path f : files) {
            if (isModuleDescriptor(f)) descriptors.add(f);
            else sources.add(f);
        }
    }

    /**
     * The discovery walk: every non-hidden-directory `.java` file under `dir`,
     * minus the caller's `excludePaths` substrings. Emits the opening/closing
     * progress lines and returns the walked set (module descriptors included;
     * they are partitioned out of attribution downstream).
     */
    static List<Path> collect(Path dir, List<String> excludePaths) throws IOException {
        System.err.println("[" + Progress.elapsed() + "] collecting source files...");
        List<Path> files = new ArrayList<>();
        // Discovery walk. Prune hidden directories — any directory whose
        // basename begins with '.' — notably a nested project worktree under
        // `apg/.worktrees/`. Walking into it compiles the parent checkout's
        // sources and the worktree's copy together, which fails with
        // `duplicate class: CallGraphBuilder`. The other frontends all prune
        // hidden names (domain.entity.scan-exclusion: `.worktrees/` is never a
        // scan root); this brings Java to the same rule. The scan root itself
        // is exempt, so a scan root that lives under `.worktrees/` is still
        // scanned.
        Files.walkFileTree(dir, new SimpleFileVisitor<Path>() {
            @Override
            public FileVisitResult preVisitDirectory(Path d, BasicFileAttributes attrs) {
                Path name = d.getFileName();
                if (!d.equals(dir) && name != null && name.toString().startsWith(".")) {
                    return FileVisitResult.SKIP_SUBTREE;
                }
                return FileVisitResult.CONTINUE;
            }

            // Phase-04 task-15 residual (feedback-138): a `module-info.java`
            // is a module DESCRIPTOR — it declares no package and no
            // struct/function/edge facts — but it is still a SCANNED source
            // file, so it stays in the walked set and the graph still carries
            // its `file` record (domain.entity.source-file: every eligible
            // source file is included; filtering is by code_type, never by
            // dropping the file). It is partitioned OUT of every javac task and
            // every `-sourcepath` root instead (partitionDescriptors): ONE
            // descriptor in a batch flips the whole compilation into
            // NAMED-module mode, whose `requires` graph hides every JDK module
            // the descriptor does not read
            // (java.desktop/java.sql/java.xml/org.xml.sax/...), and each such
            // type then degrades to an error symbol whose calls/uses leak out as
            // bare simple names (`JFrame`, `pack`, `add`, `StreamResult`). A
            // Maven multi-module tree carries one per module (jgrapht), so a
            // one-task full scan reported `too many module declarations found`
            // and lost JDK visibility wholesale. The scanner attributes an
            // UNNAMED-module source tree (global.constraint.frontend-full-
            // context: resolve against the FULL context), so descriptors are
            // attributed nowhere on BOTH legs while remaining part of the
            // scanned file set.
            @Override
            public FileVisitResult visitFile(Path p, BasicFileAttributes attrs) {
                if (!attrs.isRegularFile()) return FileVisitResult.CONTINUE;
                if (!p.toString().endsWith(".java")) return FileVisitResult.CONTINUE;
                if (excludePaths.stream().anyMatch(pat -> p.toString().contains(pat))) {
                    return FileVisitResult.CONTINUE;
                }
                files.add(p);
                return FileVisitResult.CONTINUE;
            }
        });
        System.err.println("[" + Progress.elapsed() + "] " + files.size() + " .java files");
        return files;
    }

    /** Reads the `--targets` list: absolute source paths, one per line, blanks ignored. */
    static List<Path> readTargetList(Path file) {
        List<Path> out = new ArrayList<>();
        try {
            for (String line : Files.readAllLines(file, StandardCharsets.UTF_8)) {
                String s = line.trim();
                if (s.isEmpty()) continue;
                out.add(Paths.get(s).toAbsolutePath().normalize());
            }
        } catch (IOException e) {
            System.err.println("WARNING: could not read targets " + file + ": " + e);
            return new ArrayList<>();
        }
        return out;
    }

    /**
     * A sourcepath entry that resolves the same packages as `root` but exposes
     * no `module-info.java`, so javac never loads a module descriptor from the
     * source path. Returns null when the links cannot be created (the caller
     * then keeps `root` — today's behaviour).
     */
    static Path moduleInfoFreeRoot(Path root, Path tmp) {
        try {
            Files.createDirectories(tmp);
            try (var children = Files.list(root)) {
                for (Path c : (Iterable<Path>) children::iterator) {
                    if (c.getFileName().toString().equals("module-info.java")) continue;
                    Files.createSymbolicLink(tmp.resolve(c.getFileName()), c.toAbsolutePath());
                }
            }
            return tmp;
        } catch (Throwable t) {
            return null;
        }
    }

    /** Best-effort recursive delete of scan-local scratch (the linked roots). */
    static void deleteRec(Path p) {
        if (p == null) return;
        try (var walk = Files.walk(p)) {
            walk.sorted(java.util.Comparator.reverseOrder()).forEach(x -> {
                try {
                    Files.deleteIfExists(x);
                } catch (IOException e) {
                    /* best effort */
                }
            });
        } catch (Throwable t) {
            /* best effort */
        }
    }
}
