//! Source-path resolution, exclusion, keys, item locations and line indexing.

use hir::InFile;
use ide_db::base_db::SourceDatabase;
use syntax::ast::AstNode;
use vfs::{FileId, Vfs};

use crate::load::Ctx;

pub(crate) fn path_of(ctx: &Ctx<'_>, file_id: FileId) -> String {
    path_of_vfs(ctx.vfs, file_id)
}

pub(crate) fn path_of_vfs(vfs: &Vfs, file_id: FileId) -> String {
    vfs.file_path(file_id)
        .as_path()
        .map(|p| p.to_string())
        .unwrap_or_default()
}

pub(crate) fn path_excluded(path: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|p| path.contains(p.as_str()))
}

pub(crate) fn source_key<A: AstNode>(ctx: &Ctx<'_>, f: InFile<A>) -> Option<(String, u32)> {
    let hir::HirFileId::FileId(real) = f.file_id else {
        return None;
    };
    let fid = real.file_id(ctx.db);
    let path = path_of(ctx, fid);
    if path.is_empty() {
        return None;
    }
    Some((path, u32::from(f.value.syntax().text_range().start())))
}

/// A located item's span: `(path, start, end, start_line, end_line, src_key)`.
pub(crate) type ItemLoc = (String, u32, u32, u32, u32, (String, u32));

/// (path, start, end, start_line, end_line, src_key) from a real-file item's
/// span. Macro-generated items return `None` and are skipped.
pub(crate) fn item_loc<A: AstNode>(ctx: &Ctx<'_>, f: InFile<A>) -> Option<ItemLoc> {
    let hir::HirFileId::FileId(real) = f.file_id else {
        return None;
    };
    let fid = real.file_id(ctx.db);
    let path = path_of(ctx, fid);
    if path.is_empty() {
        return None;
    }
    let range = f.value.syntax().text_range();
    let text = ctx.db.file_text(fid).text(ctx.db).to_string();
    let li = LineIndex::new(&text);
    let start = u32::from(range.start());
    let end = u32::from(range.end());
    let sl = li.line(start);
    let el = li.line(end.saturating_sub(1));
    let key = (path.clone(), start);
    Some((path, start, end, sl, el, key))
}

pub(crate) fn line_count(text: &str) -> u32 {
    if text.is_empty() {
        return 1;
    }
    let n = text.bytes().filter(|b| *b == b'\n').count() as u32;
    if text.ends_with('\n') {
        n
    } else {
        n + 1
    }
}

pub(crate) struct LineIndex {
    starts: Vec<u32>,
}

impl LineIndex {
    pub(crate) fn new(text: &str) -> LineIndex {
        let mut starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i as u32 + 1);
            }
        }
        LineIndex { starts }
    }
    pub(crate) fn line(&self, offset: u32) -> u32 {
        match self.starts.binary_search(&offset) {
            Ok(l) => l as u32 + 1,
            Err(l) => l.max(1) as u32,
        }
    }
}
