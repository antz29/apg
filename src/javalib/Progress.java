class Progress {
    static final long START = System.currentTimeMillis();

    static String elapsed() {
        long s = (System.currentTimeMillis() - START) / 1000;
        return String.format("%d:%02d", s / 60, s % 60);
    }

    /**
     * One newline-terminated progress line per unit, on stderr. stderr goes to
     * a log file (apg-frontend.log), so this is safe to make verbose: it never
     * streams into the terminal/tool.
     */
    static void progress(String label, int done, int total, String item) {
        int pct = total == 0 ? 100 : done * 100 / total;
        System.err.println("[" + elapsed() + "] " + label + " " + pct + "% (" + done + "/" + total + ")"
            + (item.isEmpty() ? "" : " " + item));
        System.err.flush();
    }

    static void endProgress() {
        System.err.println();
        System.err.flush();
    }
}
