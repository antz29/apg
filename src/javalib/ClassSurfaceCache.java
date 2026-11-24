import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

class ClassSurfaceCache {
    /** Per-source declaration surface persisted in the class cache. */
    static final class FileRec {
        String hash = "";
        boolean compiled = false;
        final List<String> structs = new ArrayList<>();
        final List<String> funcs = new ArrayList<>();
        final List<String> flats = new ArrayList<>();
    }

    static String relOf(Path root, Path abs) {
        try {
            return root.relativize(abs.toAbsolutePath().normalize()).toString().replace('\\', '/');
        } catch (Exception e) {
            return abs.toString();
        }
    }

    /** SHA-1 hex of a file's bytes (content identity, never mtime). */
    static String sha1(Path f) {
        try {
            var md = java.security.MessageDigest.getInstance("SHA-1");
            byte[] h = md.digest(Files.readAllBytes(f));
            StringBuilder sb = new StringBuilder(h.length * 2);
            for (byte b : h) sb.append(String.format("%02x", b & 0xff));
            return sb.toString();
        } catch (Exception e) {
            return "";
        }
    }

    static void deleteClasses(Path classesDir, FileRec rec) {
        for (String flat : rec.flats) {
            try {
                Files.deleteIfExists(classesDir.resolve(flat.replace('.', '/') + ".class"));
            } catch (IOException e) {
                /* best effort */
            }
        }
    }

    /**
     * Minimum plausible class-file size: a valid file is at least the 8-byte
     * magic+minor+major header. Anything shorter is truncated and must force a
     * reseed, never be reused as cached bytecode.
     */
    private static final int MIN_CLASS_BYTES = 8;

    private static final byte[] CLASS_MAGIC = {(byte) 0xCA, (byte) 0xFE, (byte) 0xBA, (byte) 0xBE};

    /**
     * True when every class file a cached record references is present, a
     * regular file, at least {@link #MIN_CLASS_BYTES} long, and starts with the
     * CAFEBABE magic. A record declaring no bytecode (`flats` empty) is
     * vacuously intact.
     */
    static boolean classesPresent(Path classesDir, FileRec rec) {
        if (rec.flats.isEmpty()) return true;
        for (String flat : rec.flats) {
            if (!classFileValid(classesDir.resolve(flat.replace('.', '/') + ".class"))) return false;
        }
        return true;
    }

    private static boolean classFileValid(Path p) {
        try {
            if (!Files.isRegularFile(p) || Files.size(p) < MIN_CLASS_BYTES) return false;
            try (var in = Files.newInputStream(p)) {
                byte[] head = in.readNBytes(4);
                if (head.length < 4) return false;
                for (int i = 0; i < 4; i++) if (head[i] != CLASS_MAGIC[i]) return false;
                return true;
            }
        } catch (IOException e) {
            return false;
        }
    }

    /** Best-effort recursive delete of the class cache; warns and continues on failure. */
    private static void wipeClasses(Path classesDir) {
        if (classesDir == null || !Files.exists(classesDir)) return;
        List<Path> entries;
        try (var walk = Files.walk(classesDir)) {
            entries = walk.sorted(Comparator.reverseOrder()).toList();
        } catch (IOException | UncheckedIOException e) {
            System.err.println("WARNING: could not wipe class cache " + classesDir + ": " + e);
            return;
        }
        for (Path p : entries) {
            try {
                Files.deleteIfExists(p);
            } catch (IOException e) {
                System.err.println("WARNING: could not remove cached class file " + p + ": " + e);
            }
        }
    }

    /**
     * Loads the persisted `surface.tsv` (path -> declaration surface).
     *
     * The cache is treated as absent — the sibling `classes/` directory is wiped
     * and an empty surface returned so the scan reseeds — unless `surface.tsv`
     * is a present, readable regular file and every class file it references
     * passes {@link #classesPresent} (present, non-truncated, valid CAFEBABE
     * magic). The per-file `D` hashes remain the wrong-source detector; the
     * class-file checks detect truncated or stale bytecode.
     */
    static Map<String, FileRec> loadSurface(Path file) {
        Path classesDir = file == null || file.getParent() == null
                ? null
                : file.getParent().resolve("classes");
        // Absent/unreadable marker: wipe and reseed rather than reuse stale bytecode.
        if (file == null || classesDir == null || !Files.isRegularFile(file) || !Files.isReadable(file)) {
            wipeClasses(classesDir);
            return new LinkedHashMap<>();
        }
        Map<String, FileRec> out = new LinkedHashMap<>();
        try {
            for (String line : Files.readAllLines(file, StandardCharsets.UTF_8)) {
                if (line.isEmpty()) continue;
                String[] p = line.split("\t", -1);
                if (p.length < 3) continue;
                switch (p[0]) {
                    case "D" -> {
                        FileRec r = new FileRec();
                        r.compiled = true;
                        r.hash = p[2];
                        out.put(p[1], r);
                    }
                    case "S" -> rec(out, p[1]).structs.add(p[2]);
                    case "F" -> rec(out, p[1]).funcs.add(p[2]);
                    case "K" -> rec(out, p[1]).flats.add(p[2]);
                    default -> { }
                }
            }
        } catch (IOException e) {
            System.err.println("WARNING: could not read surface cache: " + e);
            wipeClasses(classesDir);
            return new LinkedHashMap<>();
        }
        for (FileRec r : out.values()) {
            if (!classesPresent(classesDir, r)) {
                wipeClasses(classesDir);
                try {
                    Files.deleteIfExists(file);
                } catch (IOException e) {
                    System.err.println("WARNING: could not delete invalid surface cache " + file + ": " + e);
                }
                return new LinkedHashMap<>();
            }
        }
        return out;
    }

    static FileRec rec(Map<String, FileRec> m, String path) {
        return m.computeIfAbsent(path, k -> new FileRec());
    }

    /**
     * Atomically persists the declaration surface for the next scan.
     *
     * Each `D` record carries the source file's SHA-1 hash — the wrong-source
     * detector compared on the next scan — beside the class-file present/magic/
     * size checks ({@link #classesPresent}) that detect truncated or stale
     * bytecode. The temp+rename keeps a crash from leaving a half-written marker.
     */
    static void saveSurface(Path file, Map<String, FileRec> surface) {
        StringBuilder sb = new StringBuilder();
        for (var e : surface.entrySet()) {
            FileRec r = e.getValue();
            if (!r.compiled) continue;
            sb.append("D\t").append(e.getKey()).append('\t').append(r.hash).append('\n');
            for (String s : r.structs) sb.append("S\t").append(e.getKey()).append('\t').append(s).append('\n');
            for (String f : r.funcs) sb.append("F\t").append(e.getKey()).append('\t').append(f).append('\n');
            for (String k : r.flats) sb.append("K\t").append(e.getKey()).append('\t').append(k).append('\n');
        }
        try {
            Path tmp = file.resolveSibling(file.getFileName() + ".tmp");
            Files.writeString(tmp, sb.toString(), StandardCharsets.UTF_8);
            try {
                Files.move(tmp, file, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
            } catch (Exception e) {
                Files.move(tmp, file, StandardCopyOption.REPLACE_EXISTING);
            }
        } catch (IOException e) {
            System.err.println("WARNING: could not write surface cache: " + e);
        }
    }
}
