import java.io.IOException;
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

    /** True when a cached record's bytecode is still present in the class dir. */
    static boolean classesPresent(Path classesDir, FileRec rec) {
        if (rec.flats.isEmpty()) return true;
        for (String flat : rec.flats) {
            if (Files.exists(classesDir.resolve(flat.replace('.', '/') + ".class"))) return true;
        }
        return false;
    }

    /** Loads the persisted `surface.tsv` (path -> declaration surface). */
    static Map<String, FileRec> loadSurface(Path file) {
        Map<String, FileRec> out = new LinkedHashMap<>();
        if (!Files.exists(file)) return out;
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
        }
        return out;
    }

    static FileRec rec(Map<String, FileRec> m, String path) {
        return m.computeIfAbsent(path, k -> new FileRec());
    }

    /** Atomically persists the declaration surface for the next scan. */
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
