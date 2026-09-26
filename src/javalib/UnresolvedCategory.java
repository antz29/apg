class UnresolvedCategory {
    /** Classifies an out-of-project method FQN as stdlib or external. */
    static String categoryOf(String mfqn) {
        if (mfqn == null) return "unknown";
        for (String p : new String[]{"java.", "javax.", "jdk.", "com.sun.", "sun."}) {
            if (mfqn.startsWith(p)) return "stdlib";
        }
        return "external";
    }
}
