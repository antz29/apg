//! Unified-schema record types (SPEC §2) and per-declaration node serialization.

#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Rec {
    Module {
        fqn: String,
    },
    File {
        path: String,
        parent: String,
        start_line: u32,
        end_line: u32,
    },
    Struct {
        id: String,
        parent: String,
        name: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },
    Function {
        id: String,
        parent: String,
        name: String,
        params: Vec<String>,
        file: String,
        path: String,
        start: u32,
        end: u32,
        start_line: u32,
        end_line: u32,
    },
    Unresolved {
        fqn: String,
        category: Option<String>,
    },
    Contains {
        from: String,
        to: String,
    },
    Calls {
        from: String,
        to: String,
    },
    Uses {
        from: String,
        to: String,
    },
    UnresolvedCall {
        from: String,
        to: String,
        #[serde(default)]
        target_type: String,
    },
    UnresolvedUse {
        from: String,
        to: String,
    },
}

#[derive(Clone)]
pub(crate) struct Decl {
    pub(crate) kind: &'static str,
    pub(crate) id: String,
    pub(crate) parent: String,
    pub(crate) name: String,
    pub(crate) params: Vec<String>,
    pub(crate) path: String,
    pub(crate) file: String,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    pub(crate) src_key: (String, u32),
    /// Whether this declaration belongs to a crate selected for emission
    /// (phase-05 task-8). With no `--targets` filter every declaration is
    /// emitted, so the stream is byte-identical to a full scan.
    pub(crate) emit: bool,
}

pub(crate) struct ImplEdge {
    pub(crate) self_fqn: String,
    pub(crate) trait_fqn: String,
}

pub(crate) fn node_record(d: &Decl) -> String {
    let rec: Rec = match d.kind {
        "struct" => Rec::Struct {
            id: d.id.clone(),
            parent: d.parent.clone(),
            name: d.name.clone(),
            path: d.path.clone(),
            start: d.start,
            end: d.end,
            start_line: d.start_line,
            end_line: d.end_line,
        },
        _ => Rec::Function {
            id: d.id.clone(),
            parent: d.parent.clone(),
            name: d.name.clone(),
            params: d.params.clone(),
            file: d.file.clone(),
            path: d.path.clone(),
            start: d.start,
            end: d.end,
            start_line: d.start_line,
            end_line: d.end_line,
        },
    };
    serde_json::to_string(&rec).unwrap()
}
