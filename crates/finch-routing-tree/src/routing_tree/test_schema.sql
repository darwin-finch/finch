-- Test fixture ONLY: the routing-tree durable-row tables, copied verbatim from finch-memory's
-- `schema.sql` routing section. This crate does not own the authoritative schema -- in Finch the
-- caller creates these tables (finch-memory applies its own `schema.sql`). If the routing DDL
-- changes there, mirror it here or these tests stop testing what production runs against.
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
