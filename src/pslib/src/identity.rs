//! Whole-tree naming and FQN disambiguation: every record's emitted name and
//! parent key, computed over the whole claimed tree before any `--targets`
//! filtering. Keys are psfrontend's unrooted identities (the ingestor renders
//! `ps.` + key).

use std::collections::{BTreeSet, HashMap};

use crate::manifest::ModuleMap;
use crate::units::{FileUnits, UnitKind};

/// `path` relative to the directory `base`, `/`-joined, with `..` segments
/// when `path` sits outside `base`.
pub fn base_relative(path: &str, base: &str) -> String {
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let b: Vec<&str> = base.split('/').filter(|s| !s.is_empty()).collect();
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = vec![".."; b.len() - common];
    out.extend(&p[common..]);
    out.join("/")
}

/// An overloaded member's name carries its `(T1,T2)` signature.
pub fn emitted_name(name: &str, params: &[String], overloaded: bool) -> String {
    if overloaded {
        format!("{name}({})", params.join(","))
    } else {
        name.to_string()
    }
}

/// The key of a record named `name` under the scope key `parent`.
pub fn record_key(parent: &str, name: &str) -> String {
    format!("{parent}.{name}")
}

/// The key of a unit `name` under `scope`.
pub fn render(scope: &str, name: &str, params: &[String], overloaded: bool) -> String {
    record_key(scope, &emitted_name(name, params, overloaded))
}

/// Whether `fqn` ends in `//`, a scanned path, `:` and one or more digits.
pub fn is_disambiguated(fqn: &str, paths: &BTreeSet<String>) -> bool {
    let Some((head, digits)) = fqn.rsplit_once(':') else {
        return false;
    };
    !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && paths.iter().any(|p| {
            head.strip_suffix(p.as_str())
                .is_some_and(|h| h.ends_with("//"))
        })
}

/// One script file's naming: its module, script Struct name and per-unit
/// (parent key, emitted name).
#[derive(Debug)]
struct FileIdentity {
    module: String,
    script: String,
    units: Vec<(String, String)>,
}

/// The whole tree's resolved names and keys.
#[derive(Debug)]
pub struct Identity {
    files: HashMap<String, FileIdentity>,
    keys: Vec<String>,
}

fn is_script(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    matches!(ext.as_deref(), Some("ps1" | "psm1"))
}

fn counts<'a>(keys: impl Iterator<Item = &'a String>) -> HashMap<&'a str, usize> {
    let mut n = HashMap::new();
    for k in keys {
        *n.entry(k.as_str()).or_insert(0) += 1;
    }
    n
}

/// Names every script, type and function of `files` (`paths` = every claimed
/// `.ps1`/`.psm1`/`.psd1` path).
pub fn resolve(map: &ModuleMap, files: &[FileUnits], paths: &BTreeSet<String>) -> Identity {
    let modules: BTreeSet<String> = map.modules().into_iter().collect();
    let files: Vec<&FileUnits> = files.iter().filter(|f| is_script(&f.path)).collect();
    let suffix = |path: &str, ordinal: usize| format!("//{path}:{ordinal}");

    // (1) Script Structs.
    let first: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let module = map.module(&f.path);
            let name = base_relative(&f.path, &map.base(&f.path));
            (module, name)
        })
        .collect();
    let first_keys: Vec<String> = first.iter().map(|(m, n)| record_key(m, n)).collect();
    let n = counts(first_keys.iter());
    let scripts: Vec<(String, String)> = files
        .iter()
        .zip(first)
        .zip(&first_keys)
        .map(|((f, (module, name)), key)| {
            if n[key.as_str()] > 1 || modules.contains(key) {
                (module, format!("{name}//{}", f.path))
            } else {
                (module, name)
            }
        })
        .collect();
    let script_keys: Vec<String> = scripts.iter().map(|(m, n)| record_key(m, n)).collect();

    // (2) Class and enum Structs under their final script key.
    let is_type = |k: UnitKind| matches!(k, UnitKind::Class | UnitKind::Enum);
    let type_keys: Vec<Vec<Option<String>>> = files
        .iter()
        .zip(&script_keys)
        .map(|(f, sk)| {
            f.units
                .iter()
                .map(|u| is_type(u.kind).then(|| record_key(sk, &u.name)))
                .collect()
        })
        .collect();
    let n = counts(
        script_keys
            .iter()
            .chain(type_keys.iter().flatten().flatten()),
    );
    let mut names: Vec<Vec<String>> = files
        .iter()
        .zip(&type_keys)
        .map(|(f, keys)| {
            f.units
                .iter()
                .zip(keys)
                .map(|(u, key)| match key {
                    Some(k)
                        if n[k.as_str()] > 1
                            || modules.contains(k)
                            || is_disambiguated(k, paths) =>
                    {
                        u.name.clone() + &suffix(&f.path, u.ordinal)
                    }
                    _ => u.name.clone(),
                })
                .collect()
        })
        .collect();
    let mut struct_keys: BTreeSet<String> = script_keys.iter().cloned().collect();

    // (3) Functions under their Struct's final key.
    let parents: Vec<Vec<String>> = files
        .iter()
        .zip(&script_keys)
        .zip(&names)
        .map(|((f, sk), nm)| {
            f.units
                .iter()
                .map(|u| match u.owner {
                    Some(o) => record_key(sk, &nm[o]),
                    None => sk.clone(),
                })
                .collect()
        })
        .collect();
    for ((f, ps), nm) in files.iter().zip(&parents).zip(&names) {
        for ((u, p), name) in f.units.iter().zip(ps).zip(nm) {
            if is_type(u.kind) {
                struct_keys.insert(record_key(p, name));
            }
        }
    }
    let fn_keys: Vec<Vec<Option<String>>> = files
        .iter()
        .zip(&parents)
        .map(|(f, ps)| {
            f.units
                .iter()
                .zip(ps)
                .map(|(u, p)| {
                    (!is_type(u.kind)).then(|| render(p, &u.name, &u.params, u.overloaded))
                })
                .collect()
        })
        .collect();
    let n = counts(fn_keys.iter().flatten().flatten());
    for ((f, keys), nm) in files.iter().zip(&fn_keys).zip(&mut names) {
        for ((u, key), name) in f.units.iter().zip(keys).zip(nm.iter_mut()) {
            let Some(k) = key else { continue };
            *name = emitted_name(&u.name, &u.params, u.overloaded);
            if n[k.as_str()] > 1
                || struct_keys.contains(k)
                || modules.contains(k)
                || is_disambiguated(k, paths)
            {
                *name += &suffix(&f.path, u.ordinal);
            }
        }
    }

    let mut keys: Vec<String> = modules.into_iter().collect();
    keys.extend(script_keys);
    let mut out = HashMap::new();
    for (((f, (module, script)), ps), nm) in files.iter().zip(scripts).zip(parents).zip(names) {
        let units: Vec<(String, String)> = ps.into_iter().zip(nm).collect();
        keys.extend(units.iter().map(|(p, n)| record_key(p, n)));
        out.insert(
            f.path.clone(),
            FileIdentity {
                module,
                script,
                units,
            },
        );
    }
    Identity { files: out, keys }
}

impl Identity {
    /// The script Struct's name of `path`, if it is a scanned script.
    pub fn script_name(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(|f| f.script.as_str())
    }

    /// The emitted name of unit `index` of `path`.
    pub fn unit_name(&self, path: &str, index: usize) -> &str {
        &self.files[path].units[index].1
    }

    /// The scope key of `path`'s script Struct: its module identity.
    pub fn script_parent(&self, path: &str) -> String {
        self.files[path].module.clone()
    }

    /// The scope key of unit `index` of `path`: its script's or class's key.
    pub fn unit_parent(&self, path: &str, index: usize) -> String {
        self.files[path].units[index].0.clone()
    }

    /// Every Module, Struct and Function key.
    pub fn keys(&self) -> Vec<String> {
        self.keys.clone()
    }
}
