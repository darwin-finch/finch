//! A binary routing tree: real, data-fitted split axes (candidate-selected PCA via successive
//! Hotelling deflation, not a random hyperplane), online/incremental construction (points insert
//! one at a time, never an offline batch fit), dual-insert boundary hedging, an incrementally
//! maintained real (undeflated) centroid per node enabling O(1) best-first adaptive search, and a
//! real removal/update primitive.
//!
//! Ported from `~/repos/fractal-corpus-curation`'s `source/routing_tree.d` (validated there through
//! `EXPERIMENT_LOG.md` §54, plus §55/§56's later structural closes), itself a fusion of Annoy's
//! (Spotify) hierarchical-binary-split-forest shape with CCIPCA-family fitted axes instead of random
//! hyperplanes, deflated level-by-level in the Hotelling-deflation sense. See
//! `BUILD_ARCHITECTURE.md` in that repo for the full validated-mechanism writeup this port follows.
//!
//! **Deliberately deferred, not dropped**: the D reference also has a learned per-node `wQ`/`wK`
//! self-play scoring layer (`self_play_router.d`) meant to replace the raw-projection routing
//! decision. Across every tested configuration in that repo (§39-44, §47) and in the sibling Rust
//! spike repo's own (separately confounded) numbers, the learned layer has not been shown to reliably
//! beat this plain structural mechanism — the validated win in the whole arc is entirely structural
//! (candidate-selected axes, spherical mode, dual-insert, stability-gated splitting, adaptive/beam
//! search). [`descend_beam`](RoutingTree::descend_beam) and
//! [`descend_adaptive`](RoutingTree::descend_adaptive) accept an optional branch-scoring closure for
//! exactly this reason — a future learned layer plugs in there without restructuring anything, the
//! same seam the D reference's own `BranchScorer` delegate provides.
//!
//! **Also deliberately omitted**: `RoutingConfig.onlineAxisRefinement` (keep refining a decision
//! node's axis after split time via CCIPCA, instead of freezing it forever) — the D reference itself
//! recommends leaving this off ("no evidence it's needed, real risk if enabled") and it is real,
//! disclosed extra complexity (`ccipcaAccum`/`ccipcaUpdate`) for a mechanism nothing here currently
//! calls. Portable later if a concrete need for it shows up; a disclosed gap, not a silent one.

use anyhow::{bail, Result};

/// Config knobs, with defaults matching the D reference's own validated recommendations
/// (`BUILD_ARCHITECTURE.md` §8), not its literal code defaults (which keep some of these off for
/// backward compatibility with pre-review tests that don't apply to a fresh port).
#[derive(Debug, Clone)]
pub struct RoutingConfig {
    /// First-guess judgment call, not independently calibrated for any specific corpus.
    pub leaf_capacity: usize,
    /// The label-free split-accept threshold on `fraction_explained`. Calibrated against real
    /// d=384 BGE embeddings specifically — provably wrong at low dimension (uniform noise gets
    /// accepted as a real split at every sample size up to 500 points at dim=4), since
    /// `fraction_explained` is not near zero for any unimodal distribution; only high-dimensional
    /// concentration-of-measure makes it a real signal. Recalibrate before trusting this at a
    /// different embedding dimensionality.
    pub discrimination_gate_threshold: f64,
    /// How many successive-Hotelling-deflation PC candidates `try_split` tries before picking
    /// whichever discriminates best.
    pub max_split_candidates: usize,
    /// A later candidate (PC2, PC3, ...) only replaces PC1 if it beats PC1's own
    /// `fraction_explained` by MORE than this margin, not by any amount — real small-sample
    /// overfitting was found from switching on any improvement on tiny (n=leaf_capacity+1) buckets.
    pub candidate_switch_margin: f64,
    /// Unit-normalize every point/query at insert and query time, and renormalize the residual
    /// after each level's deflation. Real embeddings are typically not unit-norm, and linear
    /// deflation shrinks a residual's magnitude by a point-specific amount, so without this,
    /// different points' residuals drift onto incomparable scales after a few levels. A clean,
    /// mostly-consistent win in the D reference's own measurement (§48) — recommended on.
    pub spherical_mode: bool,
    /// A point whose margin at a decision node is below this threshold also gets inserted into the
    /// OTHER child (dual-insert, physically searchable from both sides), marked structural-only —
    /// excluded from that node's own future axis-fitting and leaf-capacity counting. The single
    /// largest accuracy lever measured in the D reference (§50: single-path routing jumped 43%
    /// relative at threshold=0.10) — but a real, substantial cost: the canonical embedding is
    /// stored exactly once per point regardless of dual-insert count, but leaf-membership fans out
    /// (2.43x more membership rows at threshold=0.10 on that repo's corpus). `0.0` disables
    /// dual-insert entirely. Calibrate against a real storage/query-cost budget, don't default this
    /// on blindly.
    pub dual_insert_threshold: f64,
    /// Before committing a split, independently fit PC1 on two random halves of the same bucket and
    /// require they agree beyond the real, derivable chance baseline for random unit vectors in
    /// `dim` dimensions (mean 0, std = 1/sqrt(dim) exactly) — not a corpus-specific pick, a genuine
    /// statistical significance bound. Real, measured fix for small-sample axis overfitting at
    /// near-root splits (D reference §33/§54: confident-wrong-split rate dropped by more than a
    /// third). Recommended on.
    pub stability_gated_splitting: bool,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            leaf_capacity: 10,
            discrimination_gate_threshold: 0.15,
            max_split_candidates: 3,
            candidate_switch_margin: 0.05,
            spherical_mode: true,
            dual_insert_threshold: 0.0,
            stability_gated_splitting: true,
        }
    }
}

/// A branch-selection hook: given a decision node and the query as it arrives there (already
/// deflated by every ancestor), decide whether to favor the right child and how costly taking the
/// OTHER child would be. `None` (the default at every call site) uses the tree's own raw-projection
/// decision. The seam a future learned scorer plugs into.
pub type BranchScorer<'a> = dyn FnMut(usize, &[f64]) -> (bool, f64) + 'a;

#[derive(Clone, Copy)]
struct DualEntry {
    leaf_id: usize,
    divergence_node_id: usize,
}

struct Node {
    is_leaf: bool,
    left: Option<usize>,
    right: Option<usize>,
    /// Running mean of the fit (non-dual) points that arrived here BEFORE this node became a
    /// decision node — meaningful once `is_leaf == false`, the split's own centering point.
    anchor: Vec<f64>,
    /// Unit-norm axis, frozen forever once set.
    direction: Vec<f64>,
    /// Total tree-wide point count at the moment this node split — staleness measurement, also
    /// reused by `remove_point`'s dual-chain downdate walk to know whether a point's dual entry
    /// predates or postdates this split.
    split_at_global_count: usize,
    /// Leaf-only parallel arrays, meaningful while `is_leaf`.
    bucket_ids: Vec<usize>,
    bucket_deflated: Vec<Vec<f64>>,
    bucket_is_dual: Vec<bool>,
    /// Real (undeflated) running-mean centroid and visit count, meaningful on EVERY node (leaf or
    /// decision), updated on every insert that visits this node (primary or dual). Counts exactly
    /// one visit per point per node PER DISTINCT WALK that passes through it.
    real_centroid: Vec<f64>,
    real_count: usize,
    parent: Option<usize>,
}

impl Node {
    fn leaf() -> Self {
        Self {
            is_leaf: true,
            left: None,
            right: None,
            anchor: Vec::new(),
            direction: Vec::new(),
            split_at_global_count: 0,
            bucket_ids: Vec::new(),
            bucket_deflated: Vec::new(),
            bucket_is_dual: Vec::new(),
            real_centroid: Vec::new(),
            real_count: 0,
            parent: None,
        }
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn norm(a: &[f64]) -> f64 {
    dot(a, a).sqrt()
}

fn to_double(x: &[f32]) -> Vec<f64> {
    x.iter().map(|&v| v as f64).collect()
}

fn normalize_in_place(x: &mut [f64]) {
    let n = norm(x);
    if n < 1e-12 {
        return;
    }
    for v in x.iter_mut() {
        *v /= n;
    }
}

/// `dot(x - anchor, direction)`, computed as a single literal loop everywhere a projection is
/// needed so every call site produces bit-identical floating point results (matters for the
/// axis-orthogonality regression, which checks exact zero cosines).
fn projection(x: &[f64], anchor: &[f64], direction: &[f64]) -> f64 {
    let mut proj = 0.0;
    for i in 0..x.len() {
        proj += (x[i] - anchor[i]) * direction[i];
    }
    proj
}

/// Deflates `x` in place by `direction`/`anchor`: removes the component of `x` already explained
/// by this node's own axis. `renormalize` (spherical mode) rescales the residual back to unit
/// length after the subtraction, since linear subtraction alone shrinks `x` by a point-specific
/// amount.
fn deflate(x: &mut [f64], anchor: &[f64], direction: &[f64], renormalize: bool) {
    let proj = projection(x, anchor, direction);
    for i in 0..x.len() {
        x[i] -= proj * direction[i];
    }
    if renormalize {
        normalize_in_place(x);
    }
}

fn deflate_all(pts: &[Vec<f64>], direction: &[f64], dim: usize) -> Vec<Vec<f64>> {
    pts.iter()
        .map(|p| {
            let proj = dot(p, direction);
            (0..dim).map(|i| p[i] - proj * direction[i]).collect()
        })
        .collect()
}

/// The label-free discrimination quantity: bisect `centered` by the sign of its projection onto
/// `direction`, compute `(n*sigma_sq_parent - within_total) / (n*sigma_sq_parent)` — the standard
/// 1-D total = between + within variance decomposition, mathematically identical to a labeled
/// 2-class Fisher/LDA discriminant without ever touching a label.
///
/// This is NOT near zero for an arbitrary unimodal distribution bisected at its own median (a 1-D
/// uniform split at its mean gives exactly 0.75, analytically, any sample size) — it only becomes a
/// meaningful "real two-cluster structure" signal at high dimensionality, where concentration of
/// measure makes a single linear axis capturing most of a unimodal blob's variance genuinely rare.
fn fraction_explained(centered: &[Vec<f64>], direction: &[f64]) -> f64 {
    let n = centered.len();
    let proj: Vec<f64> = centered.iter().map(|c| dot(c, direction)).collect();
    let mut left_idx = Vec::new();
    let mut right_idx = Vec::new();
    for (j, &p) in proj.iter().enumerate() {
        if p >= 0.0 {
            right_idx.push(j);
        } else {
            left_idx.push(j);
        }
    }
    if left_idx.is_empty() || right_idx.is_empty() {
        return 0.0;
    }
    let sigma_sq_parent = proj.iter().map(|p| p * p).sum::<f64>() / n as f64;
    let sigma_sq_side = |idx: &[usize]| -> f64 {
        let m = idx.iter().map(|&j| proj[j]).sum::<f64>() / idx.len() as f64;
        idx.iter().map(|&j| (proj[j] - m).powi(2)).sum::<f64>() / idx.len() as f64
    };
    let sigma_sq_left = sigma_sq_side(&left_idx);
    let sigma_sq_right = sigma_sq_side(&right_idx);
    let within_total = left_idx.len() as f64 * sigma_sq_left + right_idx.len() as f64 * sigma_sq_right;
    if sigma_sq_parent > 1e-12 {
        (n as f64 * sigma_sq_parent - within_total) / (n as f64 * sigma_sq_parent)
    } else {
        0.0
    }
}

/// SplitMix64 (Steele/Lea/Flood 2014) — a small, dependency-free, deterministic generator. Quality
/// is irrelevant here (this only seeds power-iteration starting vectors and a stability-gate
/// shuffle, never anything security-sensitive); determinism from a fixed seed is what matters, and
/// this crate otherwise carries no randomness dependency at all.
fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

fn splitmix64_uniform_half(state: &mut u64) -> f64 {
    let bits = splitmix64_next(state) >> 11; // 53 significant bits
    let unit = bits as f64 * (1.0 / (1u64 << 53) as f64); // [0, 1)
    unit - 0.5 // [-0.5, 0.5)
}

fn random_unit_vector(dim: usize, seed: u64) -> Vec<f64> {
    let mut v = vec![0.0; dim];
    let mut n2 = 0.0;
    for (i, slot) in v.iter_mut().enumerate() {
        let mut local_state = seed ^ (i as u64).wrapping_mul(0x9E3779B97F4A7C15);
        let val = splitmix64_uniform_half(&mut local_state);
        *slot = val;
        n2 += val * val;
    }
    let n = n2.sqrt();
    if n > 1e-12 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
    v
}

/// Power iteration for the dominant eigenvector of `X^T X` (X = the centered point matrix),
/// computed without forming the d x d matrix: `X^T X v` as `X^T (X v)`, O(n*d) per iteration. 25
/// fixed iterations is a generous, convergence-checked budget for the small n this runs on
/// (`<= leaf_capacity + 1`).
fn power_iteration_top_eigenvector(centered: &[Vec<f64>], dim: usize, seed_salt: u64) -> Vec<f64> {
    let mut v = random_unit_vector(dim, seed_salt ^ 0xD1B54A32D192ED03);
    for _ in 0..25 {
        let xv: Vec<f64> = centered.iter().map(|c| dot(c, &v)).collect();
        let mut xtxv = vec![0.0; dim];
        for (j, cj) in centered.iter().enumerate() {
            for i in 0..dim {
                xtxv[i] += xv[j] * cj[i];
            }
        }
        let nrm = norm(&xtxv);
        if nrm < 1e-12 {
            break; // degenerate (e.g. all points identical) -- keep whatever v currently is
        }
        for i in 0..dim {
            v[i] = xtxv[i] / nrm;
        }
    }
    v
}

fn downdate_centroid(node: &mut Node, pt: &[f32], dim: usize) {
    let rc = node.real_count;
    if rc <= 1 {
        for v in node.real_centroid.iter_mut() {
            *v = 0.0;
        }
        node.real_count = 0;
    } else {
        for i in 0..dim {
            node.real_centroid[i] = (node.real_centroid[i] * rc as f64 - pt[i] as f64) / (rc - 1) as f64;
        }
        node.real_count = rc - 1;
    }
}

/// Result of [`RoutingTree::descend_adaptive`]: the best real candidate found, plus a MEASURED
/// (not imposed-budget) node-visit count.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveResult {
    pub best_point_id: Option<usize>,
    pub best_cos: f64,
    pub nodes_visited: usize,
}

struct AdaptiveState {
    nodes_visited: usize,
    best_so_far_cos: f64,
    best_point_id: Option<usize>,
    done: bool,
}

/// One candidate in a [`RoutingTree::descend_adaptive_top_k`] result, sorted descending by `cos`.
#[derive(Debug, Clone, Copy)]
pub struct TopKCandidate {
    pub point_id: usize,
    pub cos: f64,
}

pub struct AdaptiveTopKResult {
    /// Sorted descending by `cos`, length <= k (fewer only if the tree has fewer real points).
    pub top_k: Vec<TopKCandidate>,
    pub nodes_visited: usize,
}

struct TopKState {
    top_k: Vec<TopKCandidate>,
    k: usize,
    nodes_visited: usize,
    done: bool,
}

impl TopKState {
    fn offer(&mut self, point_id: usize, cos: f64) {
        if self.top_k.len() < self.k {
            self.top_k.push(TopKCandidate { point_id, cos });
            self.top_k.sort_by(|a, b| b.cos.partial_cmp(&a.cos).unwrap());
        } else if cos > self.top_k[self.top_k.len() - 1].cos {
            let last = self.top_k.len() - 1;
            self.top_k[last] = TopKCandidate { point_id, cos };
            self.top_k.sort_by(|a, b| b.cos.partial_cmp(&a.cos).unwrap());
        }
        if self.top_k.len() >= self.k && self.top_k[self.top_k.len() - 1].cos >= 1.0 - 1e-9 {
            self.done = true;
        }
    }

    fn kth_best_cos(&self) -> f64 {
        if self.top_k.len() >= self.k {
            self.top_k[self.top_k.len() - 1].cos
        } else {
            -2.0 // always explore until k real candidates exist
        }
    }
}

pub struct RoutingTree {
    cfg: RoutingConfig,
    nodes: Vec<Node>,
    /// Original, NEVER-deflated embeddings, indexed by point id. Stored exactly once per point
    /// regardless of how many leaves' `bucket_ids` reference it via dual-insert.
    points: Vec<Vec<f32>>,
    root_id: usize,
    dim: usize,
    rng_state: u64,

    // Per-point bookkeeping for remove_point. Indexed by point id, never shrinks -- point ids are
    // permanent slots; removal tombstones the slot rather than renumbering.
    current_leaf_of: Vec<Option<usize>>,
    removed_flag: Vec<bool>,
    dual_entries_of: Vec<Vec<DualEntry>>,

    /// Node ids whose persisted columns have changed since the last `mark_persisted` -- the same
    /// dirty-tracking pattern `MemTree` already uses (mark-on-mutation, save-marked,
    /// clear-on-hydrate), so `finch-memory`'s existing "don't rewrite the whole store on one new
    /// memory" persistence discipline carries over unchanged in shape, even though the SQL for
    /// this node shape is new. A single insert can dirty many ancestors at once (every node on
    /// the path has its `real_centroid` updated) -- a real, larger fan-out than `MemTree`'s own
    /// per-insert dirty set, a disclosed behavioral difference, not an oversight.
    dirty: std::collections::HashSet<usize>,
}

impl RoutingTree {
    pub fn new(cfg: RoutingConfig, dim: usize, seed: u64) -> Self {
        Self {
            cfg,
            nodes: vec![Node::leaf()],
            points: Vec::new(),
            root_id: 0,
            dim,
            rng_state: seed,
            current_leaf_of: Vec::new(),
            removed_flag: Vec::new(),
            dual_entries_of: Vec::new(),
            dirty: std::collections::HashSet::new(),
        }
    }

    /// Node ids whose persisted columns have changed, ascending -- a copy, not a drain, matching
    /// `MemTree::dirty_nodes`'s own safety discipline (a save that fails or is cancelled before
    /// committing leaves the marks set; the next save writes them).
    pub(crate) fn dirty_node_ids(&self) -> Vec<usize> {
        let mut ids: Vec<usize> = self.dirty.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Forget exactly the ids a transaction committed -- never the whole set, since another
    /// caller may have marked more nodes while this save was writing.
    pub(crate) fn mark_persisted(&mut self, ids: &[usize]) {
        for id in ids {
            self.dirty.remove(id);
        }
    }

    pub(crate) fn parent_of(&self, id: usize) -> Option<usize> {
        self.nodes[id].parent
    }

    pub(crate) fn real_centroid_raw(&self, id: usize) -> &[f64] {
        &self.nodes[id].real_centroid
    }

    pub(crate) fn spherical_mode(&self) -> bool {
        self.cfg.spherical_mode
    }

    /// `(point_id, is_dual, divergence_node_id)` for every entry currently in leaf `node_id`'s
    /// bucket -- the persistence layer's view of a leaf's membership, independent of the
    /// `bucket_deflated` cache (which is never persisted).
    pub(crate) fn membership_of(&self, node_id: usize) -> Vec<(usize, bool, Option<usize>)> {
        let node = &self.nodes[node_id];
        node.bucket_ids
            .iter()
            .zip(node.bucket_is_dual.iter())
            .map(|(&pid, &is_dual)| {
                let divergence = if is_dual { self.dual_entries_of[pid].iter().find(|e| e.leaf_id == node_id).map(|e| e.divergence_node_id) } else { None };
                (pid, is_dual, divergence)
            })
            .collect()
    }

    /// Installs fully-reconstructed state from a hydration pass, replacing everything. Rebuilds
    /// `current_leaf_of`/`dual_entries_of` from `memberships`
    /// (`leaf_node_id, point_id, is_dual, divergence_node_id`) and clears the dirty set, since
    /// freshly loaded state IS by definition what's durable -- matching `MemTree`'s own hydration
    /// convention (`clear_dirty`'s doc comment: "the tree now matches the durable rows").
    pub(crate) fn install_loaded_state(&mut self, nodes: Vec<Node>, points: Vec<Vec<f32>>, removed_flag: Vec<bool>, memberships: &[(usize, usize, bool, Option<usize>)]) {
        let n_points = points.len();
        self.nodes = nodes;
        self.points = points;
        self.removed_flag = removed_flag;
        self.current_leaf_of = vec![None; n_points];
        self.dual_entries_of = vec![Vec::new(); n_points];
        for &(leaf_id, point_id, is_dual, divergence_node_id) in memberships {
            if is_dual {
                self.dual_entries_of[point_id].push(DualEntry { leaf_id, divergence_node_id: divergence_node_id.expect("a dual membership row must carry a divergence node id") });
            } else {
                self.current_leaf_of[point_id] = Some(leaf_id);
            }
        }
        self.dirty.clear();
    }

    pub fn root(&self) -> usize {
        self.root_id
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_leaf(&self, id: usize) -> bool {
        self.nodes[id].is_leaf
    }

    pub fn left_of(&self, id: usize) -> Option<usize> {
        self.nodes[id].left
    }

    pub fn right_of(&self, id: usize) -> Option<usize> {
        self.nodes[id].right
    }

    pub fn direction_of(&self, id: usize) -> &[f64] {
        &self.nodes[id].direction
    }

    pub fn anchor_of(&self, id: usize) -> &[f64] {
        &self.nodes[id].anchor
    }

    pub fn split_at_global_count_of(&self, id: usize) -> usize {
        self.nodes[id].split_at_global_count
    }

    pub fn bucket_of(&self, id: usize) -> &[usize] {
        &self.nodes[id].bucket_ids
    }

    pub fn embedding_of(&self, point_id: usize) -> &[f32] {
        &self.points[point_id]
    }

    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Diagnostic/test use only.
    pub fn real_count_of(&self, id: usize) -> usize {
        self.nodes[id].real_count
    }

    /// Real, undeflated centroid for a node in the ORIGINAL embedding space -- its own `anchor` if
    /// a decision node (already the batch mean), else the running mean of its own leaf bucket's
    /// real embeddings. O(1): `real_centroid`/`real_count` are maintained incrementally on every
    /// insert, never recomputed by walking the subtree.
    pub fn centroid_of(&self, node_id: usize) -> Vec<f64> {
        if self.nodes[node_id].real_centroid.is_empty() {
            vec![0.0; self.dim]
        } else {
            self.nodes[node_id].real_centroid.clone()
        }
    }

    /// Cheaper alternative to [`Self::centroid_of`]: the node's own already-deflated representation
    /// (its `anchor` for a decision node, or the mean of a leaf's own `bucket_deflated` entries) —
    /// O(bucket size) instead of O(1), but a residual-space quantity, not directly comparable to a
    /// real, full-embedding-space cosine the way `centroid_of` is.
    pub fn deflated_centroid_of(&self, node_id: usize) -> Vec<f64> {
        if !self.nodes[node_id].is_leaf {
            return self.nodes[node_id].anchor.clone();
        }
        let bucket = &self.nodes[node_id].bucket_deflated;
        if bucket.is_empty() {
            return vec![0.0; self.dim];
        }
        let mut mean = vec![0.0; self.dim];
        for b in bucket {
            for i in 0..self.dim {
                mean[i] += b[i];
            }
        }
        let n = bucket.len() as f64;
        for v in mean.iter_mut() {
            *v /= n;
        }
        mean
    }

    /// Every point id stored under `node_id`'s subtree -- diagnostic use, not on the hot path.
    pub fn collect_subtree_ids(&self, node_id: usize) -> Vec<usize> {
        if self.nodes[node_id].is_leaf {
            return self.nodes[node_id].bucket_ids.clone();
        }
        let mut ids = self.collect_subtree_ids(self.nodes[node_id].left.unwrap());
        ids.extend(self.collect_subtree_ids(self.nodes[node_id].right.unwrap()));
        ids
    }

    /// Insert one point, descending from the root. Returns the assigned point id (permanent, never
    /// reused/renumbered) -- callers doing update/removal need it to track their own external key
    /// (e.g. a memory's own row id) to current point id across an update
    /// (`remove_point(old_id)` then `insert(new_embedding)`).
    pub fn insert(&mut self, point: Vec<f32>) -> usize {
        let point_id = self.points.len();
        let mut x = to_double(&point);
        if self.cfg.spherical_mode {
            normalize_in_place(&mut x);
        }
        self.points.push(point);
        self.current_leaf_of.push(None);
        self.dual_entries_of.push(Vec::new());
        self.removed_flag.push(false);
        self.insert_into(self.root_id, point_id, x, false, None);
        point_id
    }

    /// `is_dual`: true for a structural-only dual-insert copy -- searchable via `bucket_of`, but
    /// excluded from `try_split`'s axis-fitting and from `leaf_capacity` counting. A dual copy does
    /// not itself trigger further dual-inserts deeper in its own subtree (one level of hedging, not
    /// cascading) -- but the PRIMARY walk can spawn a dual copy at EVERY qualifying node along its
    /// own path, not just once.
    fn insert_into(&mut self, start_node_id: usize, point_id: usize, mut x: Vec<f64>, is_dual: bool, dual_divergence_node: Option<usize>) {
        let mut node_id = start_node_id;
        loop {
            if self.nodes[node_id].real_centroid.is_empty() {
                self.nodes[node_id].real_centroid = vec![0.0; self.dim];
            }
            {
                let pt = &self.points[point_id];
                self.nodes[node_id].real_count += 1;
                let new_count = self.nodes[node_id].real_count as f64;
                for i in 0..self.dim {
                    let old = self.nodes[node_id].real_centroid[i];
                    self.nodes[node_id].real_centroid[i] += (pt[i] as f64 - old) / new_count;
                }
            }
            self.dirty.insert(node_id);

            if self.nodes[node_id].is_leaf {
                self.nodes[node_id].bucket_ids.push(point_id);
                self.nodes[node_id].bucket_deflated.push(x);
                self.nodes[node_id].bucket_is_dual.push(is_dual);
                if is_dual {
                    self.dual_entries_of[point_id].push(DualEntry {
                        leaf_id: node_id,
                        divergence_node_id: dual_divergence_node.expect("dual insert must carry its divergence node"),
                    });
                } else {
                    self.current_leaf_of[point_id] = Some(node_id);
                    let real_count = self.nodes[node_id].bucket_is_dual.iter().filter(|&&d| !d).count();
                    if real_count > self.cfg.leaf_capacity {
                        self.try_split(node_id);
                    }
                }
                return;
            }

            let dir = self.nodes[node_id].direction.clone();
            let anchor = self.nodes[node_id].anchor.clone();
            let proj = projection(&x, &anchor, &dir);
            let margin = proj.abs();
            let (favored_id, other_id) = if proj >= 0.0 {
                (self.nodes[node_id].right.unwrap(), self.nodes[node_id].left.unwrap())
            } else {
                (self.nodes[node_id].left.unwrap(), self.nodes[node_id].right.unwrap())
            };

            let mut x_next = x.clone();
            deflate(&mut x_next, &anchor, &dir, self.cfg.spherical_mode);

            if !is_dual && self.cfg.dual_insert_threshold > 0.0 && margin < self.cfg.dual_insert_threshold {
                self.insert_into(other_id, point_id, x_next.clone(), true, Some(node_id));
            }

            node_id = favored_id;
            x = x_next;
        }
    }

    fn shuffle(&mut self, items: &mut [usize]) {
        for i in (1..items.len()).rev() {
            let r = splitmix64_next(&mut self.rng_state);
            let j = (r % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }

    /// Discrimination-gated splitting with candidate selection: only called on an oversized leaf.
    /// Generates up to `max_split_candidates` directions via successive Hotelling deflation, scores
    /// each by `fraction_explained` against the fit set, and keeps whichever scores best (PC1 stays
    /// the default prior unless a later candidate beats it by more than `candidate_switch_margin`).
    /// Gated by `stability_gated_splitting` (if enabled) and `discrimination_gate_threshold` before
    /// ever freezing a split -- an oversized leaf that fails either gate stays an oversized flat
    /// leaf, rechecked on the next insert.
    fn try_split(&mut self, node_id: usize) {
        let bucket_ids = self.nodes[node_id].bucket_ids.clone();
        let bucket_x = self.nodes[node_id].bucket_deflated.clone();
        let bucket_is_dual = self.nodes[node_id].bucket_is_dual.clone();
        let n = bucket_ids.len();

        let fit_idx: Vec<usize> = (0..n).filter(|&j| !bucket_is_dual[j]).collect();
        let n_fit = fit_idx.len();
        if n_fit == 0 {
            return; // an all-dual bucket can't fit an axis; shouldn't happen since leaf_capacity counts non-dual only
        }

        let mut mean = vec![0.0; self.dim];
        for &j in &fit_idx {
            for i in 0..self.dim {
                mean[i] += bucket_x[j][i];
            }
        }
        for v in mean.iter_mut() {
            *v /= n_fit as f64;
        }

        let centered: Vec<Vec<f64>> = fit_idx.iter().map(|&j| (0..self.dim).map(|i| bucket_x[j][i] - mean[i]).collect()).collect();
        let all_centered: Vec<Vec<f64>> = (0..n).map(|j| (0..self.dim).map(|i| bucket_x[j][i] - mean[i]).collect()).collect();

        let mut best_fraction = -1.0;
        let mut best_direction = Vec::new();
        let mut residual = centered.clone();
        for candidate_idx in 0..self.cfg.max_split_candidates {
            let seed_salt = (node_id as u64) ^ (candidate_idx as u64).wrapping_mul(0x9E3779B97F4A7C15);
            let candidate = power_iteration_top_eigenvector(&residual, self.dim, seed_salt);
            let fraction = fraction_explained(&centered, &candidate);
            if candidate_idx == 0 || fraction > best_fraction + self.cfg.candidate_switch_margin {
                best_fraction = fraction;
                best_direction = candidate.clone();
            }
            if candidate_idx + 1 < self.cfg.max_split_candidates {
                residual = deflate_all(&residual, &candidate, self.dim);
            }
        }

        if self.cfg.stability_gated_splitting {
            if n_fit >= 4 {
                let mut shuffled = fit_idx.clone();
                self.shuffle(&mut shuffled);
                let half = shuffled.len() / 2;
                let half_a: Vec<Vec<f64>> = shuffled[..half].iter().map(|&j| (0..self.dim).map(|i| bucket_x[j][i] - mean[i]).collect()).collect();
                let half_b: Vec<Vec<f64>> = shuffled[half..].iter().map(|&j| (0..self.dim).map(|i| bucket_x[j][i] - mean[i]).collect()).collect();
                let dir_a = power_iteration_top_eigenvector(&half_a, self.dim, (node_id as u64) ^ 0x51ED270B3E31A3AD);
                let dir_b = power_iteration_top_eigenvector(&half_b, self.dim, (node_id as u64) ^ 0x9E92F1C4A7B03D5F);
                let agreement = dot(&dir_a, &dir_b).abs();
                let chance_sigma = 1.0 / (self.dim as f64).sqrt();
                let significance_sigmas = 3.0;
                if agreement < significance_sigmas * chance_sigma {
                    return; // not yet stable -- defer, let the bucket keep growing
                }
            } else {
                return; // too few points to even check stability -- defer
            }
        }

        let mut left_idx = Vec::new();
        let mut right_idx = Vec::new();
        for j in 0..n {
            if dot(&all_centered[j], &best_direction) >= 0.0 {
                right_idx.push(j);
            } else {
                left_idx.push(j);
            }
        }
        if left_idx.is_empty() || right_idx.is_empty() {
            return; // a degenerate all-one-side split can't discriminate anything
        }
        if best_fraction <= self.cfg.discrimination_gate_threshold {
            return; // leave as an oversized flat leaf
        }

        let mut left_node = Node::leaf();
        let mut right_node = Node::leaf();
        for &j in &left_idx {
            let mut xd = bucket_x[j].clone();
            deflate(&mut xd, &mean, &best_direction, self.cfg.spherical_mode);
            left_node.bucket_ids.push(bucket_ids[j]);
            left_node.bucket_deflated.push(xd);
            left_node.bucket_is_dual.push(bucket_is_dual[j]);
        }
        for &j in &right_idx {
            let mut xd = bucket_x[j].clone();
            deflate(&mut xd, &mean, &best_direction, self.cfg.spherical_mode);
            right_node.bucket_ids.push(bucket_ids[j]);
            right_node.bucket_deflated.push(xd);
            right_node.bucket_is_dual.push(bucket_is_dual[j]);
        }

        // Seed each new child's real_centroid/real_count from the REAL (undeflated) embeddings of
        // the points just redistributed into it -- without this, a freshly-split child reads as the
        // zero vector via centroid_of's own empty-case fallback until enough NEW points arrive.
        seed_real_centroid(&mut left_node, &left_idx, &bucket_ids, &self.points, self.dim);
        seed_real_centroid(&mut right_node, &right_idx, &bucket_ids, &self.points, self.dim);

        left_node.parent = Some(node_id);
        right_node.parent = Some(node_id);
        let left_id = self.nodes.len();
        self.nodes.push(left_node);
        let right_id = self.nodes.len();
        self.nodes.push(right_node);

        for &j in &left_idx {
            let pid = bucket_ids[j];
            if bucket_is_dual[j] {
                self.retarget_dual_entry(pid, node_id, left_id);
            } else {
                self.current_leaf_of[pid] = Some(left_id);
            }
        }
        for &j in &right_idx {
            let pid = bucket_ids[j];
            if bucket_is_dual[j] {
                self.retarget_dual_entry(pid, node_id, right_id);
            } else {
                self.current_leaf_of[pid] = Some(right_id);
            }
        }

        let split_at_global_count = self.points.len();
        let node = &mut self.nodes[node_id];
        node.is_leaf = false;
        node.anchor = mean;
        node.direction = best_direction;
        node.split_at_global_count = split_at_global_count;
        node.left = Some(left_id);
        node.right = Some(right_id);
        node.bucket_ids = Vec::new();
        node.bucket_deflated = Vec::new();
        node.bucket_is_dual = Vec::new();

        self.dirty.insert(node_id);
        self.dirty.insert(left_id);
        self.dirty.insert(right_id);
    }

    /// A point's tracked current leaf moves from `old_leaf_id` to `new_leaf_id` on split -- a dual
    /// entry is matched back by its OLD leaf id (not by position, since a point can carry several
    /// dual entries at once and only the one actually stored at this leaf should retarget).
    fn retarget_dual_entry(&mut self, point_id: usize, old_leaf_id: usize, new_leaf_id: usize) {
        for entry in self.dual_entries_of[point_id].iter_mut() {
            if entry.leaf_id == old_leaf_id {
                entry.leaf_id = new_leaf_id;
                return;
            }
        }
        panic!("retarget_dual_entry: no matching DualEntry record for point {point_id} at leaf {old_leaf_id}");
    }

    /// Removes a point from the tree: downdates `real_centroid`/`real_count` at exactly the nodes
    /// it currently contributes to (primary path root->leaf, plus any dual-insert path), via the
    /// exact algebraic inverse of the running-mean update `insert_into` used going in, then drops it
    /// from whichever leaf bucket(s) currently hold it.
    ///
    /// Does NOT retroactively re-fit any already-frozen split axis -- a split's axis stays exactly
    /// as fit even once the points that justified it are removed. The tree remains structurally
    /// valid and searchable either way; whether stale axes measurably hurt routing quality after
    /// heavy real-corpus editing is a disclosed, unmeasured limitation, not a bug.
    pub fn remove_point(&mut self, point_id: usize) -> Result<()> {
        if self.removed_flag[point_id] {
            bail!("remove_point: point {point_id} already removed");
        }
        let pt = self.points[point_id].clone();

        {
            let mut cur = self.current_leaf_of[point_id].expect("a never-removed point always has a primary leaf");
            loop {
                downdate_centroid(&mut self.nodes[cur], &pt, self.dim);
                self.dirty.insert(cur);
                if cur == self.root_id {
                    break;
                }
                cur = self.nodes[cur].parent.expect("a non-root node always has a parent");
            }
            self.remove_from_bucket(self.current_leaf_of[point_id].unwrap(), point_id)?;
        }

        let dual_entries = self.dual_entries_of[point_id].clone();
        for entry in dual_entries {
            let mut cur = entry.leaf_id;
            loop {
                downdate_centroid(&mut self.nodes[cur], &pt, self.dim);
                self.dirty.insert(cur);
                let parent = self.nodes[cur].parent.expect("a dual leaf always has a parent (it is never the root)");
                if parent == entry.divergence_node_id {
                    break; // the shared divergence ancestor was already downdated once, by the primary walk above
                }
                cur = parent;
            }
            self.remove_from_bucket(entry.leaf_id, point_id)?;
        }

        self.removed_flag[point_id] = true;
        Ok(())
    }

    fn remove_from_bucket(&mut self, leaf_id: usize, point_id: usize) -> Result<()> {
        let node = &mut self.nodes[leaf_id];
        if let Some(pos) = node.bucket_ids.iter().position(|&id| id == point_id) {
            node.bucket_ids.remove(pos);
            node.bucket_deflated.remove(pos);
            node.bucket_is_dual.remove(pos);
            Ok(())
        } else {
            bail!("remove_from_bucket: point {point_id} not found in expected leaf {leaf_id}");
        }
    }

    /// Plain-direction routing: walks the tree using each visited decision node's own fixed
    /// `direction`, down to a leaf. The baseline every other search mechanism should be measured
    /// against, not just against brute force.
    pub fn descend_plain_direction(&self, query: &[f32]) -> usize {
        let mut x = to_double(query);
        if self.cfg.spherical_mode {
            normalize_in_place(&mut x);
        }
        let mut node_id = self.root_id;
        while !self.nodes[node_id].is_leaf {
            let anchor = self.nodes[node_id].anchor.clone();
            let dir = self.nodes[node_id].direction.clone();
            let go_right = projection(&x, &anchor, &dir) >= 0.0;
            deflate(&mut x, &anchor, &dir, self.cfg.spherical_mode);
            node_id = if go_right { self.nodes[node_id].right.unwrap() } else { self.nodes[node_id].left.unwrap() };
        }
        node_id
    }

    /// Global best-first search: treats taking the non-favored child as a real cost (`margin`),
    /// explores the `beam_width` cheapest reachable leaves within a total `max_expansions` budget,
    /// returns leaf ids (cheapest-path-first) for a caller to pool every returned leaf's bucket for
    /// a final real-cosine scoring pass. `beam_width=1` reproduces `descend_plain_direction`'s exact
    /// leaf, verified directly (see tests), not just assumed from the algorithm's shape.
    ///
    /// `scorer`, if given, REPLACES the default raw-projection cost with a caller-supplied one (the
    /// seam a future learned scorer plugs into) -- structural deflation for continuing the descent
    /// always still uses the tree's own axis regardless of `scorer`; only branch SELECTION is
    /// pluggable.
    pub fn descend_beam(&self, query: &[f32], beam_width: usize, max_expansions: usize, mut scorer: Option<&mut BranchScorer>) -> Vec<usize> {
        struct Candidate {
            cost: f64,
            node_id: usize,
            x: Vec<f64>,
        }
        let beam_width = beam_width.max(1);
        let mut qx = to_double(query);
        if self.cfg.spherical_mode {
            normalize_in_place(&mut qx);
        }
        let mut frontier = vec![Candidate { cost: 0.0, node_id: self.root_id, x: qx }];
        let mut leaves: Vec<usize> = Vec::with_capacity(beam_width);
        let mut expansions = 0usize;

        while !frontier.is_empty() && leaves.len() < beam_width && expansions < max_expansions {
            let mut best_idx = 0;
            for (i, c) in frontier.iter().enumerate() {
                if c.cost < frontier[best_idx].cost {
                    best_idx = i;
                }
            }
            let cur = frontier.swap_remove(best_idx);

            if self.nodes[cur.node_id].is_leaf {
                leaves.push(cur.node_id);
                continue;
            }
            expansions += 1;

            let dir = self.nodes[cur.node_id].direction.clone();
            let anchor = self.nodes[cur.node_id].anchor.clone();

            let (go_right, margin) = match scorer.as_deref_mut() {
                Some(s) => s(cur.node_id, &cur.x),
                None => {
                    let proj = projection(&cur.x, &anchor, &dir);
                    (proj >= 0.0, proj.abs())
                }
            };

            let mut x_deflated = cur.x.clone();
            deflate(&mut x_deflated, &anchor, &dir, self.cfg.spherical_mode);

            let (favored_id, other_id) = if go_right {
                (self.nodes[cur.node_id].right.unwrap(), self.nodes[cur.node_id].left.unwrap())
            } else {
                (self.nodes[cur.node_id].left.unwrap(), self.nodes[cur.node_id].right.unwrap())
            };

            frontier.push(Candidate { cost: cur.cost, node_id: favored_id, x: x_deflated.clone() });
            frontier.push(Candidate { cost: cur.cost + margin, node_id: other_id, x: x_deflated });
        }

        if leaves.len() < beam_width {
            frontier.sort_by(|a, b| a.cost.partial_cmp(&b.cost).unwrap());
            for c in frontier {
                if leaves.len() >= beam_width {
                    break;
                }
                if self.nodes[c.node_id].is_leaf {
                    leaves.push(c.node_id);
                }
            }
        }
        leaves
    }

    /// Threshold-triggered backtracking: ordinary single-path descent, only forking to explore the
    /// OTHER child when that node's own margin is below `closeness_threshold` -- the classic
    /// spill-tree "defeatist search with epsilon-neighborhood backtracking" (Liu et al. 2004), a
    /// structurally different mechanism from [`Self::descend_beam`]'s global best-first search
    /// (local threshold trigger, not a global budget). `max_backtracks` bounds total backtrack
    /// events across the whole descent.
    pub fn descend_with_backtrack(&self, query: &[f32], closeness_threshold: f64, max_backtracks: usize) -> Vec<usize> {
        let mut x0 = to_double(query);
        if self.cfg.spherical_mode {
            normalize_in_place(&mut x0);
        }
        let mut results = Vec::new();
        self.backtrack_descend(self.root_id, x0, closeness_threshold, max_backtracks, &mut results);
        results
    }

    fn backtrack_descend(&self, node_id: usize, x_cur: Vec<f64>, closeness_threshold: f64, backtracks_left: usize, results: &mut Vec<usize>) {
        if self.nodes[node_id].is_leaf {
            // Only reached when node_id == root_id (whole tree is one leaf).
            results.push(node_id);
            return;
        }
        let anchor = self.nodes[node_id].anchor.clone();
        let dir = self.nodes[node_id].direction.clone();
        let proj = projection(&x_cur, &anchor, &dir);
        let margin = proj.abs();
        let mut x_next = x_cur.clone();
        deflate(&mut x_next, &anchor, &dir, self.cfg.spherical_mode);
        let (favored_id, other_id) = if proj >= 0.0 {
            (self.nodes[node_id].right.unwrap(), self.nodes[node_id].left.unwrap())
        } else {
            (self.nodes[node_id].left.unwrap(), self.nodes[node_id].right.unwrap())
        };

        if margin < closeness_threshold && backtracks_left > 0 {
            if self.nodes[other_id].is_leaf {
                results.push(other_id);
            } else {
                self.backtrack_descend(other_id, x_next.clone(), closeness_threshold, backtracks_left - 1, results);
            }
        }

        if self.nodes[favored_id].is_leaf {
            results.push(favored_id);
        } else {
            self.backtrack_descend(favored_id, x_next, closeness_threshold, backtracks_left, results);
        }
    }

    fn adaptive_score_leaf(&self, leaf_id: usize, q_orig: &[f64], q_orig_norm: f64, state: &mut AdaptiveState) {
        let mut leaf_best_pid = None;
        let mut leaf_best_cos = -2.0;
        for &pid in &self.nodes[leaf_id].bucket_ids {
            let pt = to_double(&self.points[pid]);
            let c = if q_orig_norm > 1e-12 { dot(q_orig, &pt) / (q_orig_norm * norm(&pt)) } else { 0.0 };
            if c > leaf_best_cos {
                leaf_best_cos = c;
                leaf_best_pid = Some(pid);
            }
        }
        if let Some(pid) = leaf_best_pid {
            if leaf_best_cos > state.best_so_far_cos {
                state.best_so_far_cos = leaf_best_cos;
                state.best_point_id = Some(pid);
                if state.best_so_far_cos >= 1.0 - 1e-9 {
                    state.done = true;
                }
            }
        }
    }

    /// Real adaptive best-first search, no fixed budget or threshold: explores the favored branch
    /// first (establishing a real candidate, scored by actual cosine against the query's own
    /// ORIGINAL undeflated embedding), then only descends into the OTHER branch at a node if
    /// something over there COULD beat the best real candidate found so far -- compared via real
    /// cosine against that branch's own real, undeflated centroid (O(1) via `centroid_of`), not the
    /// locally-projected margin (a real, measured scale mismatch when compared directly: the
    /// D-reference measured median projected margin 0.057 vs. median real cosine gap 0.37, a ~6.5x
    /// mismatch that made an earlier, wrong version explore almost regardless of quality). No
    /// calibration constant -- "is one real cosine bigger than another" needs no scale-matching.
    /// Explicit early exit once `best_so_far_cos` is already close enough to 1.0 that nothing could
    /// improve on it. The current recommended default for real deployment.
    pub fn descend_adaptive(&self, query: &[f32], mut scorer: Option<&mut BranchScorer>, use_deflated_prune_test: bool) -> AdaptiveResult {
        let mut x0 = to_double(query);
        if self.cfg.spherical_mode {
            normalize_in_place(&mut x0);
        }
        let q_orig = to_double(query);
        let q_orig_norm = norm(&q_orig);
        let mut state = AdaptiveState { nodes_visited: 0, best_so_far_cos: -2.0, best_point_id: None, done: false };
        self.adaptive_descend(self.root_id, x0, &q_orig, q_orig_norm, use_deflated_prune_test, scorer.as_deref_mut(), &mut state);
        AdaptiveResult { best_point_id: state.best_point_id, best_cos: state.best_so_far_cos, nodes_visited: state.nodes_visited }
    }

    #[allow(clippy::too_many_arguments)]
    fn adaptive_descend(
        &self,
        node_id: usize,
        x_cur: Vec<f64>,
        q_orig: &[f64],
        q_orig_norm: f64,
        use_deflated_prune_test: bool,
        mut scorer: Option<&mut BranchScorer>,
        state: &mut AdaptiveState,
    ) {
        if state.done {
            return;
        }
        if self.nodes[node_id].is_leaf {
            self.adaptive_score_leaf(node_id, q_orig, q_orig_norm, state);
            return;
        }
        state.nodes_visited += 1;
        let anchor = self.nodes[node_id].anchor.clone();
        let dir = self.nodes[node_id].direction.clone();

        let go_right = match scorer.as_deref_mut() {
            Some(s) => s(node_id, &x_cur).0,
            None => projection(&x_cur, &anchor, &dir) >= 0.0,
        };

        let mut x_next = x_cur.clone();
        deflate(&mut x_next, &anchor, &dir, self.cfg.spherical_mode);

        let (favored_id, other_id) = if go_right {
            (self.nodes[node_id].right.unwrap(), self.nodes[node_id].left.unwrap())
        } else {
            (self.nodes[node_id].left.unwrap(), self.nodes[node_id].right.unwrap())
        };

        // Favored FIRST -- establishes/improves best_so_far_cos before the other branch's prune
        // decision is even made.
        self.adaptive_descend(favored_id, x_next.clone(), q_orig, q_orig_norm, use_deflated_prune_test, scorer.as_deref_mut(), state);

        if !state.done {
            let other_cos = if use_deflated_prune_test {
                let other_centroid = self.deflated_centroid_of(other_id);
                let xn_norm = norm(&x_next);
                let oc_norm = norm(&other_centroid);
                if xn_norm > 1e-12 && oc_norm > 1e-12 {
                    dot(&x_next, &other_centroid) / (xn_norm * oc_norm)
                } else {
                    0.0
                }
            } else {
                let other_centroid = self.centroid_of(other_id);
                let oc_norm = norm(&other_centroid);
                if q_orig_norm > 1e-12 && oc_norm > 1e-12 {
                    dot(q_orig, &other_centroid) / (q_orig_norm * oc_norm)
                } else {
                    0.0
                }
            };
            if other_cos > state.best_so_far_cos {
                self.adaptive_descend(other_id, x_next, q_orig, q_orig_norm, use_deflated_prune_test, scorer, state);
            }
        }
    }

    /// Top-k generalization of [`Self::descend_adaptive`]: identical mechanism, generalized from
    /// tracking a single best candidate to a bounded list of the k best found so far. The prune test
    /// changes from "could this beat my single best" to "could this beat the WORST of my current
    /// k-best" -- the standard k-NN generalization (KD-trees, ball trees, HNSW's own `ef`).
    pub fn descend_adaptive_top_k(&self, query: &[f32], k: usize, use_deflated_prune_test: bool) -> AdaptiveTopKResult {
        let mut x0 = to_double(query);
        if self.cfg.spherical_mode {
            normalize_in_place(&mut x0);
        }
        let q_orig = to_double(query);
        let q_orig_norm = norm(&q_orig);
        let mut state = TopKState { top_k: Vec::new(), k, nodes_visited: 0, done: false };
        self.top_k_descend(self.root_id, x0, &q_orig, q_orig_norm, use_deflated_prune_test, &mut state);
        AdaptiveTopKResult { top_k: state.top_k, nodes_visited: state.nodes_visited }
    }

    fn top_k_score_leaf(&self, leaf_id: usize, q_orig: &[f64], q_orig_norm: f64, state: &mut TopKState) {
        for &pid in &self.nodes[leaf_id].bucket_ids {
            let pt = to_double(&self.points[pid]);
            let c = if q_orig_norm > 1e-12 { dot(q_orig, &pt) / (q_orig_norm * norm(&pt)) } else { 0.0 };
            state.offer(pid, c);
        }
    }

    fn top_k_descend(&self, node_id: usize, x_cur: Vec<f64>, q_orig: &[f64], q_orig_norm: f64, use_deflated_prune_test: bool, state: &mut TopKState) {
        if state.done {
            return;
        }
        if self.nodes[node_id].is_leaf {
            self.top_k_score_leaf(node_id, q_orig, q_orig_norm, state);
            return;
        }
        state.nodes_visited += 1;
        let anchor = self.nodes[node_id].anchor.clone();
        let dir = self.nodes[node_id].direction.clone();
        let proj = projection(&x_cur, &anchor, &dir);
        let go_right = proj >= 0.0;

        let mut x_next = x_cur.clone();
        deflate(&mut x_next, &anchor, &dir, self.cfg.spherical_mode);

        let (favored_id, other_id) = if go_right {
            (self.nodes[node_id].right.unwrap(), self.nodes[node_id].left.unwrap())
        } else {
            (self.nodes[node_id].left.unwrap(), self.nodes[node_id].right.unwrap())
        };

        self.top_k_descend(favored_id, x_next.clone(), q_orig, q_orig_norm, use_deflated_prune_test, state);

        if !state.done {
            let other_cos = if use_deflated_prune_test {
                let other_centroid = self.deflated_centroid_of(other_id);
                let xn_norm = norm(&x_next);
                let oc_norm = norm(&other_centroid);
                if xn_norm > 1e-12 && oc_norm > 1e-12 {
                    dot(&x_next, &other_centroid) / (xn_norm * oc_norm)
                } else {
                    0.0
                }
            } else {
                let other_centroid = self.centroid_of(other_id);
                let oc_norm = norm(&other_centroid);
                if q_orig_norm > 1e-12 && oc_norm > 1e-12 {
                    dot(q_orig, &other_centroid) / (q_orig_norm * oc_norm)
                } else {
                    0.0
                }
            };
            if other_cos > state.kth_best_cos() {
                self.top_k_descend(other_id, x_next, q_orig, q_orig_norm, use_deflated_prune_test, state);
            }
        }
    }
}

fn seed_real_centroid(node: &mut Node, idx: &[usize], bucket_ids: &[usize], points: &[Vec<f32>], dim: usize) {
    if idx.is_empty() {
        return;
    }
    node.real_centroid = vec![0.0; dim];
    for &j in idx {
        let pt = &points[bucket_ids[j]];
        node.real_count += 1;
        let nc = node.real_count as f64;
        for i in 0..dim {
            let old = node.real_centroid[i];
            node.real_centroid[i] += (pt[i] as f64 - old) / nc;
        }
    }
}

mod persistence;

#[cfg(test)]
mod tests;
