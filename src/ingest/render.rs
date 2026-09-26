//! Function FQN rendering: the buffered declaration record and the overload /
//! Go-init / Python-duplicate renderer.

use std::collections::HashMap;

use super::identity::file_basename;

#[derive(Debug, Clone)]
pub(crate) struct FuncDecl {
    pub(crate) id: String,
    pub(crate) parent: String,
    pub(crate) name: String,
    pub(crate) params: Vec<String>,
    pub(crate) file: String,
    pub(crate) path: String,
    pub(crate) start: u32,
    pub(crate) end: u32,
    pub(crate) start_line: u32,
    pub(crate) end_line: u32,
    /// Language this declaration was scanned under (a `lang_switch` record may
    /// set it mid-stream when a scan covers multiple languages).
    pub(crate) language: String,
}

/// Renders the FQN of every function declaration (SPEC §4).
///
/// PHASE_09 language rooting: each `FuncDecl.parent` is already the ROOTED
/// scope FQN (the caller applies [`root_module_fqn`] to the frontend's raw
/// parent before buffering — see `ingest_records`), so `parent.name` inherits
/// the language root for every declaration whose parent is a module (e.g.
/// `rust.apg.ingest.foo`). The suffix/shape rules below are unchanged.
///
/// Declarations are grouped by `(parent, name)`: a singleton group renders
/// `parent.name`, an overloaded group renders `parent.name(T1,T2,...)` for every
/// member. Go `init` functions carry no signature, so each is rendered
/// `parent.init#<file-basename>` instead. The per-declaration language drives
/// the Go `init` special case (multi-language scans mix languages in one
/// buffer).
///
/// The `py` stream additionally needs same-scope duplicate-name disambiguation
/// (Python `@overload` stubs whose annotations erase identically, and a
/// conditional redefinition): within a colliding subgroup — members erasing to
/// the SAME param list, including the both-empty `()` case — every member
/// renders the full form `parent.name(T1,T2,...)#<file-basename>:<start_line>`,
/// retaining the erased param-list suffix. Every non-py stream is unchanged.
pub(crate) fn render_function_fqns(decls: &[FuncDecl]) -> Vec<(String, String)> {
    let mut groups: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, d) in decls.iter().enumerate() {
        groups
            .entry((d.parent.as_str(), d.name.as_str()))
            .or_default()
            .push(i);
    }

    let mut out = Vec::with_capacity(decls.len());
    for ((parent, name), idxs) in groups {
        if name == "init" && idxs.iter().any(|&i| decls[i].language == "go") {
            for i in idxs {
                out.push((
                    decls[i].id.clone(),
                    format!("{parent}.init#{}", file_basename(&decls[i].file)),
                ));
            }
        } else if idxs.len() == 1 {
            let d = &decls[idxs[0]];
            out.push((d.id.clone(), format!("{parent}.{name}")));
        } else if idxs.iter().all(|&i| decls[i].language == "py") {
            // Python same-scope duplicate-name rule, scoped to the `py` stream
            // (every non-py stream keeps the existing overload shape). Bucket
            // the group by erased param list: a subgroup of more than one
            // member cannot share `parent.name(T1,T2,...)`, so each colliding
            // member renders the full form with its retained param-list suffix
            // plus the `#<file-basename>:<start_line>` disambiguator. A member
            // whose erased param list is unique in the group keeps the bare
            // overload form.
            let mut buckets: Vec<(String, Vec<usize>)> = Vec::new();
            for &i in &idxs {
                let key = decls[i].params.join(",");
                match buckets.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push(i),
                    None => buckets.push((key, vec![i])),
                }
            }
            for (params, members) in buckets {
                if members.len() > 1 {
                    for i in members {
                        let d = &decls[i];
                        out.push((
                            d.id.clone(),
                            format!(
                                "{parent}.{name}({params})#{}:{}",
                                file_basename(&d.file),
                                d.start_line
                            ),
                        ));
                    }
                } else {
                    let d = &decls[members[0]];
                    out.push((
                        d.id.clone(),
                        format!("{parent}.{name}({})", d.params.join(",")),
                    ));
                }
            }
        } else {
            for i in idxs {
                let d = &decls[i];
                out.push((
                    d.id.clone(),
                    format!("{parent}.{name}({})", d.params.join(",")),
                ));
            }
        }
    }
    out
}
