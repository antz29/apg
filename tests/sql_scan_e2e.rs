//! sql-test-suite (e2e): `apg scan` over a T-SQL scratch repo — a legacy
//! SSDT project, an SDK-style project and loose scripts — driven through the
//! staged `sqlfrontend` and the CANDIDATE binary (`testutil::apg_bin`), never
//! the apg repo. Every test is `#[ignore]`d and runs through `cargo test-e2e`.

mod common;

use apg::layers::{properties_from_json, properties_json};
use common::{ApgCommand, Repo, apg_bin, git2_repo};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

/// A legacy SSDT project: default schema `app`, an Import of a targets file
/// that does not exist, wildcard Build items and a PostDeploy script.
const LEGACY_SQLPROJ: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<Project DefaultTargets="Build" ToolsVersion="4.0" xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
  <PropertyGroup>
    <Name>Legacy</Name>
    <DefaultSchema>app</DefaultSchema>
  </PropertyGroup>
  <Import Project="$(MSBuildExtensionsPath)\Microsoft\VisualStudio\v11.0\SSDT\Microsoft.Data.Tools.Schema.SqlTasks.targets" />
  <ItemGroup>
    <Build Include="Tables\*.sql" />
    <Build Include="Procs\*.sql" />
  </ItemGroup>
  <ItemGroup>
    <PostDeploy Include="Scripts\Script.PostDeployment.sql" />
  </ItemGroup>
</Project>
"#;

/// One-part `CREATE TABLE Orders` (schema `app` by default): `Id` on line 5,
/// `Total` on line 6.
const ORDERS_SQL: &str = ":setvar Env dev\nGO\nCREATE TABLE Orders\n(\n    Id INT NOT NULL,\n    Total DECIMAL(10, 2) NULL\n)\nGO\n";

/// Semicolon-free `app.GetOrders` spanning lines 1..9: reads and INSERTs
/// Orders, EXECs app.Helper and dbo.Missing, calls GETDATE().
const GET_ORDERS_SQL: &str = "CREATE PROCEDURE app.GetOrders\nAS\nBEGIN\n    SELECT Id, Total FROM Orders\n    INSERT INTO Orders (Id, Total) VALUES (1, 2)\n    EXEC app.Helper\n    SELECT GETDATE()\n    EXEC dbo.Missing\nEND\nGO\n";

const HELPER_SQL: &str = "CREATE PROCEDURE app.Helper\nAS\n    SELECT 1\nGO\n";

/// An SDK-style project with no DefaultSchema (so `dbo`).
const SDK_SQLPROJ: &str = "<Project Sdk=\"Microsoft.Build.Sql\">\n  <PropertyGroup>\n    <Name>Sdk</Name>\n  </PropertyGroup>\n</Project>\n";

const CUSTOMERS_SQL: &str =
    "CREATE TABLE dbo.Customers\n(\n    Id INT NOT NULL,\n    Name NVARCHAR(50) NULL\n)\nGO\n";

const V_CUSTOMERS_SQL: &str =
    "CREATE VIEW dbo.vCustomers\nAS\n    SELECT Id, Name FROM dbo.Customers\nGO\n";

const SEED_SQL: &str = "INSERT INTO dbo.Customers (Id, Name) VALUES (1, N'a')\nGO\n";

/// One broken batch (line 3) between two valid ones: `dbo.Good1` (lines 1..1)
/// and `dbo.Good2` (lines 5..7).
const BROKEN_SQL: &str = "CREATE TABLE dbo.Good1 (Id INT)\nGO\nCREATE TABLE dbo.Bad (Id INT,,\nGO\nCREATE PROCEDURE dbo.Good2\nAS\n    SELECT 1\nGO\n";

/// The entries of the scan log's `Languages:` line.
fn languages(log: &str) -> Vec<String> {
    let line = log
        .lines()
        .find_map(|l| l.strip_prefix("Languages: "))
        .unwrap_or_else(|| panic!("no Languages: line: {log}"));
    line.split(", ").map(str::to_string).collect()
}

/// The staged `sqlfrontend` the candidate runs (`<profile>/frontends`),
/// resolved from `apg_bin()`. Fails loudly naming the build when absent.
fn sql_frontend_bin() -> PathBuf {
    let dir = apg_bin()
        .parent()
        .expect("apg binary has a parent")
        .join("frontends");
    let bin = ["sqlfrontend", "sqlfrontend.exe"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file());
    bin.unwrap_or_else(|| {
        panic!(
            "sqlfrontend not staged in {} — build it first: \
             cargo build --config 'env.APG_BUILD_FRONTENDS=\"sql,struct\"'",
            dir.display()
        )
    })
}

/// A scratch repo holding the self-written SQL fixtures, committed.
fn sql_repo(tag: &str) -> Repo {
    let repo = Repo::new(tag);
    repo.write("Legacy/Legacy.sqlproj", LEGACY_SQLPROJ);
    repo.write("Legacy/Tables/Orders.sql", ORDERS_SQL);
    repo.write("Legacy/Procs/GetOrders.sql", GET_ORDERS_SQL);
    repo.write("Legacy/Procs/Helper.sql", HELPER_SQL);
    repo.write("Sdk/Sdk.sqlproj", SDK_SQLPROJ);
    repo.write("Sdk/dbo/Customers.sql", CUSTOMERS_SQL);
    repo.write("Sdk/dbo/vCustomers.sql", V_CUSTOMERS_SQL);
    repo.write("scripts/seed.sql", SEED_SQL);
    repo.write("README.md", "# scratch\n");
    repo.commit_all("fixture");
    repo
}

/// A per-repo scratch HOME (`<repo root>-home`, a sibling under the temp dir).
fn home(repo: &Repo) -> String {
    let h = format!("{}-home", repo.root.display());
    std::fs::create_dir_all(&h).expect("create scratch HOME");
    h
}

/// Runs the candidate `apg scan . <args>` in `repo` and returns the output
/// plus its stderr text (the scan log).
fn scan(repo: &Repo, args: &[&str]) -> (Output, String) {
    let mut full = vec!["scan", "."];
    full.extend_from_slice(args);
    let out = ApgCommand::new(&full)
        .cwd(&repo.root)
        .env("HOME", &home(repo))
        .output();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    (out, log)
}

/// Runs a read-only Cypher through the candidate `apg query --json`.
fn query_rows(repo: &Repo, cypher: &str) -> Vec<serde_json::Value> {
    let out = ApgCommand::new(&["query", "--json", cypher])
        .cwd(&repo.root)
        .env("HOME", &home(repo))
        .output();
    assert!(
        out.status.success(),
        "query {cypher}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json rows")
}

/// The string values of column `col` across `rows`.
fn column(rows: &[serde_json::Value], col: &str) -> Vec<String> {
    rows.iter()
        .map(|r| r[col].as_str().unwrap_or_default().to_string())
        .collect()
}

/// The sorted SQL fact set: sql.-rooted modules, the files they contain, the
/// sql. declarations with lines and code_type, every Contains/Calls/Uses edge
/// with a sql endpoint and every unresolved edge out of a sql node.
fn sql_facts(repo: &Repo) -> BTreeSet<String> {
    const SQL: &str = "(a.fqn = 'sql' OR a.fqn STARTS WITH 'sql.' OR b.fqn STARTS WITH 'sql.')";
    let mut queries = vec![
        "MATCH (m:Module) WHERE m.fqn STARTS WITH 'sql.' RETURN m.fqn".to_string(),
        "MATCH (m:Module)-[:Contains]->(f:File) WHERE m.fqn STARTS WITH 'sql.' RETURN f.fqn, f.start_line, f.end_line, f.code_type".to_string(),
    ];
    for label in ["Struct", "Function"] {
        queries.push(format!(
            "MATCH (n:{label}) WHERE n.fqn STARTS WITH 'sql.' RETURN label(n) AS l, n.fqn, n.start_line, n.end_line, n.code_type"
        ));
    }
    for rel in ["Contains", "Calls", "Uses"] {
        queries.push(format!(
            "MATCH (a)-[r:{rel}]->(b) WHERE {SQL} RETURN '{rel}' AS k, a.fqn, b.fqn, r.properties"
        ));
    }
    for rel in ["UnresolvedCall", "UnresolvedUse"] {
        queries.push(format!(
            "MATCH (a)-[:{rel}]->(u:UnresolvedTarget) WHERE a.fqn STARTS WITH 'sql.' RETURN '{rel}' AS k, a.fqn, u.fqn, u.category"
        ));
    }
    queries
        .iter()
        .flat_map(|q| query_rows(repo, q))
        .map(|mut row| {
            // The splice leaves the column unset only on Contains/Calls/UnresolvedCall/
            // UnresolvedUse edges, where a full load writes `{}`; Uses rows carry the
            // full load's rendering (`{}` when absent). Unset and `{}` both decode to
            // the empty map, so compare the canonical encoding.
            if let Some(p) = row.get_mut("r.properties") {
                let raw = p.as_str().or(p.is_null().then_some("")).expect("string");
                *p = properties_json(&properties_from_json(raw).expect("properties")).into();
            }
            row.to_string()
        })
        .collect()
}

/// Asserts two fact sets are equal, printing their symmetric difference.
fn assert_facts_eq(got: &BTreeSet<String>, want: &BTreeSet<String>, ctx: &str) {
    let only_got: Vec<_> = got.difference(want).collect();
    let only_want: Vec<_> = want.difference(got).collect();
    assert!(
        only_got.is_empty() && only_want.is_empty(),
        "{ctx}: facts differ\nincremental only: {only_got:#?}\nfull only: {only_want:#?}"
    );
}

/// A plain incremental `--language sql` scan (asserting the delta path, no
/// sql failure and — when `sql_runs` — the sql frontend re-run) followed by a
/// `--force` full scan; returns both fact sets.
fn incremental_then_full(repo: &Repo, sql_runs: bool) -> (BTreeSet<String>, BTreeSet<String>) {
    let (out, log) = scan(repo, &["--language", "sql"]);
    assert!(out.status.success(), "incremental scan: {log}");
    assert!(log.contains("[scan] incremental:"), "{log}");
    assert!(!log.contains("sql frontend failed"), "{log}");
    if sql_runs {
        assert!(log.contains("running sql frontend"), "{log}");
    }
    let inc = sql_facts(repo);
    let (out, log) = scan(repo, &["--language", "sql", "--force"]);
    assert!(out.status.success(), "full scan: {log}");
    assert!(!log.contains("sql frontend failed"), "{log}");
    (inc, sql_facts(repo))
}

/// The File/Struct/Function fqns graphed at `path`.
fn graphed_at(repo: &Repo, path: &str) -> Vec<String> {
    [
        format!("MATCH (n:File) WHERE n.fqn = '{path}' RETURN n.fqn"),
        format!("MATCH (n:Struct) WHERE n.path = '{path}' RETURN n.fqn"),
        format!("MATCH (n:Function) WHERE n.path = '{path}' RETURN n.fqn"),
    ]
    .iter()
    .flat_map(|q| column(&query_rows(repo, q), "n.fqn"))
    .collect()
}

mod e2e {
    use super::*;

    /// A full `--language sql` scan graphs the project/schema/loose modules,
    /// the files, declarations with their fixture lines, and the
    /// calls/uses/unresolved edges the fixtures spell.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn full_scan_graph_shape() {
        sql_frontend_bin(); // fail loudly when the frontend is not staged
        let repo = sql_repo("sql-shape");
        let (out, log) = scan(&repo, &["--language", "sql"]);
        assert!(out.status.success(), "scan: {log}");
        assert!(!log.contains("sql frontend failed"), "{log}");

        let (legacy, sdk) = ("sql.Legacy/Legacy.sqlproj", "sql.Sdk/Sdk.sqlproj");
        let q = |c: &str, col: &str| column(&query_rows(&repo, c), col);

        let rooted = q(
            "MATCH (l:Language)-[:Contains]->(m:Module) WHERE l.fqn = 'sql' RETURN m.fqn",
            "m.fqn",
        );
        for m in [legacy, sdk, "sql.scripts"] {
            assert!(
                rooted.contains(&m.to_string()),
                "sql Language must contain {m}: {rooted:?}"
            );
        }
        for (parent, schema) in [(legacy, "app"), (sdk, "dbo")] {
            assert_eq!(
                q(
                    &format!(
                        "MATCH (p:Module)-[:Contains]->(m:Module) WHERE p.fqn = '{parent}' RETURN m.fqn"
                    ),
                    "m.fqn"
                ),
                vec![format!("{parent}.{schema}")],
                "schema module under {parent}"
            );
        }
        for (module, file) in [
            (legacy, "Legacy/Tables/Orders.sql"),
            (legacy, "Legacy/Procs/GetOrders.sql"),
            ("sql.scripts", "scripts/seed.sql"),
        ] {
            assert_eq!(
                q(
                    &format!(
                        "MATCH (m:Module)-[:Contains]->(f:File) WHERE f.fqn = '{file}' RETURN m.fqn"
                    ),
                    "m.fqn"
                ),
                vec![module.to_string()],
                "{file} module"
            );
        }

        let orders = format!("{legacy}.app.Orders");
        let cols = query_rows(
            &repo,
            &format!(
                "MATCH (t:Struct)-[:Contains]->(c:Struct) WHERE t.fqn = '{orders}' RETURN c.fqn, CAST(c.start_line AS STRING) AS line ORDER BY c.fqn"
            ),
        );
        assert_eq!(
            column(&cols, "c.fqn"),
            vec![format!("{orders}.Id"), format!("{orders}.Total")],
            "Orders columns"
        );
        assert_eq!(column(&cols, "line"), vec!["5", "6"], "column lines");

        let get_orders = format!("{legacy}.app.GetOrders");
        let f = query_rows(
            &repo,
            &format!(
                "MATCH (f:Function) WHERE f.fqn = '{get_orders}' RETURN CAST(f.start_line AS STRING) AS s, CAST(f.end_line AS STRING) AS e"
            ),
        );
        assert_eq!(
            (column(&f, "s"), column(&f, "e")),
            (vec!["1".to_string()], vec!["9".to_string()]),
            "GetOrders lines"
        );

        assert_eq!(
            q(
                &format!(
                    "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.fqn = '{get_orders}' RETURN b.fqn"
                ),
                "b.fqn"
            ),
            vec![format!("{legacy}.app.Helper")],
            "GetOrders calls"
        );
        assert_eq!(
            q(
                &format!(
                    "MATCH (a:Function)-[r:Uses]->(b:Struct) WHERE a.fqn = '{get_orders}' AND b.fqn = '{orders}' RETURN r.properties"
                ),
                "r.properties"
            ),
            vec!["{\"kind\":\"write\"}".to_string()],
            "GetOrders -> Orders uses"
        );
        assert_eq!(
            q(
                &format!(
                    "MATCH (a:Struct)-[r:Uses]->(b:Struct) WHERE a.fqn = '{sdk}.dbo.vCustomers' AND b.fqn = '{sdk}.dbo.Customers' RETURN r.properties"
                ),
                "r.properties"
            ),
            vec!["{\"kind\":\"read\"}".to_string()],
            "vCustomers -> Customers uses"
        );

        let unres = query_rows(
            &repo,
            &format!(
                "MATCH (a:Function)-[:UnresolvedCall]->(u:UnresolvedTarget) WHERE a.fqn = '{get_orders}' RETURN u.fqn, u.category ORDER BY u.fqn"
            ),
        );
        let pairs: Vec<(String, String)> = column(&unres, "u.fqn")
            .into_iter()
            .zip(column(&unres, "u.category"))
            .collect();
        for want in [("GETDATE", "builtin"), ("dbo.Missing", "external")] {
            let want = (want.0.to_string(), want.1.to_string());
            assert!(pairs.contains(&want), "missing {want:?}: {pairs:?}");
        }

        let types = q(
            "MATCH (m:Module)-[:Contains]->(f:File) WHERE m.fqn STARTS WITH 'sql.' RETURN f.fqn, f.code_type ORDER BY f.fqn",
            "f.code_type",
        );
        assert_eq!(types.len(), 6, "every .sql file is a sql File: {types:?}");
        assert!(types.iter().all(|t| t == "src"), "code_type: {types:?}");
    }

    /// `--language sql` selects sql as the only code language (the staged
    /// structural streams follow it); a plain scan auto-detects an upper-case
    /// `.SQL` file.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn language_flag_and_auto_detect_select_sql() {
        sql_frontend_bin();
        let repo = sql_repo("sql-langflag");
        let (out, log) = scan(&repo, &["--language", "sql"]);
        assert!(out.status.success(), "scan: {log}");
        let langs = languages(&log);
        assert_eq!(langs.first().map(String::as_str), Some("sql"), "{langs:?}");
        assert!(
            langs[1..]
                .iter()
                .all(|l| apg::frontends::STRUCTURAL_LANGUAGES.contains(&l.as_str())),
            "only structural streams may follow sql: {langs:?}"
        );
        assert!(log.contains("running sql frontend"), "{log}");

        let upper = Repo::new("sql-autodetect");
        upper.write("DB/T.SQL", CUSTOMERS_SQL);
        upper.commit_all("upper-case sql");
        let (out, log) = scan(&upper, &[]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            languages(&log).contains(&"sql".to_string()),
            "sql auto-detected: {log}"
        );
        let owners = column(
            &query_rows(
                &upper,
                "MATCH (m:Module)-[:Contains]->(f:File) WHERE f.fqn = 'DB/T.SQL' RETURN m.fqn",
            ),
            "m.fqn",
        );
        assert!(
            !owners.is_empty() && owners.iter().all(|m| m.starts_with("sql.")),
            "DB/T.SQL must be a sql. File: {owners:?}"
        );
    }

    /// A plain scan leaves `.sqlproj` files to the xml stream while every
    /// `.sql` file belongs to a sql. module and to no structural stream.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn sqlproj_stays_an_xml_file_and_sql_leaves_the_structural_streams() {
        sql_frontend_bin();
        let repo = sql_repo("sql-claims");
        let (out, log) = scan(&repo, &[]);
        assert!(out.status.success(), "scan: {log}");

        for proj in ["Legacy/Legacy.sqlproj", "Sdk/Sdk.sqlproj"] {
            let owners = column(
                &query_rows(
                    &repo,
                    &format!(
                        "MATCH (m:Module)-[:Contains]->(f:File) WHERE f.fqn = '{proj}' RETURN m.fqn"
                    ),
                ),
                "m.fqn",
            );
            assert!(
                !owners.is_empty() && owners.iter().all(|m| m.starts_with("xml.")),
                "{proj} must be an xml. File only: {owners:?}"
            );
        }

        let rows = query_rows(
            &repo,
            "MATCH (m:Module)-[:Contains]->(f:File) WHERE f.fqn ENDS WITH '.sql' RETURN f.fqn, m.fqn ORDER BY f.fqn",
        );
        let files = column(&rows, "f.fqn");
        let owners = column(&rows, "m.fqn");
        assert_eq!(
            files,
            vec![
                "Legacy/Procs/GetOrders.sql",
                "Legacy/Procs/Helper.sql",
                "Legacy/Tables/Orders.sql",
                "Sdk/dbo/Customers.sql",
                "Sdk/dbo/vCustomers.sql",
                "scripts/seed.sql",
            ],
            "each .sql file is contained exactly once"
        );
        for m in &owners {
            assert!(m.starts_with("sql."), "sql file under {m}: {rows:?}");
            for structural in ["misc.", "md.", "xml."] {
                assert!(!m.starts_with(structural), "{m}");
            }
        }
    }

    /// A broken batch is diagnosed, not fatal: the valid batches around it
    /// are graphed and the scan exits 0.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn syntax_error_does_not_abort_the_scan() {
        sql_frontend_bin();
        let repo = sql_repo("sql-syntax");
        repo.write("scripts/broken.sql", BROKEN_SQL);
        repo.commit_all("broken batch");
        let (out, log) = scan(&repo, &["--language", "sql"]);
        assert!(out.status.success(), "scan: {log}");
        assert!(!log.contains("sql frontend failed"), "{log}");

        let decls: Vec<String> = ["Struct", "Function"]
            .iter()
            .flat_map(|label| {
                column(
                    &query_rows(
                        &repo,
                        &format!(
                            "MATCH (n:{label}) WHERE n.path = 'scripts/broken.sql' RETURN n.fqn"
                        ),
                    ),
                    "n.fqn",
                )
            })
            .collect();
        for want in [".dbo.Good1", ".dbo.Good2"] {
            assert!(
                decls.iter().any(|d| d.ends_with(want)),
                "missing {want}: {decls:?}"
            );
        }

        let file_log = std::fs::read_to_string(repo.root.join("apg/.trans/apg-frontend.log"))
            .expect("read apg-frontend.log");
        let diag = file_log
            .lines()
            .filter_map(|l| l.strip_prefix("sql: "))
            .find_map(|l| l.split_once("broken.sql:").map(|(_, rest)| rest))
            .unwrap_or_else(|| panic!("no sql: broken.sql diagnostic: {file_log}"));
        let mut pos = diag.splitn(3, ':');
        let line: usize = pos.next().unwrap().parse().expect("diagnostic line");
        let col: usize = pos.next().unwrap().parse().expect("diagnostic column");
        assert_eq!(line, 3, "diagnostic on the broken batch: {diag}");
        assert!(col >= 1, "{diag}");
    }

    /// Each incremental scan — a line-shifting body edit, a new declaration
    /// in another file, a `.sqlproj`-only change — yields the same SQL facts
    /// as a `--force` full scan of the same tree.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn incremental_scan_equals_full_scan() {
        sql_frontend_bin();
        let repo = sql_repo("sql-incremental");
        let (out, log) = scan(&repo, &["--language", "sql"]);
        assert!(out.status.success(), "scan: {log}");
        let mut prev = sql_facts(&repo);

        let steps: [(&str, &str, &str); 3] = [
            (
                "Legacy/Procs/GetOrders.sql",
                &GET_ORDERS_SQL.replacen("BEGIN\n", "BEGIN\n    SELECT 0\n\n", 1),
                "(1) body edit",
            ),
            (
                "Legacy/Procs/Missing.sql",
                "CREATE PROCEDURE dbo.Missing\nAS\n    SELECT 2\nGO\n",
                "(2) new declaration in another file",
            ),
            (
                "Legacy/Legacy.sqlproj",
                &LEGACY_SQLPROJ.replace(
                    "<DefaultSchema>app</DefaultSchema>",
                    "<DefaultSchema>sales</DefaultSchema>",
                ),
                "(3) .sqlproj-only change",
            ),
        ];
        for (path, content, step) in steps {
            repo.write(path, content);
            repo.commit_all(step);
            let (inc, full) = incremental_then_full(&repo, true);
            assert_facts_eq(&inc, &full, step);
            assert_ne!(inc, prev, "{step}: the edit must be observed");
            prev = full;
        }

        let legacy = "sql.Legacy/Legacy.sqlproj";
        let calls = column(
            &query_rows(
                &repo,
                &format!(
                    "MATCH (a:Function)-[:Calls]->(b:Function) WHERE a.fqn = '{legacy}.app.GetOrders' RETURN b.fqn ORDER BY b.fqn"
                ),
            ),
            "b.fqn",
        );
        assert!(
            calls.contains(&format!("{legacy}.dbo.Missing")),
            "dbo.Missing must resolve: {calls:?}"
        );
        assert!(
            prev.iter()
                .any(|f| f.contains(&format!("{legacy}.sales.Orders"))),
            "Orders re-scoped to sales: {prev:#?}"
        );
    }

    /// Only tracked `.sql` files are graphed (a gitignored generated publish
    /// script never is), and a non-sql change and a deletion each scan
    /// incrementally to the facts of a `--force` full scan.
    #[test]
    #[ignore = "e2e tier: real I/O (scratch repo, spawned apg); run via cargo test-e2e"]
    fn tracked_sql_only_and_deletion_equal_full_scan() {
        sql_frontend_bin();
        let repo = sql_repo("sql-tracked");
        repo.write(
            ".gitignore",
            "apg/.trans/\napg/.worktrees/\napg/.scratch/\nbin/\n",
        );
        repo.commit_all("ignore bin/");
        let publish = "Sdk/bin/Debug/Db.publish.sql";
        repo.write(
            publish,
            ":setvar DatabaseName \"Db\"\nGO\nCREATE TABLE dbo.PublishOnly (Id INT)\nGO\n",
        );
        assert!(
            git2_repo(&repo)
                .is_path_ignored(Path::new(publish))
                .expect("check-ignore"),
            "{publish} must be gitignored"
        );
        let no_publish = |facts: &BTreeSet<String>, ctx: &str| {
            let leaked: Vec<_> = facts
                .iter()
                .filter(|f| f.contains("Db.publish.sql") || f.contains("PublishOnly"))
                .collect();
            assert!(
                leaked.is_empty(),
                "{ctx}: ignored script graphed: {leaked:#?}"
            );
        };

        // (i) a full scan graphs nothing from the ignored script.
        let (out, log) = scan(&repo, &["--language", "sql"]);
        assert!(out.status.success(), "scan: {log}");
        assert!(
            graphed_at(&repo, publish).is_empty(),
            "(i) {publish} graphed"
        );
        let publish_only = column(
            &query_rows(
                &repo,
                "MATCH (n:Struct) WHERE n.fqn STARTS WITH 'sql.' AND n.fqn ENDS WITH '.PublishOnly' RETURN n.fqn",
            ),
            "n.fqn",
        );
        assert!(publish_only.is_empty(), "(i) PublishOnly: {publish_only:?}");
        assert!(
            !graphed_at(&repo, "Sdk/dbo/Customers.sql").is_empty(),
            "(i) Customers.sql must still be graphed"
        );

        // (ii) a README-only change.
        repo.write("README.md", "# scratch\n\nedited\n");
        repo.commit_all("readme");
        let (inc, full) = incremental_then_full(&repo, false);
        assert_facts_eq(&inc, &full, "(ii) README-only change");
        no_publish(&inc, "(ii) incremental");
        no_publish(&full, "(ii) full");
        let before = full;

        // (iii) delete a tracked .sql file.
        let helper = "Legacy/Procs/Helper.sql";
        std::fs::remove_file(repo.root.join(helper)).expect("delete Helper.sql");
        let git = git2_repo(&repo);
        let mut index = git.index().expect("index");
        index.update_all(["*"], None).expect("stage deletion");
        index.write().expect("write index");
        repo.commit_all("delete Helper.sql");
        let (out, log) = scan(&repo, &["--language", "sql"]);
        assert!(out.status.success(), "(iii) incremental scan: {log}");
        assert!(log.contains("[scan] incremental:"), "{log}");
        assert!(log.contains("running sql frontend"), "{log}");
        assert!(!log.contains("sql frontend failed"), "{log}");
        let left = graphed_at(&repo, helper);
        assert!(left.is_empty(), "(iii) deleted file rows remain: {left:?}");
        let unres = query_rows(
            &repo,
            "MATCH (a:Function)-[:UnresolvedCall]->(u:UnresolvedTarget) WHERE a.fqn = 'sql.Legacy/Legacy.sqlproj.app.GetOrders' AND u.fqn = 'app.Helper' RETURN u.category",
        );
        assert_eq!(
            column(&unres, "u.category"),
            vec!["external"],
            "(iii) GetOrders -> app.Helper unresolved"
        );
        let inc = sql_facts(&repo);
        assert_ne!(inc, before, "(iii) the deletion must be observed");
        let (out, log) = scan(&repo, &["--language", "sql", "--force"]);
        assert!(out.status.success(), "(iii) full scan: {log}");
        assert_facts_eq(&inc, &sql_facts(&repo), "(iii) deletion");
    }
}
