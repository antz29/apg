//! Reachability collection over the shared fact store: the store's **only**
//! deleting pass.
//!
//! A scan never deletes — it writes bodies, units, scaffolding and its own
//! per-worktree rows. The store therefore grows with every byte identity every
//! worktree has ever scanned, and this module reaps what nothing can read
//! again. The roots are exactly three:
//!
//! 1. **Every live worktree's current manifest.** A worktree is live when it is
//!    registered in the store and its recorded checkout root still exists. A
//!    unit row `(format, config digest, language, repo-relative path, blob OID)`
//!    is reached when some live manifest maps that repo-relative path to that
//!    OID — the same `(rel, oid)` identity the delta path reuses on, so the
//!    language and the config digest never have to be re-derived to decide
//!    reachability.
//! 2. **The live scaffolding.** Module scaffolding is global, not per worktree,
//!    so every stored scaffolding row reaches the body it references. A
//!    superseded scaffolding body (one whose row was replaced) reaches nothing
//!    and is collected.
//! 3. **Every unexpired scan intent.** A scan records an intent row before its
//!    read/reuse phase and clears it after the record commit, so a concurrently
//!    running collector must not reap the bodies that scan has written but not
//!    yet committed a manifest for. Each unexpired intent roots every unit whose
//!    recorded `writer_root` is that worktree's checkout root (it can root
//!    nothing narrower — the store knows the writing root per unit, not per
//!    body).
//!
//! **The grace period.** Body rows carry raw bytes and no timestamp, so the only
//! clock input the store holds is the per-worktree intent row's unix seconds.
//! The grace period is therefore resolved *against the intents*: an intent is
//! unexpired while `now - at <= `[`INTENT_GRACE_SECS`] (see [`intent_live`]), and
//! an unexpired intent's worktree roots its written units as above. The window
//! is wide enough to span one scan's read/reuse phase and is also the bound
//! after which a crashed scan's leftover intent stops protecting anything. An
//! intent whose worktree is not registered cannot be scoped to a root at all, so
//! the whole pass collects nothing while it is unexpired.
//!
//! **Deletion is one transaction of its own**, opened after the record commit
//! returns and never inside it: a collection opens its own [`Store`], computes
//! the reachability marking set over one read view ([`collectable`]), and
//! deletes every collected body together with every unit row no live manifest
//! reaches — so no dangling row outlives a collected body. The pure policy
//! ([`collectable`], [`intent_live`]) is separated from the I/O ([`collect`]) so
//! it is testable over plain in-memory sets.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::cache::body::BodyId;
use crate::cache::factstore::FACTS_DB;
use crate::cache::store::{ScaffoldingKey, Store, UnitKey, UnitRow, WorktreeId};

/// How long a scan intent is honoured, in unix seconds. An intent younger than
/// this still roots its worktree's written units: long enough to cover one
/// scan's read/reuse phase, and the bound after which a crashed scan's leftover
/// intent stops protecting anything.
pub const INTENT_GRACE_SECS: u64 = 15 * 60;

/// The current unix time in whole seconds (`0` when the clock predates the
/// epoch). The store holds no clock of its own, so the intent writer records
/// this and the collector reads it.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// True while an intent recorded at `at` is still inside the grace window as of
/// `now`: unexpired at exactly `now - at <= grace` (the boundary included), and
/// an `at` later than `now` — a clock skew between two worktrees — counts as
/// unexpired.
pub fn intent_live(at: u64, now: u64, grace: u64) -> bool {
    now.saturating_sub(at) <= grace
}

/// The collected row sets the reachability marking set is computed over — plain
/// data, so the policy is exercised without a database.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// Every stored body.
    pub bodies: Vec<BodyId>,
    /// Every stored unit row.
    pub units: Vec<(UnitKey, UnitRow)>,
    /// Every stored scaffolding row.
    pub scaffolding: Vec<(ScaffoldingKey, BodyId)>,
    /// Each **live** worktree's current `repo-relative path -> blob OID`
    /// manifest.
    pub manifests: Vec<BTreeMap<String, String>>,
    /// The checkout roots (a [`UnitRow::writer_root`] value) of the worktrees
    /// holding an unexpired scan intent.
    pub intent_roots: BTreeSet<String>,
}

/// The rows one collection deletes. A unit or scaffolding row that referenced a
/// deleted body is in the same set, so no dangling row outlives its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collection {
    /// The bodies no root reaches.
    pub bodies: Vec<BodyId>,
    /// The unit rows no live manifest reaches (and every one a collected body
    /// would dangle from).
    pub units: Vec<UnitKey>,
    /// The scaffolding rows a collected body would dangle from.
    pub scaffolding: Vec<ScaffoldingKey>,
}

impl Collection {
    /// True when the pass has nothing to delete — the common case, and the one
    /// that opens no write transaction at all.
    pub fn is_empty(&self) -> bool {
        self.bodies.is_empty() && self.units.is_empty() && self.scaffolding.is_empty()
    }
}

/// The reachability marking set: every body that no live manifest, live
/// scaffolding row or unexpired intent reaches, plus every row that would
/// dangle from one of them. Pure — no I/O, no clock.
pub fn collectable(view: &View) -> Collection {
    // A body is a root when the live scaffolding references it, or when a unit
    // that a live manifest (or an unexpired intent) reaches references it.
    let mut live: BTreeSet<BodyId> = view.scaffolding.iter().map(|(_, body)| *body).collect();
    let mut units = Vec::new();
    for (key, row) in &view.units {
        let reached = view
            .manifests
            .iter()
            .any(|manifest| manifest.get(&key.rel) == Some(&key.oid))
            || view.intent_roots.contains(&row.writer_root);
        if reached {
            live.insert(row.body);
        } else {
            // A unit no live manifest reaches is itself collectable, whatever
            // becomes of its body: a body shared on content may survive under
            // another worktree's unit, and this row still cannot be reused.
            units.push(key.clone());
        }
    }
    let bodies: Vec<BodyId> = view
        .bodies
        .iter()
        .copied()
        .filter(|body| !live.contains(body))
        .collect();
    // Every unit row a collected body would dangle from is already in `units`
    // above: a reached unit's body enters `live`, so it can never be collected.
    // Only a scaffolding row could reference a collected body — and every
    // scaffolding body is a root — so the guard is explicit here rather than
    // implied, keeping the "no dangling row" invariant visible in one place.
    let scaffolding = view
        .scaffolding
        .iter()
        .filter(|(_, body)| !live.contains(body))
        .map(|(key, _)| key.clone())
        .collect();
    Collection {
        bodies,
        units,
        scaffolding,
    }
}

/// Collects the store at `store_root` — the `<git-common-dir>/apg` directory
/// holding `facts.db` — in one write transaction of its own, and returns the
/// rows it deleted.
///
/// `now` is the clock reading (unix seconds) and `grace` the intent window; pass
/// [`unix_now`] and [`INTENT_GRACE_SECS`]. The caller must hold no write
/// transaction on the store — this opens its own — so call it after the record
/// commit returns, never inside it. A store that was never written is left
/// untouched (this never creates one), and a pass with nothing to delete opens
/// no write transaction at all.
///
/// The view is read first and the write transaction opened second: rows created
/// after the view was taken are not in the delete set, so a scan that commits
/// concurrently loses nothing it just wrote. A unit it *reused* from another
/// worktree and committed a manifest for in that window can still be collected;
/// the next scan re-derives it — a cache miss, never a wrong fact.
pub fn collect(store_root: &Path, now: u64, grace: u64) -> Result<Collection> {
    let db = store_root.join(FACTS_DB);
    if !db.exists() {
        return Ok(Collection::default());
    }
    let store = Store::open(&db)?;
    let view = {
        let read = store.read()?;
        let worktrees: BTreeMap<WorktreeId, String> = read.iter_worktrees()?.into_iter().collect();
        let mut manifests = Vec::new();
        for (id, root) in &worktrees {
            if !Path::new(root).is_dir() {
                continue;
            }
            let manifest = read.refs(*id)?;
            if !manifest.is_empty() {
                manifests.push(manifest);
            }
        }
        let mut intent_roots = BTreeSet::new();
        for (id, at) in read.iter_intents()? {
            if !intent_live(at, now, grace) {
                continue;
            }
            let Some(root) = worktrees.get(&id) else {
                // An in-flight scan on a worktree the store has no root for
                // cannot be scoped to any root: collect nothing rather than
                // delete a body it may still be reading.
                return Ok(Collection::default());
            };
            intent_roots.insert(root.clone());
        }
        View {
            bodies: read.iter_body_ids()?,
            units: read.iter_units()?,
            scaffolding: read.iter_scaffolding()?,
            manifests,
            intent_roots,
        }
    };
    let collection = collectable(&view);
    if collection.is_empty() {
        return Ok(collection);
    }
    let write = store.begin_write()?;
    for body in &collection.bodies {
        write.delete_body(*body)?;
    }
    for key in &collection.units {
        write.delete_unit(key)?;
    }
    for key in &collection.scaffolding {
        write.delete_scaffolding(key)?;
    }
    write.commit()?;
    Ok(collection)
}
