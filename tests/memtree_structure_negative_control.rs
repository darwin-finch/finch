//! Negative control for issue #250, on the base revision.
//!
//! These are the structural properties issue #250 requires, applied to the
//! unmodified `MemTree::insert`. They fail here, which is the recorded evidence
//! that the regressions added on `claude/issue-250-memtree-structure` reproduce
//! the measured defect rather than testing behavior that already held.
//!
//! Measured on the dogfood host before the fix: 16,782 nodes for 567 distinct
//! texts, one node with 13,650 children, 3,115 nodes with exactly one child,
//! and a 2,500-deep single-node chain.
//!
//! Only the pre-existing public API is used. Evidence only — never merge.

use finch::memory::MemTree;

fn unit_vector(axis: usize, dim: usize) -> Vec<f32> {
    let mut v = vec![0.0; dim];
    v[axis] = 1.0;
    v
}

#[test]
fn test_identical_text_is_not_duplicated() {
    let mut tree = MemTree::new_with_dim(8);
    let embedding = unit_vector(0, 8);

    tree.insert("same turn".to_string(), embedding.clone(), 1)
        .unwrap();
    let after_first = tree.size();

    for _ in 0..100 {
        tree.insert("same turn".to_string(), embedding.clone(), 1)
            .unwrap();
    }

    assert_eq!(
        tree.size(),
        after_first,
        "re-inserting identical text must not grow the tree; on the base \
         revision each repeat mints a new node and a fresh embedding, which \
         produced 16,782 nodes for 567 distinct texts"
    );
}

#[test]
fn test_repeated_identical_inserts_do_not_form_a_chain() {
    let mut tree = MemTree::new_with_dim(8);
    let embedding = unit_vector(0, 8);

    for _ in 0..200 {
        tree.insert("(say \"Hello\")".to_string(), embedding.clone(), 1)
            .unwrap();
    }

    let deepest = tree
        .all_nodes()
        .values()
        .map(|node| node.level)
        .max()
        .unwrap_or(0);

    assert!(
        deepest <= 2,
        "identical content must not deepen the tree; got depth {deepest}. \
         The measured store had a 2,525-level chain of one repeated message"
    );
}

#[test]
fn test_no_single_child_chains_form() {
    let mut tree = MemTree::new_with_dim(8);
    let embedding = unit_vector(0, 8);

    for i in 0..50 {
        tree.insert(format!("near duplicate {i}"), embedding.clone(), 1)
            .unwrap();
    }

    let single_child_nodes = tree
        .all_nodes()
        .values()
        .filter(|node| node.children.len() == 1)
        .count();

    assert_eq!(
        single_child_nodes, 0,
        "no node may have exactly one child; the measured store had 3,115 \
         such nodes forming chains"
    );
}
