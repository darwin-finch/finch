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

-- Tree nodes table (MemTree hierarchical structure)
-- node_id matches the MemTree's own NodeId (u64) for round-trip fidelity.
-- importance: 0=Discard, 1=Normal, 2=High, 3=Critical (see memory/quality.rs)
CREATE TABLE IF NOT EXISTS tree_nodes (
    node_id INTEGER PRIMARY KEY,
    parent_id INTEGER,
    text TEXT NOT NULL,
    embedding BLOB NOT NULL,  -- f32 array stored as little-endian bytes
    level INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    importance INTEGER NOT NULL DEFAULT 1,
    FOREIGN KEY (parent_id) REFERENCES tree_nodes(node_id)
);

-- Stable provenance for semantic leaves. A NULL node_id records that the
-- quality classifier deliberately excluded this conversation from semantic
-- retrieval, making projection retries idempotent as well.
-- `node_id` is deliberately NOT UNIQUE. The same sentence said in several
-- conversations deduplicates to one memory node with several sources, and a
-- promoted node's provenance rows follow the moved leaf that still holds the
-- text. A UNIQUE constraint here made storing repeated content fail outright
-- with `UNIQUE constraint failed: memory_sources.node_id`.
--
-- There is no migration for databases created with the old constraint. Finch
-- has no users yet, so this file is authoritative and a pre-existing
-- `~/.finch/memory.db` should be removed rather than upgraded. Once that stops
-- being true, changing this table needs a migration.
CREATE TABLE IF NOT EXISTS memory_sources (
    conversation_id TEXT PRIMARY KEY,
    node_id INTEGER,
    indexed_at INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (node_id) REFERENCES tree_nodes(node_id) ON DELETE CASCADE
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

-- Indexes for fast retrieval
CREATE INDEX IF NOT EXISTS idx_conversations_timestamp ON conversations(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_conversations_session ON conversations(session_id);
CREATE INDEX IF NOT EXISTS idx_tree_nodes_parent ON tree_nodes(parent_id);
CREATE INDEX IF NOT EXISTS idx_tree_nodes_level ON tree_nodes(level);
CREATE INDEX IF NOT EXISTS idx_tree_nodes_created ON tree_nodes(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_memory_sources_node ON memory_sources(node_id);
CREATE INDEX IF NOT EXISTS idx_program_registry_name ON program_registry(name, language, scope);
CREATE INDEX IF NOT EXISTS idx_program_registry_source_hash ON program_registry(source_hash);
