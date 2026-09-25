use super::*;

fn fd(id: &str, parent: &str, name: &str, params: &[&str], file: &str) -> FuncDecl {
    FuncDecl {
        id: id.to_string(),
        parent: parent.to_string(),
        name: name.to_string(),
        params: params.iter().map(|s| s.to_string()).collect(),
        file: file.to_string(),
        path: "/x/a.go".to_string(),
        start: 0,
        end: 1,
        start_line: 1,
        end_line: 1,
        language: "go".to_string(),
    }
}

/// A Python function declaration with an explicit start line, so
/// [`render_function_fqns`]'s `py` duplicate-name rule can be exercised
/// in-memory (the `fd` helper pins `go` and line 1).
fn fd_py(id: &str, parent: &str, name: &str, params: &[&str], file: &str, line: u32) -> FuncDecl {
    FuncDecl {
        id: id.to_string(),
        parent: parent.to_string(),
        name: name.to_string(),
        params: params.iter().map(|s| s.to_string()).collect(),
        file: file.to_string(),
        path: file.to_string(),
        start: 0,
        end: 1,
        start_line: line,
        end_line: line,
        language: "py".to_string(),
    }
}

fn fqns(decls: &[FuncDecl]) -> HashMap<String, String> {
    render_function_fqns(decls).into_iter().collect()
}

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    #[test]
    fn unique_function_keeps_simple_name() {
        let decls = [fd("n1", "pkg", "foo", &[], "/x/a.go")];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.foo");
    }

    #[test]
    fn overloads_get_param_suffix() {
        let decls = [
            fd("n1", "pkg.C", "foo", &["int"], "/x/a.go"),
            fd("n2", "pkg.C", "foo", &["java.lang.String"], "/x/a.go"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.C.foo(int)");
        assert_eq!(m["n2"], "pkg.C.foo(java.lang.String)");
    }

    #[test]
    fn go_init_disambiguated_by_file() {
        let decls = [
            fd("n1", "pkg", "init", &[], "/x/a.go"),
            fd("n2", "pkg", "init", &[], "/x/b.go"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.init#a.go");
        assert_eq!(m["n2"], "pkg.init#b.go");
    }

    #[test]
    fn zero_param_overload_gets_empty_suffix() {
        let decls = [
            fd("n1", "pkg.C", "foo", &[], "/x/a.go"),
            fd("n2", "pkg.C", "foo", &["int"], "/x/a.go"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.C.foo()");
        assert_eq!(m["n2"], "pkg.C.foo(int)");
    }

    /// Phase-08 task-15: Python duplicate-name disambiguation. Two
    /// identically-annotated `@overload` stubs of one name (their erased
    /// param lists collide) and a same-scope conditional redefinition (the
    /// empty param list) each render a DISTINCT FQN in the FULL form
    /// `parent.name(T1,T2,...)#<file-basename>:<start_line>`: the erased
    /// param-list suffix is RETAINED on the colliding member — including the
    /// empty `()` form, never dropped to `parent.name#...` — with the
    /// `#<file-basename>:<start_line>` disambiguator appended. Because every
    /// colliding member claims a unique FQN, a call to each resolves to its
    /// own Function node and `claim` never panics.
    #[test]
    fn py_duplicate_name_declarations_render_distinct_full_fqns() {
        let decls = [
            // Two `@overload` stubs of `f`, both annotated `int` — the
            // erased param list `int` collides.
            fd_py("n1", "pkg.mod", "f", &["int"], "/root/pkg/mod.py", 10),
            fd_py("n2", "pkg.mod", "f", &["int"], "/root/pkg/mod.py", 24),
            // A same-scope conditional redefinition of `g` — both branches
            // erase to the EMPTY param list.
            fd_py("n3", "pkg.mod", "g", &[], "/root/pkg/mod.py", 40),
            fd_py("n4", "pkg.mod", "g", &[], "/root/pkg/mod.py", 52),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "pkg.mod.f(int)#mod.py:10");
        assert_eq!(m["n2"], "pkg.mod.f(int)#mod.py:24");
        // The `()` form is retained — NOT dropped to `pkg.mod.g#…`.
        assert_eq!(m["n3"], "pkg.mod.g()#mod.py:40");
        assert_eq!(m["n4"], "pkg.mod.g()#mod.py:52");

        // A call edge referencing either id resolves to that declaration's
        // own Function node: the id -> FQN map is injective.
        let unique: HashSet<&String> = m.values().collect();
        assert_eq!(unique.len(), decls.len(), "every FQN is distinct: {m:?}");

        // The distinct FQNs are exactly what keeps `claim` from panicking on
        // a same-kind collision.
        let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
        for (id, fqn) in &m {
            claim(&mut seen, id, fqn, NodeKind::Function);
        }
        assert_eq!(seen.len(), decls.len());
    }

    /// Phase-09 task-2 / task-40: `root_module_fqn` roots a frontend's
    /// verbatim dotted identity under the `lang_switch` language id, for
    /// every language — Rust's dotted module path, Python's package chain,
    /// TypeScript's npm-package + dot-path identity, Markdown's absolute
    /// directory path, Go's module path, and Java's package. Rooting is what
    /// makes two languages that both define a module identity `apg` distinct
    /// (`rust.apg` vs `py.apg`).
    ///
    /// fix-module-identity task-12: the identity is rendered repo-relative
    /// against the base first. A Markdown identity is an absolute directory
    /// path, so `/repo/docs` under base `/repo` renders `md.docs`; every
    /// other frontend identity is already relative and passes through.
    #[test]
    fn root_module_fqn_roots_every_language_identity() {
        let base = Path::new("/repo");
        assert_eq!(
            root_module_fqn("rust", "apg.ingest", base),
            "rust.apg.ingest"
        );
        assert_eq!(root_module_fqn("py", "pkg.sub", base), "py.pkg.sub");
        assert_eq!(root_module_fqn("ts", "@co/ui.src", base), "ts.@co/ui.src");
        assert_eq!(root_module_fqn("md", "/repo/docs", base), "md.docs");
        assert_eq!(
            root_module_fqn("go", "github.com/x/y", base),
            "go.github.com/x/y"
        );
        assert_eq!(root_module_fqn("java", "com.foo", base), "java.com.foo");
        // The empty base is the pass-through sentinel.
        assert_eq!(
            root_module_fqn("md", "/abs/docs", Path::new("")),
            "md./abs/docs"
        );
        // An empty parent is left empty — never the bare `<language>.` root.
        assert_eq!(rooted_scope("rust", "", base), "");
        assert_eq!(rooted_scope("rust", "apg", base), "rust.apg");
    }

    /// fix-module-identity task-2 / apg-0.17.0 phase-03 task-12:
    /// `repo_relative_identity` renders a scanner path relative to the
    /// git-toplevel base (the scan root is the non-git fallback),
    /// `/`-separated, with no leading `/` and no `..` segment. An
    /// already-relative frontend identity passes through; an absolute path
    /// under the base is stripped. A path resolving to the base ITSELF —
    /// the repo root — renders the EMPTY repo-relative identity (never the
    /// `file_name(base)` checkout basename), so `root_module_fqn` on it
    /// renders the bare `md.` root. A genuinely escaping/foreign path keeps
    /// the file-name fallback.
    #[test]
    fn repo_relative_identity_is_checkout_independent() {
        let base = Path::new("/repo");
        // Under the base: the tail, no leading slash.
        assert_eq!(
            repo_relative_identity(base, "/repo/src/load.rs"),
            "src/load.rs"
        );
        // The base itself resolves to the EMPTY repo-root identity — never
        // the checkout basename `repo`.
        assert_eq!(repo_relative_identity(base, "/repo"), "");
        assert_eq!(repo_relative_identity(base, ""), "");
        // Rooting that empty identity renders the bare `<language>.` root:
        // the repo-root Markdown module is `md.`, never `md.repo`.
        assert_eq!(root_module_fqn("md", "", base), "md.");
        assert_eq!(root_module_fqn("md", "/repo", base), "md.");
        assert_eq!(root_module_fqn("sh", "", base), "sh.");
        // A relative identity passes through unchanged.
        assert_eq!(repo_relative_identity(base, "apg.ingest"), "apg.ingest");
        assert_eq!(repo_relative_identity(base, "@co/ui.src"), "@co/ui.src");
        // Lexical normalisation: `.` dropped, `..` resolved.
        assert_eq!(
            repo_relative_identity(base, "/repo/a/./b/../c.rs"),
            "a/c.rs"
        );
        // No `..` can survive: an escaping relative path falls back to its
        // file name (the checkout component never leaks into an identity).
        assert_eq!(repo_relative_identity(base, "../etc/passwd"), "passwd");
        // A foreign absolute path outside the base also keeps the fallback.
        assert_eq!(repo_relative_identity(base, "/other/repo"), "repo");
        let out = repo_relative_identity(base, "/repo/../etc/passwd");
        assert!(!out.starts_with('/'), "no leading slash: {out}");
        assert!(!out.split('/').any(|c| c == ".."), "no .. segment: {out}");
        // The empty base is the pass-through sentinel.
        assert_eq!(
            repo_relative_identity(Path::new(""), "/abs/a.rs"),
            "/abs/a.rs"
        );
    }

    /// Phase-09 task-9 / task-40: a declaration whose parent is a module
    /// inherits the rooted module FQN through `parent.name` (the renderer
    /// only concatenates, so rooting the parent at record time roots the
    /// symbol). The shape rules — singleton, overload suffix, Go
    /// `init#<file>` — are unchanged.
    #[test]
    fn rooted_module_parent_yields_rooted_symbol_fqns() {
        let decls = [
            fd("n1", "rust.apg.ingest", "run", &[], "/x/a.rs"),
            fd("n2", "py.pkg.sub", "helper", &[], "/x/a.py"),
        ];
        let m = fqns(&decls);
        assert_eq!(m["n1"], "rust.apg.ingest.run");
        assert_eq!(m["n2"], "py.pkg.sub.helper");
        // Go `init` keeps its file disambiguator under the rooted parent.
        let inits = [
            fd("n3", "go.pkg", "init", &[], "/x/a.go"),
            fd("n4", "go.pkg", "init", &[], "/x/b.go"),
        ];
        let mi = fqns(&inits);
        assert_eq!(mi["n3"], "go.pkg.init#a.go");
        assert_eq!(mi["n4"], "go.pkg.init#b.go");
    }

    /// Phase-09 task-40: language rooting removes only the CROSS-language
    /// collision. A same-kind collision WITHIN one language still fails
    /// loudly in `claim` — the rooted FQNs are no different: two
    /// declarations rendering the same rooted FQN panic rather than silently
    /// overwriting.
    #[test]
    #[should_panic(expected = "FQN collision")]
    fn same_kind_claim_still_panics_under_rooting() {
        let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
        claim(&mut seen, "n1", "rust.pkg.F", NodeKind::Function);
        claim(&mut seen, "n2", "rust.pkg.F", NodeKind::Function);
    }

    /// apg-0.17.0 phase-01 task-20: the claim guard accepts the structural
    /// scanner's language-rooted identities — the bare per-format roots
    /// (`md.`/`sh.`/…/`misc.`), rooted submodules, and file-rooted
    /// declarations — without a false same-FQN collision. A re-claim by the
    /// same declaration id (a module's FQN is its id) is idempotent; the
    /// genuine same-kind collision still panics (pinned by
    /// `same_kind_claim_still_panics_under_rooting`).
    #[test]
    fn structural_identities_claim_without_false_collision() {
        let mut seen: HashMap<String, (String, NodeKind)> = HashMap::new();
        // The bare per-format roots are distinct module FQNs.
        for root in [
            "md.",
            "sh.",
            "yaml.",
            "json.",
            "toml.",
            "xml.",
            "dockerfile.",
            "makefile.",
            "ini.",
            "misc.",
        ] {
            claim(&mut seen, root, root, NodeKind::Module);
        }
        // A rooted submodule and file-rooted declarations coexist with
        // their stream root.
        claim(&mut seen, "md.docs", "md.docs", NodeKind::Module);
        claim(&mut seen, "n1", "md.docs/guide.md", NodeKind::Struct);
        claim(&mut seen, "n2", "md.AGENTS.md.title", NodeKind::Struct);
        // A re-claim by the same declaration id is idempotent.
        claim(&mut seen, "md.", "md.", NodeKind::Module);
        assert_eq!(seen.len(), 13);
    }

    /// The edge spool round-trips through an IN-MEMORY `Vec<u8>`/`Cursor`, not a
    /// file: the evidence listed it as "writes and re-reads a spool file", but
    /// its body performs no filesystem I/O, so by the law it is unit (the body
    /// wins over the evidence).
    #[test]
    fn edge_spool_roundtrip() {
        let edges = vec![
            Record::Contains {
                from: "n1".to_string(),
                to: "n2".to_string(),
            },
            Record::Calls {
                from: "n1".to_string(),
                to: "n2".to_string(),
            },
            Record::Uses {
                from: "n1".to_string(),
                to: "n2".to_string(),
            },
            Record::UnresolvedCall {
                from: "n1".to_string(),
                to: "java.lang.String.format".to_string(),
                target_type: String::new(),
            },
            Record::UnresolvedUse {
                from: "n1".to_string(),
                to: "java.util.List".to_string(),
            },
        ];
        let mut buf: Vec<u8> = Vec::new();
        for e in &edges {
            write_edge(&mut buf, e.clone());
        }
        let mut er = EdgeReader {
            r: std::io::Cursor::new(buf),
        };
        let mut out = Vec::new();
        while let Some(e) = er.next_edge() {
            out.push(e);
        }
        assert_eq!(out, edges);
    }
}

/// int tier -- two or more units wired together, pure in-memory (no
/// filesystem, database, git or process): the identity renderer chain.
mod int {
    use super::*;

    /// fix-module-identity task-16: the single identity base flows through
    /// the whole ingest-side render chain in memory —
    /// [`repo_relative_identity`] → [`root_module_fqn`] → [`rooted_scope`] →
    /// `parent.name` ([`render_function_fqns`]) — so a scanner record set
    /// renders checkout-independent module/File/symbol identities end to end.
    ///
    /// The spooling `ingest`/`ingest_records` entry point is e2e (it opens a
    /// `std::env::temp_dir()` spool file), so the assembly contract this
    /// task owns is exercised here on the pure renderer units it is built
    /// from.
    #[test]
    fn repo_relative_identity_flows_through_the_render_chain() {
        let base = Path::new("/repo");
        // A File's canonical identity is its repo-relative path, never the
        // absolute scanner path.
        assert_eq!(
            repo_relative_identity(base, "/repo/internal/store/store.go"),
            "internal/store/store.go"
        );
        // Its parent module identity is rooted off the same base; a relative
        // frontend identity passes through and is rooted verbatim.
        let module = root_module_fqn("go", "github.com/x/y", base);
        assert_eq!(module, "go.github.com/x/y");
        // A declaration under that module renders `parent.name` off the
        // rooted parent.
        let decls = [fd(
            "n1",
            &module,
            "Open",
            &["string"],
            "/repo/internal/store/store.go",
        )];
        assert_eq!(fqns(&decls)["n1"], "go.github.com/x/y.Open");
        // The same file rendered from two checkouts agrees: a Markdown
        // absolute module identity (`/repo/docs` and `/other/docs`) rebases
        // to the SAME repo-relative identity.
        assert_eq!(root_module_fqn("md", "/repo/docs", base), "md.docs");
        assert_eq!(
            root_module_fqn("md", "/other/docs", Path::new("/other")),
            "md.docs"
        );
        // An empty scope stays empty — never a bare `<language>.` root.
        assert_eq!(rooted_scope("go", "", base), "");
    }

    /// apg-0.17.0 phase-03 task-9: the structural streams' identity chain
    /// wires [`classify_code_type`] + [`root_module_fqn`] +
    /// [`render_function_fqns`] over every structural stream id — a
    /// structural module roots under its stream id (`md.`/`sh.`/`yaml.` at
    /// the repo root; `yaml.sub/dir` for a subdirectory), a declaration
    /// renders `parent.name` off the rooted parent, the code_type is
    /// `config` (`md` keeps `docs`), and an empty repo-root identity renders
    /// the bare `md.` root. Pure in-memory: two-plus units wired, no I/O.
    #[test]
    fn structural_streams_root_render_and_classify() {
        let base = Path::new("/repo");
        // The repo-root module of every structural stream is its bare root;
        // a subdirectory identity roots under the stream id.
        for lang in [
            "md",
            "sh",
            "yaml",
            "json",
            "toml",
            "xml",
            "dockerfile",
            "makefile",
            "ini",
            "misc",
        ] {
            assert_eq!(root_module_fqn(lang, "", base), format!("{lang}."));
            assert_eq!(
                root_module_fqn(lang, "sub/dir", base),
                format!("{lang}.sub/dir")
            );
        }
        // An absolute directory identity under the base renders repo-relative
        // and roots under its stream id (the repo base itself is the empty
        // repo-root identity → the bare root, never a checkout basename).
        assert_eq!(root_module_fqn("yaml", "/repo/ci", base), "yaml.ci");
        assert_eq!(root_module_fqn("md", "/repo", base), "md.");

        // A declaration under a rooted structural module renders
        // `parent.name`: a shell function in the repo-root `run.sh` renders
        // `sh.run.sh.hello` (the emitter file-roots the Struct, so the
        // parent is the rooted file identity).
        let sh_parent = root_module_fqn("sh", "/repo/run.sh", base);
        assert_eq!(sh_parent, "sh.run.sh");
        let decls = [fd("n1", &sh_parent, "hello", &[], "/repo/run.sh")];
        assert_eq!(fqns(&decls)["n1"], "sh.run.sh.hello");
        // The root module and the file-rooted declaration coexist.
        let md_parent = root_module_fqn("md", "/repo/README.md", base);
        let md_decls = [fd("n2", &md_parent, "intro", &[], "/repo/README.md")];
        assert_eq!(fqns(&md_decls)["n2"], "md.README.md.intro");

        // The code_type: non-md structural streams are `config`, Markdown
        // keeps its built-in `docs` (no config present).
        for lang in [
            "sh",
            "yaml",
            "json",
            "toml",
            "xml",
            "dockerfile",
            "makefile",
            "ini",
            "misc",
        ] {
            assert_eq!(
                crate::classify::classify_code_type("run.sh", "sh.run.sh", lang, None),
                "config",
                "{lang} must classify config"
            );
        }
        assert_eq!(
            crate::classify::classify_code_type("README.md", "md.README.md", "md", None),
            "docs",
            "Markdown keeps docs"
        );
        // With the init-style config present the structural decision is the
        // same (the config's structural `code_type` default).
        let cfg = crate::classify::ApgConfig {
            default: "src".to_string(),
            types: Vec::new(),
            structural: Some(crate::classify::StructuralScope::default()),
        };
        assert_eq!(
            crate::classify::classify_code_type("ci.yml", "yaml.ci.yml", "yaml", Some(&cfg)),
            "config"
        );
        assert_eq!(
            crate::classify::classify_code_type("README.md", "md.README.md", "md", Some(&cfg)),
            "docs"
        );
    }
}
