//! The `apg scan` orchestration: the scan temp dir, the merged scanner-record
//! stream, and the `cmd_scan` pipeline driver.
//!
//! Extracted from the crate root as a cohesive single-responsibility module
//! (phase-04 decomposition); no behaviour change — the same detection, spawn
//! loop, pre-ingest universe derivation, and pipeline dispatch.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use crate::frontends::{
    FrontendHandoff, STRUCTURAL_LANGUAGES, auto_detect_languages, available_languages,
    should_spawn_language, spawn_frontend, targets_for_language,
};
use crate::logging::{Log, emit_timing};
use crate::warm::{reuse_universe, warm_prepared, warm_universe};
use crate::{
    cache, classify, find_or_create_apg_root, git, graph, incremental, ingest, layers,
    run_pipeline, schema, session, specs, timing, version_gate,
};

pub fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("apg-load-{}-{nanos}", std::process::id()))
}

/// Build the scanner JSONL record stream from the frontend spools: a
/// `lang_switch` record before each language's records plus the leading
/// `scan_meta` control record. Borrows the spool paths (reopening each file)
/// so it can be run twice — once for the pre-ingest that computes
/// `ingest_tree`'s scanned code-FQN universe, then again for the real
/// pipeline.
pub fn scanner_records<'a>(
    spools: &'a [(String, PathBuf)],
    git_state: &'a git::GitState,
) -> impl Iterator<Item = schema::Record> + 'a {
    let iterators = spools.iter().map(|(lang, spool)| {
        let lines = BufReader::new(std::fs::File::open(spool).unwrap()).lines();
        let records = lines.map(|x| {
            let line = x.expect("io error");
            serde_json::from_str::<schema::Record>(&line)
                .unwrap_or_else(|e| panic!("bad json {e}: {line}"))
        });
        Box::new(
            std::iter::once(schema::Record::LangSwitch {
                language: lang.clone(),
            })
            .chain(records),
        ) as Box<dyn Iterator<Item = schema::Record>>
    });
    let records = iterators.into_iter().flatten();
    std::iter::once(schema::Record::ScanMeta {
        git_sha: git_state.sha.clone(),
        git_clean: git_state.sha.as_ref().map(|_| git_state.clean),
        content_key: git_state.content_key.clone(),
        scanned_at: git::now_iso8601(),
    })
    .chain(records)
}

/// `apg scan [dir] [options] [blacklist...]`: run the scanner + ingestor
/// pipeline and write `db.lbug`, `graph.jsonl`, and `apg-frontend.log` into
/// the project's `.apg` directory. A repo may mix languages: auto-detection
/// (or `--language a,b`) runs every frontend present and merges their graphs
/// into one database. Opaque ids are namespaced per language
/// (`--id-prefix`), and a `lang_switch` record before each stream tells the
/// ingestor which language the following records came from (for code_type
/// classification and FQN rendering). A `scan_meta` control record leads the
/// whole stream with the git state the scan ran under (recorded as the DB's
/// `Scan` node and graph.jsonl line 1).
pub(crate) fn cmd_scan(args: &[String]) -> anyhow::Result<()> {
    // Per-phase timing (phase-04): `scan_start` is taken before any work so the
    // startup/overhead phase spans argument parsing onward.
    let scan_start = std::time::Instant::now();
    let mut timing = timing::TimingReport::new();
    let mut language_args: Vec<String> = Vec::new();
    let mut path_excludes: Vec<String> = Vec::new();
    let mut module_dirs: Vec<String> = Vec::new();
    let mut no_build_scripts = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--language" | "-l" => {
                i += 1;
                if i < args.len() {
                    for l in args[i].split(',') {
                        let l = l.trim();
                        if !l.is_empty() {
                            language_args.push(l.to_string());
                        }
                    }
                }
            }
            "--exclude-path" => {
                i += 1;
                if i < args.len() {
                    path_excludes.push(args[i].clone());
                }
            }
            "--module" => {
                i += 1;
                if i < args.len() {
                    module_dirs.push(args[i].clone());
                }
            }
            "--no-build-scripts" => {
                no_build_scripts = true;
            }
            _ => positional.push(args[i].clone()),
        }
        i += 1;
    }

    let project_dir = if positional.is_empty() {
        std::env::current_dir()?
    } else {
        PathBuf::from(&positional[0])
    };
    let blacklist: Vec<String> = positional.get(1..).unwrap_or(&[]).to_vec();
    let project_dir = project_dir.canonicalize()?;

    // The checkout-independent identity base (`requirements.requirement.
    // repo-relative-file-identity`): the git toplevel, or the scan root when
    // the tree is not a git repository. Every scanner path is rendered
    // repo-relative against it, so `apg scan <repo>` and `apg scan
    // <repo>/subdir` mint the same File identity and two worktrees agree.
    let identity_base = git::repo_rel(&project_dir);

    // The git state this scan runs under (repo HEAD sha + tree cleanliness),
    // recorded as the scan_meta control record and the DB's Scan node so later
    // spec/plan/review mutations can refuse to run against a stale DB.
    let git_state = git::git_state(&project_dir);

    // R10 version gate: `apg scan` is a layout-touching op, so the repo must
    // carry a versioned layout — `apg init` is the layout entry/upgrade act
    // that writes the binary version into apg/config.json. A missing version
    // (pre-versioning layout) or a major/minor mismatch in either direction
    // blocks with upgrade guidance; the gate never warns. The gate runs
    // BEFORE find_or_create below so a refusal never leaves a stray,
    // unversioned layout behind.
    let gate_root =
        specs::find_apg_root(&project_dir).unwrap_or_else(|| project_dir.join(specs::LAYOUT));
    version_gate::require_layout_version(&gate_root, "re-run `apg scan`")?;

    // Resolve the committed `apg/` layout root, then run the pipeline from
    // inside its gitignored `.trans/` so db.lbug / graph.jsonl /
    // apg-frontend.log all land there (the committed `apg/` data — config,
    // specs, notes — stays in the root).
    let apg_root = find_or_create_apg_root(&project_dir);
    // Lifecycle exclusivity (phase-03): a scan replaces `db.lbug` (it unlinks
    // and rebuilds it), which would silently diverge the graph a live session
    // holds open. Refuse BEFORE any of that work — the session must end first.
    if session::live_session(&apg_root) {
        anyhow::bail!(
            "refused: a live `apg session` owns {} — end it first (`apg session end` inside the project worktree) before scanning",
            apg_root.join(specs::TRANS).join("db.lbug").display()
        );
    }
    let trans_dir = apg_root.join(specs::TRANS);
    std::fs::create_dir_all(&trans_dir)?;
    std::env::set_current_dir(&trans_dir)?;

    let mut log = Log::new();
    log.ln(&format!("Project: {}", project_dir.display()));
    // Staleness of the pre-scan DB vs the tree (STALE/FRESH/N-A).
    log.ln(&git::staleness_line(&apg_root, &git_state));

    // Win-A fast path (scan-freshness): when the pre-scan DB's recorded content
    // identity matches the tree exactly, the existing DB is reusable and every
    // language frontend is skipped — reuse the DB and return BEFORE any
    // frontend spawn or DB rebuild. The predicate is content identity (never
    // mtime) and is the same rule `is_stale`/`staleness_line` use, so the
    // printed verdict and the fast-path decision can never disagree. A stale
    // tree falls through to the full pipeline below.
    if git::is_fresh(&apg_root) {
        log.ln(
            "[scan] fast-path: tree unchanged since the recorded scan — reusing db.lbug (frontends skipped)",
        );
        // Phase-04 task-3: the whole-tree fast path runs no frontend and
        // rebuilds nothing, so all the elapsed time is startup/overhead and the
        // frontend phase is reported `frontend-skipped` (with ingest-assembly
        // and db-load at zero, every phase key still present).
        timing.record(timing::Phase::Startup, scan_start.elapsed());
        timing.mark_frontend_skipped();
        emit_timing(&mut log, &timing);
        return Ok(());
    }

    let available = available_languages();
    if available.is_empty() {
        panic!(
            "No scanner frontends found. Install one via brew (e.g. `brew install antz29/apg/apg-go`), the curl installer (e.g. `install.sh go`), set APG_FRONTEND_DIR, or rebuild with the required toolchain."
        );
    }

    let mut languages: Vec<String> = if !language_args.is_empty() {
        for l in &language_args {
            if !available.iter().any(|a| a == l) {
                panic!(
                    "Language '{l}' is not available. Installed frontends: {}. Install it via brew (e.g. `brew install antz29/apg/apg-{l}`) or the curl installer (`install.sh {l}`).",
                    available.join(", ")
                );
            }
        }
        language_args
    } else {
        let detected = auto_detect_languages(&project_dir, &available);
        if detected.is_empty() {
            available.clone()
        } else {
            detected
        }
    };
    // The structural stream ids are selected whenever the bundled
    // `structfrontend` is installed: they are not extension-detectable (the
    // driver enumerates them), so neither `--language` nor auto-detection can
    // name them, and the selection must not drop them. Appended in canonical
    // order after the code languages.
    for lang in STRUCTURAL_LANGUAGES {
        if available.iter().any(|l| l == lang) && !languages.iter().any(|l| l == lang) {
            languages.push(lang.to_string());
        }
    }
    log.ln(&format!("Languages: {}", languages.join(", ")));

    if !blacklist.is_empty() {
        log.ln(&format!("Blacklist: {:?}", blacklist));
    }
    if !path_excludes.is_empty() {
        log.ln(&format!("Path excludes: {:?}", path_excludes));
    }

    let config = classify::ApgConfig::load(&project_dir);

    // The scan config identity shared by the warm-cache probe and the win-B
    // preparation: the exact languages/excludes/modules this scan runs with.
    let scan_config = cache::ScanConfigKey {
        languages: languages.clone(),
        excludes: path_excludes.clone(),
        modules: module_dirs.clone(),
    };

    // Win-B incremental preparation (phase-02 task-8): the content manifest,
    // git delta + correctness fallbacks, impact target set, and the fact-reuse
    // candidates. `full_scan: Some(reason)` falls through to the full pipeline
    // (the correctness reference). The target set drives the frontend
    // `--targets` hand-off (task-9) and the fact splice in `run_pipeline`.
    //
    // Warm-cache seed (phase-06 task-4): when the shared store already holds a
    // COMPLETE recorded scan for exactly this HEAD, prepare from the recorded
    // manifest and assemble from its re-based facts — no `Manifest::build`, so
    // no full-tree walk, and zero frontends. The ordinary `prepare` runs only
    // when the warm probe misses. A blacklist is never folded into the cache
    // key, so a blacklisted scan falls back to the ordinary path (never a warm
    // reuse of facts recorded under a different FQN filter).
    let warm = if blacklist.is_empty() {
        warm_prepared(&project_dir, &apg_root, &git_state, &scan_config)
    } else {
        None
    };
    let warm_complete = warm.is_some();
    let incremental =
        warm.unwrap_or_else(|| incremental::prepare(&project_dir, &apg_root, &scan_config));
    let mut handoff = FrontendHandoff::default();
    let mut reuse_plan: Option<incremental::ReusePlan> = None;
    if let Some(reason) = &incremental.full_scan {
        log.ln(&format!("[scan] {}", reason.describe()));
        // Phase-04 task-33: carry the pinned cache hand-off on the FULL-scan
        // path too, so each frontend's full-scan native-artifact seeding (the
        // Java class surface behind tasks 17/32) is reachable through the CLI.
        // `targets_enabled` stays FALSE — a full scan emits everything and must
        // carry NO `--targets` (phase-02 task-9). The no-store case
        // (`FullScanReason::NotAGitRepo` leaves `store_root` empty) yields
        // `None`/`None`: never a fabricated or defaulted cache path (AC (c)).
        if !incremental.store_root.as_os_str().is_empty() {
            handoff.cache_dir = Some(incremental.store_root.clone());
            handoff.cache_key = Some(incremental.cache_key.token());
        }
    } else if warm_complete {
        log.ln(&format!(
            "[scan] warm cache: recorded scan at HEAD — assembling {} file(s) from re-based facts (frontends skipped)",
            incremental.reuse_candidates.len(),
        ));
        // No `--targets`: nothing is re-emitted; the per-file units are spliced
        // from the store and their languages' scaffolding is replayed from the
        // store's `skipped_langs` set.
        handoff.cache_dir = Some(incremental.store_root.clone());
        handoff.cache_key = Some(incremental.cache_key.token());
        reuse_plan = Some(incremental::ReusePlan {
            store_root: incremental.store_root.clone(),
            cache_key: incremental.cache_key.clone(),
            files: incremental
                .reuse_candidates
                .iter()
                .map(|f| (f.rel.clone(), f.lang.clone(), f.oid.clone()))
                .collect(),
            reader_root: project_dir.to_string_lossy().into_owned(),
            // Every language is reconstructible from the store; the final set is
            // refilled after the (zero-spawn) loop from the same full cache.
            skipped_langs: languages.iter().cloned().collect(),
        });
    } else {
        log.ln(&format!(
            "[scan] incremental: {} target file(s), {} reusable file(s)",
            incremental.targets_rel.len(),
            incremental.reuse_candidates.len(),
        ));
        // The pinned target-set hand-off contract (task-9): `--targets`,
        // `--cache-dir`, `--cache-key` on every language's argv. The target
        // files live in the scan's own temp dir (removed with it).
        handoff.targets_enabled = true;
        handoff.cache_dir = Some(incremental.store_root.clone());
        handoff.cache_key = Some(incremental.cache_key.token());
        reuse_plan = Some(incremental::ReusePlan {
            store_root: incremental.store_root.clone(),
            cache_key: incremental.cache_key.clone(),
            files: incremental
                .reuse_candidates
                .iter()
                .map(|f| (f.rel.clone(), f.lang.clone(), f.oid.clone()))
                .collect(),
            reader_root: project_dir.to_string_lossy().into_owned(),
            // Filled from the FINAL target set after the spawn loop.
            skipped_langs: BTreeSet::new(),
        });
    }

    // Each frontend's stderr (progress + compiler diagnostics) is spooled to a
    // per-language temp file, then folded into the log file. On a non-zero
    // exit the tail is also reported to the terminal (SPEC 0.9.1 R1).
    log.ln("Frontend progress -> apg-frontend.log");

    // Startup/overhead ends where the frontend work begins (phase-04 task-2):
    // argument parsing, git state, the version gate, layout discovery, and the
    // win-B incremental preparation all fall in this phase.
    timing.record(timing::Phase::Startup, scan_start.elapsed());
    let frontend_start = std::time::Instant::now();

    // Drain each frontend's stdout to a temp file (spooled to disk, never
    // buffered in memory), then ingest the merged streams. Running them
    // sequentially avoids pipe-backpressure deadlock and matches the old
    // single-frontend behavior. A failing frontend is reported and skipped, not
    // fatal: the remaining languages still produce a graph, and a non-zero exit
    // is aggregated at the end of the run.
    let tmp = temp_dir();
    std::fs::create_dir_all(&tmp).unwrap();
    let multi = languages.len() > 1;
    let mut spools: Vec<(String, PathBuf)> = Vec::new();
    let mut failed: Vec<String> = Vec::new();

    // Phase 1: the changed files ∪ overload peers (the stage-1 target set).
    // The signature early-cutoff (phase-02 task-6) is applied AFTER this pass:
    // the phase-1 stream yields the changed files' new exported signatures, and
    // only a genuine signature change pulls the reverse-dependency closure into
    // phase 2. A body-only change ends after phase 1, so its dependents are
    // reused.
    let full_scan_path = incremental.full_scan.is_some();
    let mut phase = 1u32;
    let mut targets_rel = incremental.targets_rel.clone();
    loop {
        // The scan as a whole has work to re-emit (the PARTIAL case). When it
        // does not, phase-02 runs every frontend unfiltered instead of skipping
        // (see `should_spawn_language`).
        let any_targets = !targets_rel.is_empty();
        for lang in &languages {
            let targets = targets_for_language(&targets_rel, &project_dir, lang);
            // Win-C per-language spawn skip (phase-03 task-5): on the
            // incremental path an unchanged language (empty target set while
            // the scan has other changed languages) has its frontend process
            // skipped entirely; its per-file facts arrive via the win-B
            // cached-fact reuse path. On a full scan every language spawns. A
            // phase-2 language whose only targets are its phase-1 ones still
            // has them in the final (stage-1 ∪ cascade) set, so it re-runs and
            // replaces its spool; only a language with no targets at all is
            // skipped here.
            if !should_spawn_language(
                full_scan_path,
                targets.is_empty(),
                any_targets,
                warm_complete,
            ) {
                if phase == 1 {
                    if warm_complete {
                        log.ln(&format!(
                            "[scan] {lang}: recorded scan at HEAD — frontend skipped (warm cache)"
                        ));
                    } else {
                        log.ln(&format!(
                            "[scan] {lang}: no changed targets — frontend skipped (facts reused)"
                        ));
                    }
                }
                continue;
            }
            match spawn_frontend(
                lang,
                &project_dir,
                &module_dirs,
                &path_excludes,
                no_build_scripts,
                multi,
                &handoff,
                &tmp,
                &targets,
                phase,
                &mut log,
            )? {
                Some(spool) => {
                    // A phase-2 re-run replaces the language's phase-1 spool, so
                    // each language contributes exactly one stream (no duplicate
                    // emission / FQN collision).
                    spools.retain(|(l, _)| l != lang);
                    spools.push((lang.clone(), spool));
                }
                None => failed.push(lang.clone()),
            }
        }
        if phase == 2 || incremental.full_scan.is_some() {
            break;
        }
        // Apply the signature early-cutoff using the phase-1 stream.
        let extra = {
            let phase1_lang = if languages.len() == 1 {
                languages[0].clone()
            } else {
                languages.join(",")
            };
            let (phase1_graph, _) = ingest::ingest(
                scanner_records(&spools[..], &git_state),
                &ingest::IngestOptions {
                    blacklist: &blacklist,
                    language: &phase1_lang,
                    config: config.as_ref(),
                    base: Some(&identity_base),
                },
            );
            incremental::extra_cascade_targets(
                &incremental.store_root,
                &project_dir,
                &phase1_graph,
                &targets_rel,
            )
        };
        if extra.is_empty() {
            break;
        }
        log.ln(&format!(
            "[scan] signature change cascades to {} dependent file(s)",
            extra.len()
        ));
        targets_rel.extend(extra);
        // Phase 2 re-spawns the languages that gained targets over the UNION
        // (stage-1 ∪ cascade), with the spools accumulated above: a language
        // that gained targets is dropped and re-spawned so its stream covers
        // the union exactly once.
        phase = 2;
    }
    // The frontend phase covers the whole spawn loop — both the stage-1 pass
    // and any signature-cascade stage-2 re-runs (phase-04 task-2).
    timing.record(timing::Phase::Frontend, frontend_start.elapsed());
    // The final re-emission target set (stage 1 ∪ the signature cascade).
    let targets_rel = targets_rel;
    // Rebuild the reuse plan against the FULL target set, so a cascaded
    // dependent is re-emitted rather than reused from stale facts.
    if incremental.full_scan.is_none() {
        let files: Vec<(String, String, String)> = incremental
            .reuse_candidates
            .iter()
            .filter(|f| !targets_rel.contains(&f.rel))
            .map(|f| (f.rel.clone(), f.lang.clone(), f.oid.clone()))
            .collect();
        // The languages whose frontend was skipped this scan: every language
        // with reused facts that has NO file in the final target set. Derived
        // from the SAME final target set that drives the spawn skip and the DB
        // splice, so the scaffolding replay and the spawn skip can never
        // disagree; a language with any target is spawned and emits its own
        // (fresh) scaffolding.
        //
        // Warm-cache completeness (phase-06 task-1/4): every configured language
        // was skipped, so EVERY language's scaffolding must be replayed from the
        // store — the empty-target-set case below.
        let skipped_langs: BTreeSet<String> = if warm_complete {
            languages.iter().cloned().collect()
        } else if targets_rel.is_empty() {
            BTreeSet::new()
        } else {
            let with_targets: BTreeSet<&str> = targets_rel
                .iter()
                .map(|rel| incremental::language_of(rel))
                .collect();
            files
                .iter()
                .map(|(_, lang, _)| lang)
                .filter(|lang| !with_targets.contains(lang.as_str()))
                .cloned()
                .collect()
        };
        reuse_plan = Some(incremental::ReusePlan {
            store_root: incremental.store_root.clone(),
            cache_key: incremental.cache_key.clone(),
            files,
            reader_root: project_dir.to_string_lossy().into_owned(),
            skipped_langs,
        });
    }

    // Merge the streams into one record iterator, with a `lang_switch` record
    // before each language's records so the ingestor classifies and renders
    // each under the right language. A `scan_meta` control record (the git
    // state this scan ran under) leads the whole stream; the ingestor turns it
    // into the DB's `Scan` node and the export puts it on graph.jsonl line 1.
    //
    // The scanner stream is built by a helper (borrowing the spool paths) so
    // it can be read twice: once for a pre-ingest that computes the scanned
    // code-FQN universe (the honest renderer reuse for `ingest_tree`'s
    // `implemented-by` validation), and once for the real pipeline.
    // Ingest-assembly (phase-04 task-4) starts at the pre-ingest/universe work
    // below and continues through `run_pipeline`'s own ingestion; the DB build
    // after that in `run_pipeline` is the db-load phase.
    let assembly_start = std::time::Instant::now();

    let records = scanner_records(&spools, &git_state);

    // Cleanup span validation is per-language: keep the single-language value,
    // and disable it (by joining) for mixed scans where the check cannot be
    // attributed per node.
    let cleanup_language = if languages.len() == 1 {
        languages[0].clone()
    } else {
        languages.join(",")
    };

    // Pre-ingest the scanner stream to compute the scanned code-FQN universe
    // `ingest_tree` validates `implemented-by` refs against (real → real).
    //
    // FULL-UNIVERSE SEAM (feedback-92): on the win-B incremental path the spool
    // holds only the re-emitted target facts, so a language skipped/emission-
    // filtered this scan would vanish from the universe and `validate_code_refs`
    // would falsely bail `spec drift`. Derive the FULL universe instead from the
    // PREVIOUS export (the sole full code-identity source) MINUS the delta's
    // removed FQNs UNION the delta's emitted real code FQNs — never the
    // target-only spool. On a full scan the full spool IS the universe.
    //
    // Warm-cache path (phase-06 task-4): the spool is EMPTY (zero frontends) and
    // a fresh worktree has no local export, so derive the universe from the
    // shared store's re-based facts + module scaffolding — exactly the nodes the
    // warm assembly produces.
    let scanned_code: BTreeSet<String> = if warm_complete {
        let warm_langs: BTreeSet<String> = languages.iter().cloned().collect();
        warm_universe(
            &incremental.store_root,
            &incremental.cache_key,
            &incremental.manifest,
            &warm_langs,
        )
    } else if incremental.full_scan.is_some() {
        let (pre, _) = ingest::ingest(
            scanner_records(&spools, &git_state),
            &ingest::IngestOptions {
                blacklist: &blacklist,
                language: &cleanup_language,
                config: config.as_ref(),
                base: Some(&identity_base),
            },
        );
        pre.nodes
            .iter()
            .filter(|(_, n)| {
                matches!(
                    n.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                ) && n.status.is_none()
            })
            .map(|(f, _)| f.clone())
            .collect()
    } else {
        // The delta's emitted real code FQNs: pre-ingest the target-only spool
        // to discover exactly which FQNs this scan's frontends produced.
        let (emitted, _) = ingest::ingest(
            scanner_records(&spools, &git_state),
            &ingest::IngestOptions {
                blacklist: &blacklist,
                language: &cleanup_language,
                config: config.as_ref(),
                base: Some(&identity_base),
            },
        );
        let emitted_fqns: BTreeSet<String> = emitted
            .nodes
            .iter()
            .filter(|(_, n)| {
                matches!(
                    n.kind,
                    graph::NodeKind::Module
                        | graph::NodeKind::Struct
                        | graph::NodeKind::Function
                        | graph::NodeKind::File
                ) && n.status.is_none()
            })
            .map(|(f, _)| f.clone())
            .collect();
        let mut universe = incremental::full_universe(&apg_root, &incremental, &emitted_fqns);
        // The reused files' facts are spliced from the shared store, not
        // re-emitted, so they contribute no spool FQNs. This checkout's previous
        // export can lag the shared store (another worktree scanned ahead), so
        // union in the reused units' own code FQNs — otherwise a reused file
        // newer than the local export would drop out of the universe and falsely
        // trip `spec drift`.
        if let Some(plan) = &reuse_plan {
            universe.extend(reuse_universe(
                &plan.store_root,
                &plan.cache_key,
                &plan.files,
                &plan.skipped_langs,
            ));
        }
        universe
    };

    // Read the transient legs (`.trans/plans/*.jsonl` — the per-branch plan
    // store — plus the five `.trans/<tier>/*.jsonl` feedback mirrors, SPEC
    // §5: feedback sits in the tier dir of its attached node, both halves of
    // the relationship in `.trans`) into both the planned-FQN universe
    // `ingest_tree` uses (planned → pending) and the records the pipeline
    // chains after code.
    let transient_files = specs::plan_files(&apg_root)
        .into_iter()
        .chain(specs::trans_mirror_files(&apg_root))
        .collect::<Vec<_>>();
    let mut transient_records: Vec<schema::Record> = Vec::new();
    let mut planned: BTreeSet<String> = BTreeSet::new();
    for f in &transient_files {
        for r in specs::read_jsonl(f).unwrap_or_else(|e| panic!("{e:#}")) {
            if let schema::Record::PlannedNode { fqn, .. } = &r {
                planned.insert(fqn.clone());
            }
            transient_records.push(r);
        }
    }

    // Ingest the durable `apg/layers/` tree into the new-model records,
    // validating pairing / code-refs / constraints (R14/R16) against the
    // scanned graph and the planned-node universe.
    let layers_records = layers::ingest_tree(&apg_root, &scanned_code, &planned)?;

    if !layers_records.is_empty() || !transient_records.is_empty() {
        log.ln(&format!(
            "Layer tree + transient inputs: {} layer-node records, {} transient records",
            layers_records.len(),
            transient_records.len(),
        ));
    }

    let records = records.chain(layers_records).chain(transient_records);

    // The win-B pipeline input: the store to splice cached facts from and to
    // record the completed scan back into. The store is ALWAYS recorded (a full
    // scan records the cold baseline the next scan diffs against); only `reuse`
    // is `None` on the full path.
    let pipeline_input = if incremental.store_root.as_os_str().is_empty() {
        None
    } else {
        Some(incremental::PipelineInput {
            store_root: Some(incremental.store_root.clone()),
            cache_key: incremental.cache_key.clone(),
            scan_root: project_dir.clone(),
            manifest: incremental.manifest.clone(),
            sha: git_state.sha.clone().unwrap_or_default(),
            reuse: reuse_plan.clone(),
            // The win-C splice's delete scope and subtraction set are the SAME
            // phase-2 state that drove the frontend target hand-off: the FINAL
            // target set (stage-1 ∪ the signature cascade) and the delta's
            // removed FQNs. Threaded, never re-derived (phase-03 task-4).
            targets_rel: targets_rel.clone(),
            removed_fqns: incremental.removed_fqns.clone(),
            // The shared recorded content identity the delta was derived from,
            // captured before the completed scan rewrote `scan.json` — the
            // splice's equivalence guard (feedback-101).
            recorded_content_key: incremental.recorded_content_key.clone(),
        })
    };

    timing.add(timing::Phase::IngestAssembly, assembly_start.elapsed());
    let pipeline_timings = run_pipeline(
        records,
        &blacklist,
        &path_excludes,
        &cleanup_language,
        config.as_ref(),
        pipeline_input.as_ref(),
        Some(&identity_base),
        &mut log,
    );
    timing.add(
        timing::Phase::IngestAssembly,
        pipeline_timings.ingest_assembly,
    );
    timing.record(timing::Phase::DbLoad, pipeline_timings.db_load);
    let _ = std::fs::remove_dir_all(&tmp);
    log.ln("[scan] spool temp dir removed");
    // The per-phase report is first-class scan output (phase-04 task-2): it is
    // emitted on every non-panicking path, including a partial graph after a
    // frontend failure.
    emit_timing(&mut log, &timing);
    if !failed.is_empty() {
        anyhow::bail!(
            "{} frontend(s) failed to scan (partial graph written): {}",
            failed.len(),
            failed.join(", ")
        );
    }
    Ok(())
}
