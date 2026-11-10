//! csharp-projects (e2e): the C# frontend's project-aware and Razor scan
//! tier, driving the staged `csharpfrontend` / the CANDIDATE binary
//! (`testutil::apg_bin`) against scratch `/tmp` trees only — never the apg
//! repo. Every test is `#[ignore]`d and runs through `cargo test-e2e`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

/// A `.cshtml` view with an `@functions` method: `Greet` is named on line 3.
/// No trailing newline, so its line count is `split('\n').count()` (7).
const VIEW_CSHTML: &str = "<h1>Home</h1>\n@functions {\n    public string Greet()\n    {\n        return \"hi\";\n    }\n}";

/// A `.razor` component with an `@code` method: `Increment` is named on
/// line 6. No trailing newline (10 lines).
const COUNTER_RAZOR: &str = "<p>@Count</p>\n<button>Go</button>\n@code {\n    private int Count;\n\n    private void Increment()\n    {\n        Count++;\n    }\n}";

/// A `.razor` component whose `@code {` block never closes: it fails to compile.
const BROKEN_RAZOR: &str = "<p>broken</p>\n@code {\n    private void Broken()\n    {";

/// The staged single-file `csharpfrontend` the candidate runs
/// (`<profile>/frontends/csharpfrontend`), resolved from
/// `testutil::apg_bin()`. Fails loudly naming the build when it is absent.
fn csharp_frontend_bin() -> PathBuf {
    let dir = common::apg_bin()
        .parent()
        .expect("apg binary has a parent")
        .join("frontends");
    let bin = ["csharpfrontend", "csharpfrontend.exe"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file());
    bin.unwrap_or_else(|| {
        panic!(
            "csharpfrontend not staged in {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"csharp\"'",
            dir.display()
        )
    })
}

/// A fresh scratch directory under the system temp dir (never the apg repo).
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("apg-csharp-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Writes `text` at `dir/name` and returns its absolute path as a string.
fn write(dir: &Path, name: &str, text: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, text).expect("write fixture");
    p.to_str().expect("utf-8 path").to_string()
}

/// Runs `csharpfrontend <dir> --razor <p>...`.
fn run_razor(dir: &Path, razor: &[&str]) -> Output {
    let bin = csharp_frontend_bin();
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg(dir);
    for p in razor {
        cmd.arg("--razor").arg(p);
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", bin.display()))
}

mod e2e {
    use super::*;

    /// Phase 1 task 5's pinned CLI contract: each `--razor` file yields a
    /// `file` record spanning `1..line count` and a `function` record per
    /// `@code`/`@functions` method at its `#line`-mapped source line; a file
    /// that fails to compile exits 1 with a `csharpfrontend error:` line.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch tree, spawned csharpfrontend); run via cargo test-e2e"]
    fn razor_engine_single_file_smoke() {
        let dir = scratch("razor");
        let view = write(&dir, "Index.cshtml", VIEW_CSHTML);
        let counter = write(&dir, "Counter.razor", COUNTER_RAZOR);

        let out = run_razor(&dir, &[&view, &counter]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(0),
            "csharpfrontend failed: {stderr}"
        );
        let records: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad record `{l}`: {e}")))
            .collect();

        // Line count convention: the fixtures carry no trailing newline, so
        // Roslyn's SourceText line count (line breaks + 1) equals
        // `split('\n').count()`.
        for (path, text, method, line) in [
            (&view, VIEW_CSHTML, "Greet", 3),
            (&counter, COUNTER_RAZOR, "Increment", 6),
        ] {
            let files: Vec<_> = records
                .iter()
                .filter(|r| r["type"] == "file" && r["path"] == path.as_str())
                .collect();
            assert_eq!(files.len(), 1, "one file record for {path}: {records:#?}");
            assert_eq!(files[0]["start_line"], 1, "{path}");
            assert_eq!(
                files[0]["end_line"],
                text.split('\n').count(),
                "end_line is the line count of {path}"
            );

            let func = records
                .iter()
                .find(|r| {
                    r["type"] == "function" && r["name"] == method && r["path"] == path.as_str()
                })
                .unwrap_or_else(|| panic!("no function {method} in {path}: {records:#?}"));
            assert_eq!(
                func["start_line"], line,
                "{method} start_line is its #line-mapped line in {path}"
            );
        }

        let broken = write(&dir, "Broken.razor", BROKEN_RAZOR);
        let out = run_razor(&dir, &[&broken]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(1),
            "broken Razor must exit 1: {stderr}"
        );
        assert!(
            stderr
                .lines()
                .any(|l| l.starts_with("csharpfrontend error:")),
            "missing `csharpfrontend error:` line: {stderr}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
