//! [`RoutingMemTree`]: a `MemTree`-shaped facade over [`RoutingTree`](finch_routing_tree::RoutingTree), so
//! `MemorySystem` in `lib.rs` can hold this in place of `MemTree` with a minimal-diff call-site
//! swap rather than a rewrite of the surrounding persistence/hydration/quality machinery (already
//! confirmed algorithm-agnostic).
//!
//! Two real, disclosed behavioral differences from `MemTree`, both deliberate:
//!
//! - **No promotion.** A point's id is permanent from the moment `insert` assigns it, regardless
//!   of how the tree restructures around it later (`RoutingTree`'s own design property) -- unlike
//!   `MemTree`, where a leaf can be promoted into a new parent and its content moved to a
//!   different node id. There is nothing for a caller's `memory_sources` row to follow after the
//!   fact; `RoutingInsertEffect` has no `promotion` field at all, not an always-`None` one.
//! - **No importance-weighted re-ranking.** `MemTree::retrieve` boosts a Critical/High memory's
//!   score (x1.4/x1.2) so it can outrank a closer but less important match. This facade keeps the
//!   one filter that is a real content-safety decision (importance=0 / Discard is never returned)
//!   but does not yet replicate the boost -- a disclosed simplification, not a silent drop.
//!
//! [`RoutingMemTree::retrieve`] resolves near-tied cosine scores using occurrence-chain neighbor
//! context (`routing_occurrences`, `NEAR_TIE_EPSILON`) rather than leaving tied candidates in
//! whatever order the tree's float-precision descent happened to produce.

use anyhow::{Context, Result};
use finch_routing_tree::{load_routing_tree, AdaptiveTopKResult, RoutingConfig, RoutingTree};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use uuid::Uuid;

/// Two retrieval candidates are a near-tie when their cosine scores differ by no more than this:
/// close enough that raw ranking order between them is noise from `RoutingTree`'s float-precision
/// descent rather than a meaningful preference, so [`RoutingMemTree::retrieve`] falls back to a
/// secondary signal (occurrence-chain neighbor context) to order them instead. Set an order of
/// magnitude below `MemoryConfig::min_relevance_score`'s default per-result floor (`0.15`, in
/// `lib.rs`) -- that floor already treats scores within roughly this range as interchangeable
/// "good enough" matches for injection purposes, so this reuses the same scale rather than
/// introducing an unrelated tolerance.
const NEAR_TIE_EPSILON: f32 = 0.01;

/// Fixed, not random: every process that opens the same store must seed identical
/// candidate/stability-gate randomness for a freshly-built or freshly-reloaded tree to behave
/// identically to what produced the persisted structure it's continuing.
pub(crate) const FIXED_SEED: u64 = 0x46_49_4e_43_48; // "FINCH", ascii bytes -- a real constant, not a magic-looking one

pub(crate) type PointId = u64;

#[derive(Debug, Clone)]
pub(crate) struct PointMeta {
    pub text: String,
    pub importance: u8,
    pub created_at: i64,
}

/// What an insert did -- `MemTree::InsertEffect`'s shape, minus `promotion` (see module doc).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoutingInsertEffect {
    pub point_id: PointId,
    pub deduplicated: bool,
}

/// One conversational occurrence of a memory point, decoupled from `routing_points`' own
/// text-level dedup (`routing_occurrences` in `schema.sql`).
///
/// A point's identity answers "what was said"; an occurrence's identity answers "which time it
/// was said" -- repeated identical text (e.g. "hello" recurring across a conversation) gets its
/// own independent `point_id` and embedding every time via [`RoutingMemTree::insert_occurrence`]
/// (never collapsed onto an earlier occurrence's point just because the text matches -- that is
/// `insert_with_effect`'s dedup, and the occurrence path deliberately does not use it), and each
/// occurrence also gets its own occurrence row, uuid, and place in the `prev`/`next` chain, so
/// "what was said right before/after this" stays answerable even when the text itself is not
/// unique.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Occurrence {
    pub uuid: Uuid,
    pub point_id: PointId,
    pub prev: Option<Uuid>,
    pub next: Option<Uuid>,
}

/// [`RoutingMemTree::link_next`] found `prev` already linked to a different `next_uuid` --
/// a real, expected outcome when two callers race to continue from the same occurrence (e.g. two
/// frontend processes both resuming from the same last-known turn), not corruption. Whichever
/// caller's `UPDATE` lands first wins; every other racer observes this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinkConflict;

impl std::fmt::Display for LinkConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "routing_occurrences: prev was already linked to a different next_uuid (0 rows affected)"
        )
    }
}

impl std::error::Error for LinkConflict {}

/// [`RoutingMemTree::link_next`]'s failure modes, kept distinct on purpose: [`Self::Conflict`]
/// is the real, expected zero-rows-affected race (see [`LinkConflict`]), while [`Self::Sql`] is
/// a genuine SQLite failure -- most notably the busy-timeout budget (`open_connection`'s
/// `busy_timeout` pragma) being exhausted under sustained contention, but also I/O or corruption.
/// Conflating the two would hide a real failure as if it were the benign race, or vice versa.
#[derive(Debug)]
pub(crate) enum LinkNextError {
    Conflict(LinkConflict),
    Sql(rusqlite::Error),
}

impl std::fmt::Display for LinkNextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkNextError::Conflict(e) => write!(f, "{e}"),
            LinkNextError::Sql(e) => write!(
                f,
                "routing_occurrences: sqlite error linking next_uuid: {e}"
            ),
        }
    }
}

impl std::error::Error for LinkNextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LinkNextError::Conflict(e) => Some(e),
            LinkNextError::Sql(e) => Some(e),
        }
    }
}

impl From<rusqlite::Error> for LinkNextError {
    fn from(e: rusqlite::Error) -> Self {
        LinkNextError::Sql(e)
    }
}

pub(crate) struct RoutingMemTree {
    tree: RoutingTree,
    meta: HashMap<PointId, PointMeta>,
    /// Exact-text dedup index, mirroring `MemTree::find_leaf_by_text` -- content already stored
    /// gets its importance raised in place (if the new occurrence is more important) instead of a
    /// second point.
    text_index: HashMap<String, PointId>,
}

impl RoutingMemTree {
    pub(crate) fn new_with_dim(dim: usize) -> Self {
        Self {
            tree: RoutingTree::new(RoutingConfig::default(), dim, FIXED_SEED),
            meta: HashMap::new(),
            text_index: HashMap::new(),
        }
    }

    /// Rebuild from durable rows via `finch_routing_tree::load_routing_tree`. `created_at`
    /// is not tracked by that function's own metadata tuple (text, importance only), so it is
    /// re-read here directly -- a second, small query rather than widening that function's return
    /// shape for one caller's own bookkeeping need.
    ///
    /// Disclosed narrowing: `text_index` is rebuilt here from every persisted point
    /// indiscriminately, including ones originally created through
    /// [`Self::insert_occurrence`]'s non-deduping path -- `routing_points` records no provenance
    /// of which insert path produced a row, so after a reload, a later plain `insert_with_effect`
    /// call may dedup onto a point that started life as an occurrence, something that could never
    /// happen within the same process before a reload. `insert_occurrence` itself is unaffected
    /// either way: it never consults `text_index`, so every occurrence still gets its own point
    /// on both sides of a reload. Adding provenance tracking to fully close this gap was judged
    /// out of scope for the tie-break/dedup-removal work that introduced it.
    pub(crate) fn load(conn: &Connection, dim: usize) -> Result<Self> {
        let (tree, points) = load_routing_tree(conn, RoutingConfig::default(), dim, FIXED_SEED)?;
        let mut meta = HashMap::with_capacity(points.len());
        let mut text_index = HashMap::with_capacity(points.len());
        for (point_id, text, importance) in points {
            let pid = point_id as PointId;
            let created_at: i64 = conn.query_row(
                "SELECT created_at FROM routing_points WHERE point_id = ?1",
                [pid as i64],
                |r| r.get(0),
            )?;
            text_index.insert(text.clone(), pid);
            meta.insert(
                pid,
                PointMeta {
                    text,
                    importance,
                    created_at,
                },
            );
        }
        Ok(Self {
            tree,
            meta,
            text_index,
        })
    }

    pub(crate) fn insert_with_effect(
        &mut self,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
        created_at: i64,
    ) -> RoutingInsertEffect {
        if let Some(&existing) = self.text_index.get(&text) {
            if let Some(m) = self.meta.get_mut(&existing) {
                if importance > m.importance {
                    m.importance = importance;
                    // The tree itself has no notion of importance, so nothing there needs marking
                    // dirty -- only the point-metadata side changed, and the caller persists that
                    // via `save_point` the same way it does for a brand new point.
                }
            }
            return RoutingInsertEffect {
                point_id: existing,
                deduplicated: true,
            };
        }
        let point_id = self.tree.insert(embedding) as PointId;
        self.text_index.insert(text.clone(), point_id);
        self.meta.insert(
            point_id,
            PointMeta {
                text,
                importance,
                created_at,
            },
        );
        RoutingInsertEffect {
            point_id,
            deduplicated: false,
        }
    }

    /// Insert a brand-new point unconditionally: no `text_index` lookup, and the new point is
    /// never added to `text_index` either. Used only by [`Self::insert_occurrence`], so that an
    /// occurrence's point can never be the dedup target of a later `insert_with_effect` call
    /// (the plain, non-occurrence insert path), nor can a later occurrence insert collapse onto
    /// an existing `insert_with_effect`-created point just because the text matches. The two
    /// insert paths stay fully independent of each other's dedup in both directions, within one
    /// process's lifetime -- see [`Self::load`]'s doc for the one disclosed way that independence
    /// narrows after a reload.
    fn insert_new_point(
        &mut self,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
        created_at: i64,
    ) -> PointId {
        let point_id = self.tree.insert(embedding) as PointId;
        self.meta.insert(
            point_id,
            PointMeta {
                text,
                importance,
                created_at,
            },
        );
        point_id
    }

    /// Insert one conversational turn's text/embedding and record it as a fresh occurrence,
    /// distinct from the point-layer text dedup `insert_with_effect` does for its own callers.
    ///
    /// Unlike `insert_with_effect`, this never dedups by text: every call mints both a brand-new
    /// point/embedding (via [`Self::insert_new_point`]) and a brand-new occurrence row (a new
    /// `Uuid::new_v4`), regardless of whether the text has been seen before, so "hello" said at
    /// two different points in a conversation is two distinct occurrences with two distinct
    /// points, each individually addressable by its own uuid and linkable to its neighbors via
    /// `prev`/`next`. That is a deliberate behavior change from an earlier version of this
    /// method, which called `insert_with_effect` and let identical text collapse onto one point
    /// (bumping its importance in place on a repeat) -- for occurrences, "bump importance in
    /// place on duplicate" is gone on purpose: two occurrences of the same text are two distinct
    /// conversational moments, not the same fact restated, and retrieval's own neighbor-context
    /// tie-break (see [`Self::retrieve`]) depends on each occurrence keeping its own embedding
    /// and place in the chain to be useful as a comparison point.
    ///
    /// `prev` is taken directly from the caller -- the caller already knows its own last turn's
    /// uuid (its own previous `insert_occurrence` or `link_next` call), so this never queries for
    /// a "current tail"; there is no such notion here; see the module-level design note this
    /// mirrors in `crates/finch-memory/AGENTS.md`'s sibling design conversation. `next` is always
    /// `None` at creation; a later call links this occurrence forward via [`Self::link_next`].
    pub(crate) fn insert_occurrence(
        &mut self,
        conn: &Connection,
        text: String,
        embedding: Vec<f32>,
        importance: u8,
        created_at: i64,
        prev: Option<Uuid>,
    ) -> Result<Occurrence> {
        let point_id = self.insert_new_point(text, embedding, importance, created_at);
        let uuid = Uuid::new_v4();
        conn.execute(
            "INSERT INTO routing_occurrences (uuid, point_id, prev_uuid, next_uuid, created_at)
             VALUES (?1, ?2, ?3, NULL, ?4)",
            params![
                uuid.to_string(),
                point_id as i64,
                prev.map(|u| u.to_string()),
                created_at,
            ],
        )
        .context("insert_occurrence: insert routing_occurrences row")?;
        Ok(Occurrence {
            uuid,
            point_id,
            prev,
            next: None,
        })
    }

    /// Link `prev`'s occurrence forward to `next`, atomically and without reading a "current
    /// tail" first (there is no such notion -- see [`Self::insert_occurrence`]).
    ///
    /// A single `UPDATE ... WHERE uuid = ?2 AND next_uuid IS NULL` is the entire operation: no
    /// separate read-then-write, so nothing can race between the read and the write. Two callers
    /// racing to continue from the same `prev` is a real, expected outcome (not corruption) --
    /// exactly one wins and the loser gets [`LinkNextError::Conflict`], distinct from
    /// [`LinkNextError::Sql`] (a real SQLite failure, e.g. the busy-timeout budget exhausted
    /// under contention, or I/O/corruption) so a caller -- and this crate's own hostile-
    /// concurrency test -- never mistakes a transient busy failure for the real zero-rows-
    /// affected race.
    ///
    /// Takes `conn` explicitly rather than `&self`/`&mut self`: unlike `insert_occurrence`, this
    /// never touches the in-memory tree, only the `routing_occurrences` table, so nothing here
    /// needs a `RoutingMemTree` at all. Kept as an associated function on `RoutingMemTree` (rather
    /// than a bare free function) so both occurrence operations are found in one place.
    pub(crate) fn link_next(
        conn: &Connection,
        prev: Uuid,
        next: Uuid,
    ) -> Result<(), LinkNextError> {
        let affected = conn.execute(
            "UPDATE routing_occurrences SET next_uuid = ?1 WHERE uuid = ?2 AND next_uuid IS NULL",
            params![next.to_string(), prev.to_string()],
        )?;
        match affected {
            1 => Ok(()),
            0 => Err(LinkNextError::Conflict(LinkConflict)),
            n => unreachable!(
                "UPDATE routing_occurrences ... WHERE uuid = ?2 can affect at most one row \
                 (uuid is the PRIMARY KEY); got {n} affected rows linking prev={prev}"
            ),
        }
    }

    /// Top-k real candidates by cosine similarity, importance=0 (Discard) excluded -- the one
    /// `MemTree::retrieve` filter this facade keeps (module doc: no boost re-ranking yet) -- with
    /// near-tied candidates (`NEAR_TIE_EPSILON`) reordered by occurrence-chain neighbor context
    /// (see [`Self::break_near_ties`]) before the final `top_k` truncation, so a genuine near-tie
    /// can change which candidates survive the cut, not only their order within it.
    ///
    /// Takes `conn` because the tie-break needs `routing_occurrences` (never needed by this
    /// method before); everything else it uses (the tree, `self.meta`) was already in hand.
    pub(crate) fn retrieve(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<(PointId, String, f32)>> {
        if top_k == 0 {
            return Ok(Vec::new());
        }
        // Oversample: some of the tree's real top-k may be importance=0 and get filtered below,
        // so ask for a wider pool before truncating to what the caller actually asked for.
        let AdaptiveTopKResult {
            top_k: candidates, ..
        } = self
            .tree
            .descend_adaptive_top_k(query_embedding, top_k + 16, false);
        let mut results: Vec<(PointId, String, f32)> = candidates
            .into_iter()
            .filter_map(|c| {
                let pid = c.point_id as PointId;
                let m = self.meta.get(&pid)?;
                if m.importance == 0 {
                    return None;
                }
                Some((pid, m.text.clone(), c.cos as f32))
            })
            .collect();

        self.break_near_ties(conn, query_embedding, &mut results)
            .context("retrieve: neighbor-context tie-break")?;

        results.truncate(top_k);
        Ok(results)
    }

    /// Reorder every near-tied run within `results` (already sorted descending by cosine score,
    /// the tree's own order) by neighbor context. A run is a maximal stretch of adjacent entries
    /// whose score is within `NEAR_TIE_EPSILON` of the run's own best (first) entry; runs of
    /// length 1 are left untouched (nothing to break a tie against).
    fn break_near_ties(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        results: &mut [(PointId, String, f32)],
    ) -> Result<()> {
        let mut start = 0;
        while start < results.len() {
            let mut end = start + 1;
            while end < results.len() && results[start].2 - results[end].2 <= NEAR_TIE_EPSILON {
                end += 1;
            }
            if end - start > 1 {
                self.reorder_tie_run(conn, query_embedding, &mut results[start..end])?;
            }
            start = end;
        }
        Ok(())
    }

    /// Stable-sort one near-tie run by descending neighbor-context score
    /// ([`Self::neighbor_context_score`]), computed once per candidate. A candidate with no
    /// usable neighbor context (no occurrence row, or an occurrence with neither `prev` nor
    /// `next` set) sorts after every candidate that does have one; ties among no-context
    /// candidates, and ties among equal-context candidates, keep their original (cosine) relative
    /// order -- `sort_by` is stable and the `Equal` arms below rely on that.
    fn reorder_tie_run(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        run: &mut [(PointId, String, f32)],
    ) -> Result<()> {
        let mut scored: Vec<(Option<f32>, (PointId, String, f32))> = Vec::with_capacity(run.len());
        for entry in run.iter() {
            let ctx = self.neighbor_context_score(conn, query_embedding, entry.0)?;
            scored.push((ctx, entry.clone()));
        }
        scored.sort_by(|a, b| match (a.0, b.0) {
            (Some(x), Some(y)) => y.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        for (slot, (_, entry)) in run.iter_mut().zip(scored) {
            *slot = entry;
        }
        Ok(())
    }

    /// A candidate's tie-break signal: the average cosine similarity between `query_embedding`
    /// and the embedding(s) of whichever of this point's occurrence's `prev`/`next` neighbors
    /// exist and still resolve to a live (non-removed) point. Averaged rather than best-of when
    /// both neighbors exist, so a candidate is not rewarded for one lucky neighbor while its
    /// surrounding conversation as a whole is off-topic.
    ///
    /// `None` -- the documented "can't benefit from tie-breaking" case, not an error -- when this
    /// point has no `routing_occurrences` row at all (never inserted via `insert_occurrence`),
    /// when its occurrence has neither `prev_uuid` nor `next_uuid` set (first or last turn of its
    /// chain), or when every linked neighbor's point has since been [`Self::remove`]d.
    fn neighbor_context_score(
        &self,
        conn: &Connection,
        query_embedding: &[f32],
        point_id: PointId,
    ) -> Result<Option<f32>> {
        let occurrence: Option<(Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT prev_uuid, next_uuid FROM routing_occurrences WHERE point_id = ?1 LIMIT 1",
                params![point_id as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("neighbor_context_score: query routing_occurrences by point_id")?;
        let Some((prev_uuid, next_uuid)) = occurrence else {
            return Ok(None);
        };

        let mut scores = Vec::with_capacity(2);
        for neighbor_uuid in [prev_uuid, next_uuid].into_iter().flatten() {
            let Some(neighbor_point_id) = self.occurrence_point_id(conn, &neighbor_uuid)? else {
                continue;
            };
            if self.meta.contains_key(&neighbor_point_id) {
                let neighbor_embedding = self.tree.embedding_of(neighbor_point_id as usize);
                scores.push(crate::cosine_similarity(
                    neighbor_embedding,
                    query_embedding,
                ));
            }
        }
        if scores.is_empty() {
            return Ok(None);
        }
        Ok(Some(scores.iter().sum::<f32>() / scores.len() as f32))
    }

    /// The point_id a `routing_occurrences.uuid` resolves to, or `None` if the uuid has no row.
    fn occurrence_point_id(&self, conn: &Connection, uuid: &str) -> Result<Option<PointId>> {
        conn.query_row(
            "SELECT point_id FROM routing_occurrences WHERE uuid = ?1",
            params![uuid],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|opt| opt.map(|v| v as PointId))
        .context("occurrence_point_id: query routing_occurrences by uuid")
    }

    pub(crate) fn get_point(&self, point_id: PointId) -> Option<&PointMeta> {
        self.meta.get(&point_id)
    }

    /// Every live (non-removed) point's id, metadata, and canonical embedding -- for callers that
    /// need to scan the whole store (e.g. `conversation_summary`'s centroid windows), not the
    /// query-time hot path.
    pub(crate) fn iter_points(&self) -> impl Iterator<Item = (PointId, &PointMeta, &[f32])> {
        self.meta
            .iter()
            .map(|(&pid, m)| (pid, m, self.tree.embedding_of(pid as usize)))
    }

    pub(crate) fn remove(&mut self, point_id: PointId) -> anyhow::Result<()> {
        self.tree.remove_point(point_id as usize)?;
        if let Some(m) = self.meta.remove(&point_id) {
            self.text_index.remove(&m.text);
        }
        Ok(())
    }

    pub(crate) fn size(&self) -> usize {
        self.meta.len()
    }

    pub(crate) fn tree(&self) -> &RoutingTree {
        &self.tree
    }

    pub(crate) fn tree_mut(&mut self) -> &mut RoutingTree {
        &mut self.tree
    }
}

#[cfg(test)]
mod tests;
