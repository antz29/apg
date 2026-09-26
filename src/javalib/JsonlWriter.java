import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.*;

class JsonlWriter {
    final BufferedWriter out = new BufferedWriter(
        new OutputStreamWriter(System.out, StandardCharsets.UTF_8));

    final Set<String> unresolvedSeen = new HashSet<>();

    /** Escapes a string for a JSON string literal. */
    static String jstr(String s) {
        if (s == null) return "";
        StringBuilder sb = new StringBuilder(s.length() + 16);
        for (int i = 0; i < s.length(); i++) {
            char ch = s.charAt(i);
            switch (ch) {
                case '\\': sb.append("\\\\"); break;
                case '"': sb.append("\\\""); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default:
                    if (ch < 0x20) sb.append(String.format("\\u%04x", (int) ch));
                    else sb.append(ch);
            }
        }
        return sb.toString();
    }

    void emit(String line) {
        try {
            out.write(line);
            out.newLine();
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }

    void emitEdge(String type, String from, String to) {
        emit("{\"type\":\"" + type + "\",\"from\":\"" + jstr(from)
            + "\",\"to\":\"" + jstr(to) + "\"}");
    }

    void emitUnresolvedCall(String from, String to) {
        emit("{\"type\":\"unresolved_call\",\"from\":\"" + jstr(from)
            + "\",\"to\":\"" + jstr(to) + "\"}");
    }

    /** Emits the unresolved node record for fqn on first encounter. */
    void unresolved(String fqn, String category) {
        if (fqn.isEmpty() || unresolvedSeen.contains(fqn)) return;
        unresolvedSeen.add(fqn);
        emit("{\"type\":\"unresolved\",\"fqn\":\"" + jstr(fqn)
            + "\",\"category\":\"" + jstr(category) + "\"}");
    }

    void flush() {
        try {
            out.flush();
            System.err.println();
        } catch (IOException e) {
            throw new RuntimeException(e);
        }
    }
}
