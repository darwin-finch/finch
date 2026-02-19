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
    created_at INTEGER NOT NULL
);

-- Tree nodes table (MemTree hierarchical structure)
CREATE TABLE IF NOT EXISTS tree_nodes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER,
    text TEXT NOT NULL,
    embedding BLOB NOT NULL,  -- Store as binary blob (f32 array)
    level INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (parent_id) REFERENCES tree_nodes(id)
);

-- Metadata for tracking system state
CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Indexes for fast retrieval
CREATE INDEX IF NOT EXISTS idx_conversations_timestamp ON conversations(timestamp DESC);
CREATE INDEX IF NOT EXISTS idx_conversations_session ON conversations(session_id);
CREATE INDEX IF NOT EXISTS idx_tree_nodes_parent ON tree_nodes(parent_id);
CREATE INDEX IF NOT EXISTS idx_tree_nodes_level ON tree_nodes(level);
CREATE INDEX IF NOT EXISTS idx_tree_nodes_created ON tree_nodes(created_at DESC);
