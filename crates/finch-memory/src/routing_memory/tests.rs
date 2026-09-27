use super::*;
use std::sync::Barrier;

const DIM: usize = 8;

/// A `relative_margin_cutoff` (#1327) large enough that [`filter_by_relative_margin`] can never
/// drop anything, no matter how the corpus's own random-baseline sample happens to land -- for
/// tests exercising unrelated `retrieve` behavior (the near-tie-break machinery) that predates
/// #1327 and is not itself about margin filtering.
const CUTOFF_DISABLED: f32 = 1000.0;

fn embedding(seed: u32) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    v[(seed as usize) % DIM] = 1.0;
    v
}

/// Open a fresh temp-file-backed connection with the real schema applied, the same
/// `CREATE TABLE IF NOT EXISTS` initialization every production caller goes through
/// (`MemorySystem::new_with_connection`'s own sequence, minus the migrations this table doesn't
/// need). A real file, not `:memory:`, because the hostile-concurrency test below needs two
/// independent connections to actually contend with each other -- `:memory:` connections are
/// each their own private database.
fn open_schema_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("occurrences.db");
    let conn = crate::MemorySystem::open_connection(&path).expect("open_connection");
    conn.execute_batch(include_str!("../schema.sql"))
        .expect("apply schema.sql");
    (dir, path)
}

fn read_next_uuid(conn: &Connection, uuid: Uuid) -> Option<String> {
    conn.query_row(
        "SELECT next_uuid FROM routing_occurrences WHERE uuid = ?1",
        params![uuid.to_string()],
        |row| row.get(0),
    )
    .expect("row must exist")
}

fn read_point_id(conn: &Connection, uuid: Uuid) -> i64 {
    conn.query_row(
        "SELECT point_id FROM routing_occurrences WHERE uuid = ?1",
        params![uuid.to_string()],
        |row| row.get(0),
    )
    .expect("row must exist")
}

#[test]
fn test_insert_occurrence_mints_distinct_uuids_for_repeated_identical_text() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let first = tree
        .insert_occurrence(&conn, "hello".to_string(), embedding(0), 1, 100, None)
        .expect("first insert_occurrence");
    let second = tree
        .insert_occurrence(&conn, "hello".to_string(), embedding(0), 1, 200, None)
        .expect("second insert_occurrence");

    assert_ne!(
        first.uuid,
        second.uuid,
        "two occurrences of identical text (\"hello\" at created_at=100 and created_at=200) must \
         get distinct uuids -- that is the entire point of this feature; got the same uuid \
         {first_uuid} twice",
        first_uuid = first.uuid
    );
    assert_ne!(
        first.point_id, second.point_id,
        "insert_occurrence must NOT route through insert_with_effect's text dedup: two \
         occurrences of identical text (\"hello\") must each get their own independent \
         point_id/embedding, never collapsed onto the same point just because the text matches \
         (first={:?}, second={:?})",
        first, second
    );

    // Both rows are independently persisted and addressable, not just distinguishable in memory.
    assert_eq!(
        read_point_id(&conn, first.uuid),
        first.point_id as i64,
        "persisted routing_occurrences row for {} must record point_id {}",
        first.uuid,
        first.point_id
    );
    assert_eq!(
        read_point_id(&conn, second.uuid),
        second.point_id as i64,
        "persisted routing_occurrences row for {} must record point_id {}",
        second.uuid,
        second.point_id
    );
    assert_eq!(
        read_next_uuid(&conn, first.uuid),
        None,
        "next_uuid must be NULL immediately after insert_occurrence, before any link_next"
    );
}

#[test]
fn test_link_next_succeeds_on_fresh_prev_with_no_existing_next() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let prev = tree
        .insert_occurrence(&conn, "turn one".to_string(), embedding(1), 1, 100, None)
        .expect("insert prev");
    let next = tree
        .insert_occurrence(
            &conn,
            "turn two".to_string(),
            embedding(2),
            1,
            200,
            Some(prev.uuid),
        )
        .expect("insert next");

    let result = RoutingMemTree::link_next(&conn, prev.uuid, next.uuid);
    assert!(
        result.is_ok(),
        "link_next on a fresh prev ({}) with no existing next must succeed, got {:?}",
        prev.uuid,
        result
    );

    assert_eq!(
        read_next_uuid(&conn, prev.uuid),
        Some(next.uuid.to_string()),
        "prev's persisted next_uuid must equal the linked next after a successful link_next, \
         re-read fresh from the database"
    );
}

#[test]
fn test_link_next_second_attempt_on_already_linked_prev_returns_conflict() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let prev = tree
        .insert_occurrence(&conn, "turn one".to_string(), embedding(1), 1, 100, None)
        .expect("insert prev");
    let winner = tree
        .insert_occurrence(
            &conn,
            "turn two, winner".to_string(),
            embedding(2),
            1,
            200,
            Some(prev.uuid),
        )
        .expect("insert winner");
    let loser = tree
        .insert_occurrence(
            &conn,
            "turn two, loser".to_string(),
            embedding(3),
            1,
            201,
            Some(prev.uuid),
        )
        .expect("insert loser");

    let first_link = RoutingMemTree::link_next(&conn, prev.uuid, winner.uuid);
    assert!(
        first_link.is_ok(),
        "the first link_next on a fresh prev must succeed, got {:?}",
        first_link
    );

    let second_link = RoutingMemTree::link_next(&conn, prev.uuid, loser.uuid);
    match second_link {
        Err(LinkNextError::Conflict(_)) => {}
        other => panic!(
            "a second link_next on an already-linked prev ({}) must return \
             LinkNextError::Conflict, not {:?} -- this is a real, expected outcome, not a bug",
            prev.uuid, other
        ),
    }

    // The already-linked next_uuid must be untouched by the losing attempt.
    assert_eq!(
        read_next_uuid(&conn, prev.uuid),
        Some(winner.uuid.to_string()),
        "the losing link_next call must not overwrite the winner's next_uuid"
    );
}

/// Hostile-concurrency regression: two independent connections against the *same* database
/// file, both calling `link_next` with the *same* `prev` uuid but two *different* `next` uuids,
/// from real OS threads racing via a `Barrier` (not sequential calls -- sequential calls could
/// never exercise the zero-rows-affected race this proves). This is the production boundary the
/// feature exists for: two frontend processes both trying to continue a conversation from the
/// same last-known turn.
///
/// Asserts the exact-once terminal state required by this crate's testing convention: exactly
/// one caller observes `Ok(())`, the other observes `Err(LinkNextError::Conflict(_))` -- never
/// both-Ok (double link) and never both-Err (lost update) -- and the persisted winner, re-read
/// from a third, fresh connection (never from either racing caller's own belief about what it
/// wrote), matches whichever `next` the `Ok` caller actually sent.
///
/// A transient `SQLITE_BUSY` while the two writers serialize under WAL is expected to be
/// absorbed by `busy_timeout` (`MemorySystem::open_connection`) and must never surface as
/// `LinkNextError::Sql` here; if it did, this test would fail on `LinkNextError::Sql` rather
/// than resolve to the expected `Ok`/`Conflict` split, which is exactly the distinction this
/// assertion is checking.
#[test]
fn test_link_next_hostile_concurrency_exactly_one_winner() {
    let (_dir, path) = open_schema_db();

    let prev_uuid = Uuid::new_v4();
    let next_a = Uuid::new_v4();
    let next_b = Uuid::new_v4();

    // Seed the prev row directly: what matters for this race is routing_occurrences alone, not
    // how the row was produced.
    {
        let conn = Connection::open(&path).expect("seed connection");
        conn.execute(
            "INSERT INTO routing_occurrences (uuid, point_id, prev_uuid, next_uuid, created_at)
             VALUES (?1, 0, NULL, NULL, 0)",
            params![prev_uuid.to_string()],
        )
        .expect("seed prev row");
    }

    let barrier = std::sync::Arc::new(Barrier::new(2));

    let path_a = path.clone();
    let barrier_a = std::sync::Arc::clone(&barrier);
    let handle_a = std::thread::spawn(move || {
        let conn = crate::MemorySystem::open_connection(&path_a).expect("thread a open_connection");
        barrier_a.wait();
        RoutingMemTree::link_next(&conn, prev_uuid, next_a)
    });

    let path_b = path.clone();
    let barrier_b = std::sync::Arc::clone(&barrier);
    let handle_b = std::thread::spawn(move || {
        let conn = crate::MemorySystem::open_connection(&path_b).expect("thread b open_connection");
        barrier_b.wait();
        RoutingMemTree::link_next(&conn, prev_uuid, next_b)
    });

    let result_a = handle_a.join().expect("thread a panicked");
    let result_b = handle_b.join().expect("thread b panicked");

    // Neither result may be the transient-busy failure mode: busy_timeout must have absorbed
    // SQLITE_BUSY by waiting, not surfaced it as an error indistinguishable from a real conflict.
    for (label, result) in [("a", &result_a), ("b", &result_b)] {
        if let Err(LinkNextError::Sql(e)) = result {
            panic!(
                "thread {label}'s link_next returned a raw SQL error ({e:?}) instead of \
                 resolving to Ok or Conflict -- busy_timeout should have absorbed contention \
                 under WAL rather than surfacing SQLITE_BUSY here"
            );
        }
    }

    let winner = match (&result_a, &result_b) {
        (Ok(()), Err(LinkNextError::Conflict(_))) => next_a,
        (Err(LinkNextError::Conflict(_)), Ok(())) => next_b,
        other => panic!(
            "exactly one of two racing link_next calls on the same prev must win: expected \
             (Ok, Conflict) in one order, got {other:?}"
        ),
    };

    // Re-read from a fresh, third connection -- never from either racing caller's own belief.
    let verify_conn = Connection::open(&path).expect("verify connection");
    let persisted = read_next_uuid(&verify_conn, prev_uuid);
    assert_eq!(
        persisted,
        Some(winner.to_string()),
        "persisted next_uuid for prev={prev_uuid} must match whichever next actually won \
         (next_a={next_a}, next_b={next_b}, result_a={result_a:?}, result_b={result_b:?}), \
         re-read fresh from the database rather than trusted from either caller"
    );
}

#[test]
fn test_insert_occurrence_does_not_bump_importance_of_earlier_occurrence_on_duplicate_text() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let first = tree
        .insert_occurrence(&conn, "hello".to_string(), embedding(0), 1, 100, None)
        .expect("first insert_occurrence");
    let second = tree
        .insert_occurrence(&conn, "hello".to_string(), embedding(0), 5, 200, None)
        .expect("second insert_occurrence, higher importance");

    // `insert_with_effect`'s "bump importance in place on a text duplicate" behavior does not
    // apply here: these are two distinct points now, so the first occurrence's own importance
    // must be untouched by the second, more-important occurrence of the same text.
    let first_meta = tree
        .get_point(first.point_id)
        .expect("first point_id must still be resolvable");
    assert_eq!(
        first_meta.importance, 1,
        "the first occurrence's point (point_id={}) must keep its original importance (1); a \
         later, higher-importance occurrence of identical text (\"hello\", point_id={}, \
         importance=5) must not bump it in place -- that behavior belonged to \
         insert_with_effect's text dedup, which insert_occurrence no longer uses",
        first.point_id, second.point_id
    );
    let second_meta = tree
        .get_point(second.point_id)
        .expect("second point_id must be resolvable");
    assert_eq!(
        second_meta.importance, 5,
        "the second occurrence's own point (point_id={}) must carry its own importance (5)",
        second.point_id
    );
}

/// Retrieval tie-break regression: two candidates (`a`, `b`) whose raw cosine scores against the
/// query are within `NEAR_TIE_EPSILON` of each other -- `b`'s raw score is strictly higher than
/// `a`'s, so absent the tie-break, `retrieve` would return `[b, a]` in that order, purely by
/// cosine rank. Each candidate has one occurrence-chain neighbor (`prev`): `a`'s neighbor is
/// embedded close to the query, `b`'s neighbor is embedded orthogonal to it (zero similarity).
/// The tie-break must reverse the raw-cosine order and return `a` first, because `a`'s
/// surrounding conversational context matches the live query far better than `b`'s does.
#[test]
fn test_retrieve_prefers_near_tied_candidate_whose_occurrence_neighbor_matches_query_context() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    // Query: unit vector along dim 0.
    let query: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 1.0;
        v
    };

    // `a`: cos(a, query) ~= 0.98871
    let a_embedding: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 0.99;
        v[1] = 0.15;
        v
    };
    // `b`: cos(b, query) ~= 0.99499 -- strictly higher than `a`'s, but within NEAR_TIE_EPSILON.
    let b_embedding: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 0.995;
        v[1] = 0.10;
        v
    };
    // `a`'s neighbor: cos(neighbor_a, query) ~= 0.8944 -- close to the query.
    let neighbor_a_embedding: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 0.6;
        v[2] = 0.3;
        v
    };
    // `b`'s neighbor: orthogonal to the query -- cos(neighbor_b, query) == 0.0.
    let neighbor_b_embedding: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[3] = 0.5;
        v
    };

    let raw_cos_a = crate::cosine_similarity(&a_embedding, &query);
    let raw_cos_b = crate::cosine_similarity(&b_embedding, &query);
    assert!(
        raw_cos_b > raw_cos_a && raw_cos_b - raw_cos_a <= NEAR_TIE_EPSILON,
        "fixture sanity: b's raw cosine ({raw_cos_b}) must be strictly higher than a's \
         ({raw_cos_a}) but within NEAR_TIE_EPSILON ({NEAR_TIE_EPSILON}) -- otherwise this test \
         does not exercise a near-tie at all"
    );
    let ctx_a = crate::cosine_similarity(&neighbor_a_embedding, &query);
    let ctx_b = crate::cosine_similarity(&neighbor_b_embedding, &query);
    assert!(
        ctx_a - ctx_b > NEAR_TIE_EPSILON,
        "fixture sanity: a's neighbor context score ({ctx_a}) must clearly beat b's ({ctx_b}) \
         for this to be a meaningful tie-break test"
    );

    let neighbor_a = tree
        .insert_occurrence(
            &conn,
            "context near a".to_string(),
            neighbor_a_embedding,
            1,
            100,
            None,
        )
        .expect("insert neighbor_a");
    let a = tree
        .insert_occurrence(
            &conn,
            "candidate a".to_string(),
            a_embedding,
            1,
            101,
            Some(neighbor_a.uuid),
        )
        .expect("insert a");
    RoutingMemTree::link_next(&conn, neighbor_a.uuid, a.uuid).expect("link neighbor_a -> a");

    let neighbor_b = tree
        .insert_occurrence(
            &conn,
            "context near b".to_string(),
            neighbor_b_embedding,
            1,
            200,
            None,
        )
        .expect("insert neighbor_b");
    let b = tree
        .insert_occurrence(
            &conn,
            "candidate b".to_string(),
            b_embedding,
            1,
            201,
            Some(neighbor_b.uuid),
        )
        .expect("insert b");
    RoutingMemTree::link_next(&conn, neighbor_b.uuid, b.uuid).expect("link neighbor_b -> b");

    let results = tree
        .retrieve(&conn, &query, 2, CUTOFF_DISABLED)
        .expect("retrieve must succeed");

    assert_eq!(
        results.len(),
        2,
        "expected exactly the two near-tied candidates (a, b) in the top-2, got {results:?}"
    );
    assert_eq!(
        results[0].0, a.point_id,
        "a (point_id={}) must be ranked first: although b (point_id={}) has the strictly higher \
         raw cosine score (raw_cos_a={raw_cos_a}, raw_cos_b={raw_cos_b}, within \
         NEAR_TIE_EPSILON={NEAR_TIE_EPSILON}), a's occurrence-chain neighbor context score \
         ({ctx_a}) far exceeds b's ({ctx_b}) -- the near-tie must be resolved by neighbor \
         context, not left at raw-cosine order. Got results={results:?}",
        a.point_id, b.point_id
    );
    assert_eq!(
        results[1].0, b.point_id,
        "b (point_id={}) must be ranked second, got results={results:?}",
        b.point_id
    );
}

/// A candidate with no `routing_occurrences` row at all (never inserted via `insert_occurrence`)
/// must not crash or be excluded when it takes part in a near-tie -- it simply falls back to
/// whatever order the tie run already has it in (here, nothing reorders a two-element run when
/// neither element has usable context, so raw-cosine order is preserved).
#[test]
fn test_retrieve_tie_break_falls_back_gracefully_when_neither_candidate_has_occurrence_context() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let query: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 1.0;
        v
    };
    let a_embedding: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 0.99;
        v[1] = 0.15;
        v
    };
    let b_embedding: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 0.995;
        v[1] = 0.10;
        v
    };
    let raw_cos_a = crate::cosine_similarity(&a_embedding, &query);
    let raw_cos_b = crate::cosine_similarity(&b_embedding, &query);
    assert!(
        raw_cos_b - raw_cos_a <= NEAR_TIE_EPSILON,
        "fixture sanity: a and b must be a near-tie (raw_cos_a={raw_cos_a}, raw_cos_b={raw_cos_b})"
    );

    // Plain `insert_with_effect`, not `insert_occurrence`: neither point ever gets a
    // `routing_occurrences` row, so neither has neighbor context to break the tie with.
    let a_effect = tree.insert_with_effect("a".to_string(), a_embedding, 1, 100);
    let b_effect = tree.insert_with_effect("b".to_string(), b_embedding, 1, 200);

    let results = tree
        .retrieve(&conn, &query, 2, CUTOFF_DISABLED)
        .expect("retrieve must succeed even when no candidate has occurrence context");

    assert_eq!(
        results.len(),
        2,
        "expected both candidates back, got {results:?}"
    );
    let returned_ids: std::collections::HashSet<PointId> =
        results.iter().map(|(pid, _, _)| *pid).collect();
    assert_eq!(
        returned_ids,
        std::collections::HashSet::from([a_effect.point_id, b_effect.point_id]),
        "both candidates must still be returned when neither has occurrence context, got {results:?}"
    );
}

// --- #1327: relative-margin filtering of individual weak candidates -----------------------------

/// The formula itself (issue #1327), checked against a hand-worked distribution: top score 0.99,
/// random-baseline mean 0.134, cutoff 0.5 -- halfway from the baseline up to the top score is
/// `0.99 - 0.5 * (0.99 - 0.134) = 0.562`.
#[test]
fn test_relative_margin_floor_matches_hand_worked_formula() {
    let floor = relative_margin_floor(0.99, 0.134, 0.5);
    assert!(
        (floor - 0.562).abs() < 1e-6,
        "relative_margin_floor(top_score=0.99, random_baseline_mean=0.134, cutoff=0.5) must equal \
         0.99 - 0.5 * (0.99 - 0.134) = 0.562 per issue #1327's formula, got {floor}"
    );
}

/// `cutoff=0.0` keeps only candidates tied with the top score; `cutoff=1.0` reduces to the
/// "beats the random baseline" absolute floor #1327 replaced (the floor becomes exactly the
/// baseline itself).
#[test]
fn test_relative_margin_floor_cutoff_endpoints() {
    let top_score = 0.9;
    let baseline = 0.2;
    assert_eq!(
        relative_margin_floor(top_score, baseline, 0.0),
        top_score,
        "cutoff=0.0 must set the floor to exactly the top score (only ties with the top survive)"
    );
    let floor_at_one = relative_margin_floor(top_score, baseline, 1.0);
    assert!(
        (floor_at_one - baseline).abs() < 1e-6,
        "cutoff=1.0 must set the floor to (within f32 rounding of) the random baseline (the old \
         absolute-baseline behavior #1327 replaces), got {floor_at_one} vs baseline {baseline}"
    );
}

/// `filter_by_relative_margin` drops a candidate scoring below the floor and keeps one scoring at
/// or above it, checked directly against the pure function (no tree/db machinery), per this
/// crate's testing convention of asserting the formula's own behavior, not just an end-to-end
/// outcome.
#[test]
fn test_filter_by_relative_margin_drops_below_floor_keeps_at_or_above() {
    let mut results: Vec<(PointId, String, f32)> = vec![
        (1, "top".to_string(), 0.99),
        (2, "borderline".to_string(), 0.7),
        (3, "weak".to_string(), 0.3),
    ];
    // baseline mean = 0.134, cutoff = 0.5 -> floor = 0.562 (same numbers as the hand-worked test
    // above): "top" (0.99) and "borderline" (0.7) clear it, "weak" (0.3) does not.
    filter_by_relative_margin(&mut results, 0.134, 0.5);
    let kept: std::collections::HashSet<PointId> = results.iter().map(|(pid, _, _)| *pid).collect();
    assert_eq!(
        kept,
        std::collections::HashSet::from([1, 2]),
        "candidates at/above the floor (0.562) must survive and the one below it must be \
         dropped, got kept={kept:?} from results={results:?}"
    );
}

/// A degenerate baseline (at or above the result set's own top score -- e.g. a corpus too small
/// for the sample to differ meaningfully from the candidates it is judging) must not drop
/// anything, including the top candidate itself: dropping the top score over a meaningless margin
/// would be strictly worse than doing nothing.
#[test]
fn test_filter_by_relative_margin_is_a_no_op_when_baseline_is_not_below_top_score() {
    let mut results: Vec<(PointId, String, f32)> =
        vec![(1, "top".to_string(), 0.9), (2, "second".to_string(), 0.85)];
    let before = results.clone();
    filter_by_relative_margin(&mut results, 0.9, 0.5); // baseline == top_score
    assert_eq!(
        results, before,
        "a baseline equal to the top score must leave every candidate untouched, got {results:?}"
    );

    let mut results: Vec<(PointId, String, f32)> =
        vec![(1, "top".to_string(), 0.9), (2, "second".to_string(), 0.85)];
    let before = results.clone();
    filter_by_relative_margin(&mut results, 0.95, 0.5); // baseline > top_score
    assert_eq!(
        results, before,
        "a baseline above the top score must leave every candidate untouched, got {results:?}"
    );
}

/// Production-boundary regression through the real `retrieve()` path (tree + sqlite + the real
/// per-query random-baseline sample), at `MemoryConfig::relative_margin_cutoff`'s real default
/// (0.5): a genuinely weak candidate is dropped, a borderline-but-real one is kept, and a clearly
/// unrelated filler background establishes a low random baseline -- the same three-way
/// distinction issue #1327 measured (suppressing wrong candidates while keeping true answers),
/// not just a synthetic call to the pure formula above.
///
/// All embeddings are constructed as `[a, sqrt(1-a^2), 0, ...]` against the query `[1, 0, ...]`,
/// which makes `cos(v, query) == a` exactly (both vectors are unit length by construction), so
/// the intended cosine scores are exact rather than approximate.
#[test]
fn test_retrieve_drops_weak_candidate_and_keeps_borderline_real_one() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let query: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 1.0;
        v
    };
    let with_cosine = |a: f32| -> Vec<f32> {
        let mut v = vec![0.0f32; DIM];
        v[0] = a;
        v[1] = (1.0 - a * a).sqrt();
        v
    };

    let top = tree.insert_with_effect("true answer".to_string(), with_cosine(0.99), 1, 100);
    let borderline = tree.insert_with_effect(
        "real but weaker match".to_string(),
        with_cosine(0.7),
        1,
        101,
    );
    let weak = tree.insert_with_effect("wrong candidate".to_string(), with_cosine(0.3), 1, 102);
    // Six orthogonal filler points (cos == 0.0 against the query) establish a low random
    // baseline, using the six embedding dimensions not spent on the `a`/`sqrt(1-a^2)` pair above.
    for dim in 2..DIM {
        let mut filler = vec![0.0f32; DIM];
        filler[dim] = 1.0;
        tree.insert_with_effect(format!("filler {dim}"), filler, 1, 200 + dim as i64);
    }

    // Corpus is 3 + 6 = 9 points, all sampled by `random_baseline_mean` (well under
    // RANDOM_BASELINE_SAMPLE_SIZE), so the baseline is the exact mean of all nine cosines:
    // (0.99 + 0.7 + 0.3 + 6*0.0) / 9 ~= 0.2211, giving a floor of
    // 0.99 - 0.5 * (0.99 - 0.2211) ~= 0.6056 at the real default cutoff of 0.5.
    let results = tree
        .retrieve(&conn, &query, 3, 0.5)
        .expect("retrieve must succeed");

    let returned_ids: std::collections::HashSet<PointId> =
        results.iter().map(|(pid, _, _)| *pid).collect();
    assert_eq!(
        returned_ids,
        std::collections::HashSet::from([top.point_id, borderline.point_id]),
        "at cutoff=0.5 the weak candidate (cos=0.3, point_id={}) must be dropped while the top \
         (cos=0.99, point_id={}) and borderline-but-real (cos=0.7, point_id={}) candidates \
         survive -- got results={results:?}",
        weak.point_id,
        top.point_id,
        borderline.point_id
    );
    assert!(
        !returned_ids.contains(&weak.point_id),
        "the weak candidate (point_id={}) must not appear in {results:?}",
        weak.point_id
    );
}

/// `random_baseline_mean` must exclude Discard (importance=0) points from its sample, matching
/// `retrieve`'s own candidate filter (module doc: Discard is a real content-safety exclusion, the
/// one filter this facade keeps). A Discard point that scores HIGH against the query is planted
/// deliberately: if it leaked into the sample it would pull the baseline mean up sharply (toward
/// the query's own near-duplicate), which would raise `relative_margin_floor` and could drop
/// legitimate candidates that would otherwise have survived -- content that can never itself be
/// returned must not be able to skew what other candidates are judged against.
#[test]
fn test_random_baseline_mean_excludes_discard_content() {
    let (_dir, path) = open_schema_db();
    let conn = Connection::open(&path).expect("open");
    let mut tree = RoutingMemTree::new_with_dim(DIM);

    let query: Vec<f32> = {
        let mut v = vec![0.0f32; DIM];
        v[0] = 1.0;
        v
    };
    let with_cosine = |a: f32| -> Vec<f32> {
        let mut v = vec![0.0f32; DIM];
        v[0] = a;
        v[1] = (1.0 - a * a).sqrt();
        v
    };

    tree.insert_with_effect("normal one".to_string(), with_cosine(0.2), 1, 100);
    tree.insert_with_effect("normal two".to_string(), with_cosine(0.3), 1, 101);
    // Discard content, deliberately near-identical to the query (cos=0.99) so it would dominate
    // the mean if it leaked into the sample.
    tree.insert_with_effect(
        "discarded near-duplicate".to_string(),
        with_cosine(0.99),
        0,
        102,
    );

    let baseline = tree
        .random_baseline_mean(&query)
        .expect("corpus has live (non-Discard) points to sample");
    let expected = (0.2 + 0.3) / 2.0;
    assert!(
        (baseline - expected).abs() < 1e-5,
        "random_baseline_mean must equal the mean of only the two non-Discard points \
         ((0.2 + 0.3) / 2 = {expected}), got {baseline} -- a value near \
         (0.2 + 0.3 + 0.99) / 3 = {} would mean the Discard point leaked into the sample",
        (0.2 + 0.3 + 0.99) / 3.0
    );
}
