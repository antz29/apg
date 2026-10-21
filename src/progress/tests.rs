use super::*;

/// Drive a `Progress` over an in-memory writer and return everything it
/// emitted as text.
fn render(gate: bool, run: impl FnOnce(&mut Progress<'_>)) -> String {
    let mut out = Vec::new();
    {
        let mut progress = Progress::new(gate, &mut out);
        run(&mut progress);
    }
    String::from_utf8(out).expect("progress output is UTF-8")
}

/// unit tier -- pure in-memory: no filesystem, database, git or process.
mod unit {
    use super::*;

    #[test]
    fn symbol_maps_each_code_kind_and_rejects_others() {
        for (kind, symbol) in [
            (NodeKind::Language, "🌍"),
            (NodeKind::Module, "📦"),
            (NodeKind::File, "📄"),
            (NodeKind::Struct, "🧱"),
            (NodeKind::Function, "⚙️"),
        ] {
            assert_eq!(symbol_for(kind), Some(symbol), "{kind:?}");
        }
        for kind in [
            NodeKind::UnresolvedTarget,
            NodeKind::Requirement,
            NodeKind::Note,
            NodeKind::Feedback,
            NodeKind::Plan,
            NodeKind::PlanPhase,
            NodeKind::Task,
            NodeKind::Scan,
        ] {
            assert_eq!(symbol_for(kind), None, "{kind:?}");
        }
    }

    #[test]
    fn start_emits_the_start_line() {
        assert_eq!(
            render(true, |progress| progress.start()),
            "Scan running...\n"
        );
    }

    #[test]
    fn node_emits_one_symbol_per_code_kind_and_nothing_else() {
        let rendered = render(true, |progress| {
            progress.node(NodeKind::Module);
            progress.node(NodeKind::Module);
            progress.node(NodeKind::Function);
            progress.node(NodeKind::Requirement);
        });
        assert_eq!(rendered, "📦📦⚙️");
    }

    #[test]
    fn summary_tallies_each_kind_and_carries_the_legend() {
        let rendered = render(true, |progress| {
            progress.node(NodeKind::Language);
            progress.node(NodeKind::Module);
            progress.node(NodeKind::Module);
            for _ in 0..3 {
                progress.node(NodeKind::File);
            }
            for _ in 0..4 {
                progress.node(NodeKind::Struct);
            }
            for _ in 0..5 {
                progress.node(NodeKind::Function);
            }
            progress.summary();
        });
        assert_eq!(
            rendered,
            "🌍📦📦📄📄📄🧱🧱🧱🧱⚙️⚙️⚙️⚙️⚙️\n🌍 Language: 1, 📦 Module: 2, 📄 File: 3, 🧱 Struct: 4, ⚙️ Function: 5\n"
        );
    }

    #[test]
    fn summary_tallies_each_language_in_scan_order() {
        let rendered = render(true, |progress| {
            progress.language("rust");
            progress.node(NodeKind::Module);
            progress.node(NodeKind::Module);
            progress.node(NodeKind::File);
            progress.language("go");
            progress.node(NodeKind::Module);
            progress.language("rust");
            progress.node(NodeKind::Function);
            progress.summary();
        });
        assert_eq!(
            rendered,
            "📦📦📄📦⚙️\n🌍 Language: 0, 📦 Module: 3, 📄 File: 1, 🧱 Struct: 0, ⚙️ Function: 1 | rust: 4, go: 1\n"
        );
    }

    #[test]
    fn disabled_progress_emits_nothing() {
        let rendered = render(false, |progress| {
            progress.start();
            progress.language("rust");
            progress.node(NodeKind::Function);
            progress.summary();
        });
        assert!(rendered.is_empty(), "{rendered:?}");
    }
}
