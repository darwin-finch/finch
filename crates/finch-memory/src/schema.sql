-- MemTree hierarchical memory schema
-- SQLite database for storing conversations and tree structure

-- Conversations table (stores all interactions)
CREATE TABLE IF NOT EXISTS conversations (
    id TEXT PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    role TEXT NOT NULL,  -- 'user' or 'assistant'
    content TEXT NOT NULL,
    tokens INTEGER,
    model TEXT,
    session_id TEXT,
    brain_id TEXT,
    run_id TEXT,
    request_seq INTEGER,
    created_at INTEGER NOT NULL
);

-- Stable provenance for semantic leaves. A NULL node_id records that the
-- quality classifier deliberately excluded this conversation from semantic
-- retrieval, making projection retries idempotent as well.
-- `node_id` is deliberately NOT UNIQUE. The same sentence said in several
-- conversations deduplicates to one memory node with several sources.
-- A UNIQUE constraint here made storing repeated content fail outright
-- with `UNIQUE constraint failed: memory_sources.node_id`.
--
-- `node_id` references `routing_points(point_id)`. RoutingMemTree's insert never
-- moves content to a different id the way MemTree's promotion could, so there is
-- no in-place `UPDATE memory_sources SET node_id = ...` fixup to perform after an
-- insert; a point's id is permanent from the moment it is assigned (see
-- routing_memory.rs's own module doc).
--
-- There is no migration for databases created against the old (tree_nodes) FK from
-- before RoutingTree replaced MemTree. Finch has no users yet, so this file is
-- authoritative and a pre-existing `~/.finch/memory.db` should be removed rather
-- than upgraded. Once that stops being true, changing this table needs a migration.
CREATE TABLE IF NOT EXISTS memory_sources (
    conversation_id TEXT PRIMARY KEY,
    node_id INTEGER,
    indexed_at INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (node_id) REFERENCES routing_points(point_id) ON DELETE CASCADE
);

-- Metadata for tracking system state
CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Lisp environment: successful (define ...) expressions, replayed on reattach.
-- seq is auto-increment so replay order matches definition order.
CREATE TABLE IF NOT EXISTS lisp_env (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    expr TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

-- Language-neutral executable vocabulary. Definitions are immutable by (id, version);
-- names resolve to the newest visible version for interactive use.
CREATE TABLE IF NOT EXISTS program_registry (
    id TEXT NOT NULL,
    version INTEGER NOT NULL,
    name TEXT NOT NULL,
    language TEXT NOT NULL,
    source TEXT NOT NULL,
    documentation TEXT NOT NULL DEFAULT '',
    signature TEXT,
    effect TEXT NOT NULL DEFAULT 'unclassified',
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    dependencies_json TEXT NOT NULL DEFAULT '[]',
    tests_json TEXT NOT NULL DEFAULT '[]',
    provenance TEXT NOT NULL,
    trust TEXT NOT NULL,
    scope TEXT NOT NULL,
    scope_key TEXT,
    source_hash TEXT NOT NULL,
    environment_hash TEXT NOT NULL,
    use_count INTEGER NOT NULL DEFAULT 0,
    success_count INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (id, version)
);

-- RoutingTree persistence (crates/finch-memory/src/routing_tree.rs) -- the sole memory index;
-- it replaced tree_nodes/MemTree outright rather than migrating it. Canonical point content
-- (text + embedding) is stored exactly once per point regardless of how many leaves reference
-- it via dual-insert; leaf membership is a lean set of (leaf, point) pointer rows, not
-- duplicated embedding data (a real
-- point made explicitly while sizing dual-insert's storage cost: dual-insert multiplies membership
-- rows, never embeddings). `routing_nodes.node_id` matches RoutingTree's own internal node index
-- (contiguous from 0, never reused). `bucket_deflated` (the per-node deflated residual cache) is
-- deliberately NOT persisted -- it's deterministically recomputable from a point's canonical
-- embedding plus the frozen `(anchor, direction)` chain of the nodes it passes through, so hydration
-- reconstructs it once rather than storing a second, derived copy of embedding-sized data per leaf
-- entry.
CREATE TABLE IF NOT EXISTS routing_points (
    point_id INTEGER PRIMARY KEY,
    text TEXT NOT NULL,
    embedding BLOB NOT NULL,  -- f32 array, little-endian, the CANONICAL never-deflated embedding
    importance INTEGER NOT NULL DEFAULT 1,
    removed INTEGER NOT NULL DEFAULT 0,  -- tombstone; point_id is never reused/renumbered
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS routing_nodes (
    node_id INTEGER PRIMARY KEY,
    parent_id INTEGER,
    is_leaf INTEGER NOT NULL,
    left_id INTEGER,
    right_id INTEGER,
    anchor BLOB,               -- f64 array, little-endian; NULL while is_leaf
    direction BLOB,            -- f64 array, little-endian; NULL while is_leaf, frozen forever once set
    split_at_global_count INTEGER NOT NULL DEFAULT 0,
    real_centroid BLOB NOT NULL,  -- f64 array, little-endian; incrementally maintained, never a walk-and-average
    real_count INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (parent_id) REFERENCES routing_nodes(node_id)
);

CREATE TABLE IF NOT EXISTS routing_leaf_membership (
    leaf_node_id INTEGER NOT NULL,
    point_id INTEGER NOT NULL,
    is_dual INTEGER NOT NULL DEFAULT 0,
    divergence_node_id INTEGER,  -- meaningful only when is_dual=1: the ancestor this dual copy branched off from
    PRIMARY KEY (leaf_node_id, point_id),
    FOREIGN KEY (leaf_node_id) REFERENCES routing_nodes(node_id),
    FOREIGN KEY (point_id) REFERENCES routing_points(point_id)
);

-- Indexes for fast retrieval
CREATE INDEX IF NOT EXISTS idx_routing_nodes_parent ON routing_nodes(parent_id);
CREATE INDEX IF NOT EXISTS idx_routing_leaf_membership_point ON routing_leaf_membership(point_id);
CREATE INDEX IF NOT EXISTS idx_conversations_timestamp ON conversations(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_conversations_session ON conversations(session_id);
CREATE INDEX IF NOT EXISTS idx_memory_sources_node ON memory_sources(node_id);
CREATE INDEX IF NOT EXISTS idx_program_registry_name ON program_registry(name, language, scope);
CREATE INDEX IF NOT EXISTS idx_program_registry_source_hash ON program_registry(source_hash);
