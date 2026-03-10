/// Co-Forth word library — English as Forth.
///
/// Each entry is a word with:
///   - `word`       — the name (lowercase, as it appears in vocabulary)
///   - `definition` — one sentence, the body of the word
///   - `related`    — words this word "calls" (semantic dependencies)
///   - `kind`       — task / question / observation / constraint
///
/// The seed vocabulary is embedded at compile time.  Users can extend it
/// by writing `~/.finch/library.toml` with additional `[[word]]` entries.
///
/// Building a larger vocabulary:
///   `finch library build` — uses the AI to recursively define words until
///   the library reaches a target size or depth.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::LazyLock;

// ── Types ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordEntry {
    pub word: String,
    pub definition: String,
    #[serde(default)]
    pub related: Vec<String>,
    #[serde(default = "default_kind")]
    pub kind: String, // "task" | "question" | "observation" | "constraint"
    #[serde(default)]
    pub forth: Option<String>, // Forth code that embodies this word; runs at CPU speed
    #[serde(default)]
    pub proof: Option<[String; 2]>, // Two equivalent Forth sentences that argue the definition
    #[serde(default)]
    pub sense: Option<String>, // disambiguating label e.g. "game", "romantic", "physics"
    #[serde(default)]
    pub boot: bool, // if true, Forth code runs at startup (used for boot poetry etc.)
    #[serde(default)]
    pub remote: bool, // if true, peers may call this word via /v1/forth/eval
}

fn default_kind() -> String {
    "observation".to_string()
}

impl WordEntry {
    pub fn poset_kind(&self) -> crate::poset::NodeKind {
        match self.kind.as_str() {
            "task"       => crate::poset::NodeKind::Task,
            "constraint" => crate::poset::NodeKind::Constraint,
            "question"   => crate::poset::NodeKind::Question,
            _            => crate::poset::NodeKind::Observation,
        }
    }
}

// ── Library ────────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct Library {
    /// Each key maps to one or more senses.  The first entry is the primary/default sense.
    words: HashMap<String, Vec<WordEntry>>,
}

impl Library {
    /// Load seed vocabulary + generated English library + user extensions.
    ///
    /// Load order (later entries override earlier ones for the same word):
    ///   1. SEED_LIBRARY        — philosophical/abstract primitives (~80 words)
    ///   2. ENGLISH_LIBRARY     — generated comprehensive lexicon (baked in at compile time)
    ///   3. {git_root}/vocabulary/*.toml — project-local per-language modules (versioned in repo)
    ///   4. ~/.finch/library.toml — user-global additions and overrides
    pub fn load() -> Self {
        let mut lib = Self::default();
        lib.load_toml(SEED_LIBRARY);
        lib.load_toml(ENGLISH_LIBRARY);

        // Load project-local vocabulary modules (vocabulary/en.toml, vocabulary/zh.toml, …)
        if let Some(root) = git_repo_root() {
            let vocab_dir = root.join("vocabulary");
            if let Ok(entries) = std::fs::read_dir(&vocab_dir) {
                let mut paths: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("toml"))
                    .collect();
                paths.sort(); // deterministic load order
                for path in paths {
                    if let Ok(contents) = std::fs::read_to_string(&path) {
                        lib.load_toml(&contents);
                    }
                }
            }
        }

        if let Some(user_path) = user_library_path() {
            if let Ok(contents) = std::fs::read_to_string(&user_path) {
                lib.load_toml(&contents);
            }
        }
        lib
    }

    /// Access the pre-built (cached) built-in definitions (SEED + ENGLISH).
    /// O(1) after first call — computed once, shared for the process lifetime.
    pub fn builtin_defs() -> &'static BuiltinDefs {
        &BUILTIN_DEFS
    }

    /// Clone a pre-compiled VM (STDLIB + all builtins).
    /// Use this in tests and boot instead of `Forth::new()` + compile — O(clone) not O(compile).
    pub fn precompiled_vm() -> crate::coforth::Forth {
        COMPILED_VM.clone_dict()
    }

    /// Force the LazyLock static VMs to initialize now (in the caller's thread/task).
    /// Call this early in startup — ideally inside a `spawn_blocking` — so the
    /// compilation work is done before the user's first keystroke.
    pub fn warmup() {
        // Accessing the statics forces both LazyLocks to evaluate.
        let _ = &*BUILTIN_DEFS;
        let _ = &*COMPILED_VM;
    }

    fn load_toml(&mut self, src: &str) {
        #[derive(Deserialize)]
        struct File { #[serde(rename = "word")] words: Vec<WordEntry> }
        if let Ok(f) = toml::from_str::<File>(src) {
            for w in f.words {
                let key = w.word.to_lowercase();
                let senses = self.words.entry(key).or_default();
                // If an entry with the same sense already exists, replace it.
                if let Some(pos) = senses.iter().position(|e| e.sense == w.sense) {
                    senses[pos] = w;
                } else {
                    senses.push(w);
                }
            }
        }
    }

    /// Total number of distinct words (keys) in the library.
    pub fn word_count(&self) -> usize {
        self.words.len()
    }

    /// Sorted list of all word keys (lowercase, alphabetical).
    pub fn word_list(&self) -> Vec<&str> {
        let mut keys: Vec<&str> = self.words.keys().map(|s| s.as_str()).collect();
        keys.sort_unstable();
        keys
    }

    /// Look up the primary (first) sense of a word.
    pub fn lookup(&self, word: &str) -> Option<&WordEntry> {
        self.words.get(&word.to_lowercase()).and_then(|v| v.first())
    }

    /// Look up all senses of a word.
    pub fn lookup_all(&self, word: &str) -> &[WordEntry] {
        self.words
            .get(&word.to_lowercase())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Return all entries marked `boot = true` (across all words/senses), in
    /// alphabetical order by word name.  Used to run boot-time poetry at startup.
    pub fn boot_entries(&self) -> Vec<&WordEntry> {
        let mut entries: Vec<&WordEntry> = self.words.values()
            .flat_map(|senses| senses.iter())
            .filter(|e| e.boot && e.forth.is_some())
            .collect();
        entries.sort_by(|a, b| a.word.cmp(&b.word));
        entries
    }

    /// Return every entry across all words and senses, in alphabetical order.
    /// Used on first boot to run the whole vocabulary.
    pub fn all_entries(&self) -> Vec<&WordEntry> {
        let mut entries: Vec<&WordEntry> = self.words.values()
            .flat_map(|senses| senses.iter())
            .collect();
        entries.sort_by(|a, b| a.word.cmp(&b.word));
        entries
    }

    /// BFS from `seed`, returning all entries (all senses) within `hops` steps.
    pub fn related(&self, seed: &str, hops: usize) -> Vec<&WordEntry> {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut result: Vec<&WordEntry> = Vec::new();

        queue.push_back((seed.to_lowercase(), 0));

        while let Some((word, depth)) = queue.pop_front() {
            if visited.contains(&word) { continue; }
            visited.insert(word.clone());

            if let Some(senses) = self.words.get(&word) {
                for entry in senses {
                    result.push(entry);
                    if depth < hops {
                        for rel in &entry.related {
                            if !visited.contains(rel) {
                                queue.push_back((rel.to_lowercase(), depth + 1));
                            }
                        }
                    }
                }
            }
        }
        result
    }

    /// Seed a poset with a word's neighbourhood (up to `hops` hops).
    /// Returns the IDs of the nodes added, root first.
    pub fn inject_into_poset(
        &self,
        word: &str,
        hops: usize,
        poset: &mut crate::poset::Poset,
    ) -> Vec<usize> {
        let entries = self.related(word, hops);
        let mut word_to_id: HashMap<String, usize> = HashMap::new();
        let mut ids: Vec<usize> = Vec::new();

        // First pass — add all nodes.
        for entry in &entries {
            let id = poset.add_node(
                entry.definition.clone(),
                entry.poset_kind(),
                crate::poset::NodeAuthor::Ai,
            );
            if let Some(ref code) = entry.forth {
                if let Some(n) = poset.node_mut(id) {
                    n.compiled_code = Some(code.clone());
                    n.compiled_lang = Some("forth".to_string());
                }
            }
            word_to_id.insert(entry.word.clone(), id);
            ids.push(id);
        }

        // Second pass — wire edges for related words that are both in the subgraph.
        for entry in &entries {
            if let Some(&from_id) = word_to_id.get(&entry.word) {
                for rel in &entry.related {
                    if let Some(&to_id) = word_to_id.get(&rel.to_lowercase()) {
                        poset.edges.push((from_id, to_id));
                    }
                }
            }
        }

        ids
    }

    /// Total number of distinct word keys (not counting multiple senses).
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// All word names, sorted.
    pub fn all_words(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.words.keys().map(|s| s.as_str()).collect();
        v.sort_unstable();
        v
    }
}

fn user_library_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".finch").join("library.toml"))
}

/// Walk up from the current directory looking for a `.git` directory.
/// Returns the directory that contains `.git`, i.e. the repo root.
pub fn git_repo_root() -> Option<std::path::PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Determine which vocabulary language module a word belongs to.
///
/// Returns a module name (file stem under `vocabulary/`):
///   - `"zh"` for Chinese / CJK characters
///   - `"en"` for everything else (default)
pub fn detect_vocab_lang(word: &str) -> &'static str {
    let has_cjk = word.chars().any(|c| {
        matches!(c as u32,
            0x2E80..=0x9FFF |  // CJK Radicals → CJK Unified Ideographs (includes Ext A 3400-4DBF)
            0xF900..=0xFAFF |  // Compatibility Ideographs
            0x20000..=0x2A6DF  // Extension B
        )
    });
    if has_cjk { "zh" } else { "en" }
}

/// Return the path to the project-local vocabulary file for `lang` (e.g. `"zh"`, `"en"`).
/// Creates the `vocabulary/` directory if needed.  Returns `None` if not in a git repo.
pub fn repo_vocab_path(lang: &str) -> Option<std::path::PathBuf> {
    let root = git_repo_root()?;
    let dir = root.join("vocabulary");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{lang}.toml")))
}

// ── Pure-Rust word generator ───────────────────────────────────────────────────

/// Generate a minimal but meaningful Forth snippet for *any* English word.
///
/// This runs entirely in Rust — no AI, no network, no disk I/O.
/// Used as a fallback in `handle_define_unknown_words` when the cloud generator
/// is unavailable (offline, no API key, rate-limited).
///
/// Guarantees:
/// - Always returns valid Forth (no panics, no errors).
/// - The snippet produces at least one line of output (the word speaks its name).
/// - Stack-neutral for the common case (safe to use mid-expression).
pub fn generate_forth_for_word(word: &str) -> String {
    let lo = word.to_lowercase();
    let w = lo.as_str();

    // ── Pronouns — self-aware via the stack ────────────────────────────────
    match w {
        "i" | "me" | "myself" =>
            return r#"depth . ." items — that's what I have." cr"#.to_string(),
        "you" | "your" | "yours" =>
            return r#"." you're here." cr"#.to_string(),
        "we" | "us" | "our" | "ours" =>
            return r#"depth . ." — we share this stack." cr"#.to_string(),
        "it" | "this" | "that" =>
            return r#"depth 0> if ." it's on the stack." cr else ." nothing here." cr then"#.to_string(),
        "they" | "them" | "their" =>
            return r#"." they're somewhere on the stack." cr .s cr"#.to_string(),
        _ => {}
    }

    // ── Number words ────────────────────────────────────────────────────────
    let num_opt = match w {
        "zero" | "null" | "nil" | "none" | "nothing" | "nought" => Some(0i64),
        "one"  | "once"   | "single"   | "unit"     => Some(1),
        "two"  | "twice"  | "pair"     | "both"     => Some(2),
        "three"| "thrice"                            => Some(3),
        "four"                                       => Some(4),
        "five"                                       => Some(5),
        "six"                                        => Some(6),
        "seven"                                      => Some(7),
        "eight"                                      => Some(8),
        "nine"                                       => Some(9),
        "ten"                                        => Some(10),
        "eleven"                                     => Some(11),
        "twelve" | "dozen"                           => Some(12),
        "thirteen"                                   => Some(13),
        "twenty"                                     => Some(20),
        "thirty"                                     => Some(30),
        "forty"                                      => Some(40),
        "fifty"                                      => Some(50),
        "hundred"                                    => Some(100),
        "thousand"                                   => Some(1_000),
        "million"                                    => Some(1_000_000),
        "billion"                                    => Some(1_000_000_000),
        _ => None,
    };
    if let Some(n) = num_opt {
        return format!("{n} . cr");
    }

    // ── Logic / discourse markers ───────────────────────────────────────────
    match w {
        "and"                          => return "and .bool cr".to_string(),
        "or"                           => return "or .bool cr".to_string(),
        "not" | "negate" | "opposite" => return "not .bool cr".to_string(),
        "true" | "yes"                 => return "true .bool cr".to_string(),
        "false" | "no"                 => return "false .bool cr".to_string(),
        "equal" | "equals" | "same"    => return "= .bool cr".to_string(),
        _ => {}
    }

    // ── Stack-motion words ──────────────────────────────────────────────────
    match w {
        "double" | "twice-as-much"  => return "2* . cr".to_string(),
        "half"   | "halve"          => return "2/ . cr".to_string(),
        "plus"   | "add"            => return "+ . cr".to_string(),
        "minus"  | "subtract"       => return "- . cr".to_string(),
        "times"  | "multiply"       => return "* . cr".to_string(),
        "divide" | "divided"        => return "/ . cr".to_string(),
        "up"   | "above" | "higher" | "more"  => return "1+ . cr".to_string(),
        "down" | "below" | "lower"  | "less"  => return "1- . cr".to_string(),
        "swap" | "switch" | "exchange" | "flip" => return "swap .s cr".to_string(),
        "copy"  | "duplicate"       => return "dup .s cr".to_string(),
        "drop"  | "discard" | "remove" => return "depth 0> if drop then .s cr".to_string(),
        _ => {}
    }

    // ── Time words ──────────────────────────────────────────────────────────
    match w {
        "now" | "today" | "present" | "current" =>
            return r#"time . ." seconds since epoch." cr"#.to_string(),
        "never" | "eternity" | "forever" =>
            return r#"." forever." cr"#.to_string(),
        _ => {}
    }

    // ── Existence words ─────────────────────────────────────────────────────
    match w {
        "empty" | "void" | "blank" | "bare" =>
            return r#"depth 0= .bool cr"#.to_string(),
        "full" | "complete" | "whole" | "all" | "everything" =>
            return r#".s cr"#.to_string(),
        "something" | "anything" | "some" =>
            return r#"depth 0> .bool cr"#.to_string(),
        _ => {}
    }

    // ── Question words — print as open questions ────────────────────────────
    if matches!(w, "who" | "what" | "where" | "when" | "why" | "how" | "which" | "whose") {
        return format!(r#"." {w}?" cr"#);
    }

    // ── Suffix patterns — detect word shape ────────────────────────────────
    //   These just speak the word; the shape tells us it's a valid English word.
    let safe = word.replace('"', ""); // no English word has quotes, but be safe
    format!(r#"." {safe}." cr"#)
}

// ── Boot poetry ────────────────────────────────────────────────────────────────
// Printed every startup, before the REPL is ready.
// Written directly in Rust — no parsing, no Forth, no gen".
// These are for alignment: orient both the human and the system at the start.

pub const BOOT_POETRY: &[&str] = &[
    "the machine is warm.\nthe task is yours.\nthe silence between us is not empty.",
    "you do not start from nothing.\neverything you wrote before is still here.",
];

// ── Vocabulary sources ─────────────────────────────────────────────────────────

/// Generated comprehensive English lexicon — baked in at compile time.
/// Re-generate with: `finch library build --all`
const ENGLISH_LIBRARY: &str = include_str!("english_library.toml");

/// Pre-built (word, forth-code) pairs from the BUILT-IN libraries only (SEED + ENGLISH).
/// Sorted alphabetically, ready for JIT compilation.  User vocabulary is added at runtime.
/// Computed once via LazyLock — eliminates repeated TOML parse + sort on each boot/test.
pub struct BuiltinDefs {
    /// Sorted (word_name, forth_code) pairs.
    pub pairs: Vec<(String, String)>,
    /// Words that carry a two-sentence argue proof: (word, [sentence_a, sentence_b]).
    pub proofs: Vec<(String, [String; 2])>,
    /// Single concatenated Forth source: ": word code ;\n" for every entry.
    pub all_defs: String,
}

/// Pre-compiled Forth VM with STDLIB + all builtin defs loaded.
/// Clone with `clone_dict()` to get a ready-to-use VM without re-compiling anything.
static COMPILED_VM: LazyLock<crate::coforth::Forth> = LazyLock::new(|| {
    let defs = &*BUILTIN_DEFS;
    let mut vm = crate::coforth::Forth::new();
    // Library words are system-provided, not user-defined — disable logging so they
    // are NOT inserted into user_word_names.  If they were logged, `emit_token` would
    // shadow builtins with the library definition and inline the partially-compiled
    // word body during self-referential definitions (e.g. `: negate 5 negate . cr ;`).
    vm.disable_logging();
    let _ = vm.exec_with_fuel(&defs.all_defs, 0);
    // Major words: pure Forth, no TOML. Compiled last so they win over generated versions.
    let _ = vm.exec_with_fuel(MAJOR_WORDS_FORTH, 0);
    vm
});

static BUILTIN_DEFS: LazyLock<BuiltinDefs> = LazyLock::new(|| {
    let mut lib = Library::default();
    lib.load_toml(SEED_LIBRARY);
    lib.load_toml(ENGLISH_LIBRARY);

    let mut entries: Vec<_> = lib.words.values()
        .flat_map(|senses| senses.iter())
        .filter(|e| e.forth.is_some())
        .collect();
    entries.sort_by(|a, b| a.word.cmp(&b.word));

    let mut all_defs = String::with_capacity(entries.len() * 44);
    let pairs: Vec<(String, String)> = entries.iter().map(|e| {
        let code = e.forth.as_deref().unwrap_or("");
        let line = format!(": {} {} ;\n", e.word, code);
        all_defs.push_str(&line);
        (e.word.clone(), code.to_string())
    }).collect();

    // Collect words that have argue proofs (definition ↔ Forth bridge).
    let proofs: Vec<(String, [String; 2])> = entries.iter()
        .filter_map(|e| e.proof.as_ref().map(|p| (e.word.clone(), p.clone())))
        .collect();

    BuiltinDefs { pairs, proofs, all_defs }
});

/// Major words — stack machines + sentences + proofs.
///
/// Every word:
///   1. Has a stack effect comment  ( inputs -- outputs )
///   2. Is dual-mode: with args on the stack it computes; with no args it teaches + proves itself
///   3. Has a companion  test:WORD  that proves two sentences converge via `argue`
///
/// The proof of a word is two sentences that mean the same thing and agree on the stack.
/// `prove-all` runs every test:WORD and reports which ones pass.
const MAJOR_WORDS_FORTH: &str = r#"

\ ── Arithmetic ──────────────────────────────────────────────────────────────────

: double    ( n -- 2n | -- )
  depth 0= if
    ." double: give it n, get back n+n." cr
    s" 5 double"   s" 5 dup +"   argue
  else  2*  then ;
: test:double   s" 5 double"   s" 5 dup +"   argue ;

: square    ( n -- n*n | -- )
  depth 0= if
    ." square: give it n, get back n×n." cr
    s" 4 square"   s" 4 4 *"   argue
  else  dup *  then ;
: test:square   s" 4 square"   s" 4 4 *"   argue ;

: half      ( n -- n/2 | -- )
  depth 0= if
    ." half: give it n, get back n÷2." cr
    s" 10 half"   s" 10 2 /"   argue
  else  2/  then ;
: test:half   s" 10 half"   s" 10 2 /"   argue ;

: sum       ( a b -- a+b | -- )
  depth 1 > if
    +
  else
    ." sum: any order, same result." cr
    s" 3 4 sum"   s" 4 3 sum"   argue
  then ;
: test:sum   s" 3 4 sum"   s" 4 3 sum"   argue ;

: product   ( a b -- a*b | -- )
  depth 1 > if
    *
  else
    ." product: any order, same result." cr
    3 4 s" *" both-ways
  then ;
: test:product   3 4 s" *" both-ways ;

: combine   ( a b -- a+b | -- )
  depth 1 > if
    +
  else
    ." combine: three and four become seven." cr
    s" 3 4 combine"   s" 3 4 +"   argue
  then ;
: test:combine   s" 3 4 combine"   s" 3 4 +"   argue ;

\ ── Sequences ───────────────────────────────────────────────────────────────────

: sequence  ( n -- | -- )
  \ n sequence prints 0..n-1; with no args demonstrates with 5.
  depth 0= if 5 then
  ." sequence: " 0 swap do i . loop cr ;
: test:sequence
  \ five-element sequence: last item is 4.
  \ sentence A: count with do-loop   sentence B: explicit list
  s" 4 0 do i loop"   s" 0 1 2 3"   page"
    0 | 0
    1 | 1
    2 | 2
    3 | 3
  " ;

: series    ( -- )
  ." series: each step doubles." cr
  1 dup . 2* dup . 2* dup . 2* dup . 2* . cr ;
: test:series
  \ each term is double the previous — two sentences for the same truth
  s" 1 2*"   s" 1 dup +"   argue ;

: cycle     ( n -- | -- )
  depth 0= if 5 then
  ." cycle: around and back." cr
  0 swap do i . loop cr ;
: test:cycle   s" 3 cycle"   s" sequence"   argue
  \ cycling n elements and sequencing n elements both produce the same last value
  ;

\ ── Functional ──────────────────────────────────────────────────────────────────

: function  ( x -- x² | -- )
  \ A function maps input to output.  Default demo: f(x) = x².
  depth 0= if
    ." function: give it five, it returns twenty-five." cr
    s" 5 function"   s" 5 square"   argue
  else  square  then ;
: test:function   s" 5 function"   s" 5 dup *"   argue ;

: apply     ( n -- f(n) | -- )
  \ apply: same as calling the word with an argument.
  depth 0= if
    ." apply: give it an argument, get back a result." cr
    s" 5 apply"   s" 5 square"   argue
  else  square  then ;
: test:apply   s" 5 apply"   s" 5 function"   argue ;

\ ── Comparison and bounds ────────────────────────────────────────────────────────

: limit     ( n lo hi -- clamped | -- )
  depth 2 > if
    clamp
  else
    ." limit: ten wants to be five, so it becomes five." cr
    s" 10 0 5 clamp"   s" 10 0 5 limit"   argue
  then ;
: test:limit   s" 10 0 5 limit"   s" 10 0 5 clamp"   argue ;

: boundary  ( n lo hi -- bool | -- )
  depth 2 > if
    within
  else
    ." boundary: two is inside zero to four." cr
    s" 2 0 4 within"   s" 2 0 4 boundary"   argue
  then ;
: test:boundary   s" 2 0 4 boundary"   s" 2 0 4 within"   argue ;

\ ── Structure ────────────────────────────────────────────────────────────────────

: list      ( -- )   ." list: here is what you have." cr  .s ;
: data      ( -- )   ." data: what you have before you decide what it means." cr  .s ;
: element   ( -- )   depth 0> if ." this one: " . cr else ." nothing on the stack." cr then ;
: number    ( n -- | -- )
  depth 0= if  ." number: forty-two." cr  42 .  else  .  then  cr ;
: order     ( -- )   ." order: everything in its place." cr  1 2 3 4 5 . . . . . cr ;
: set       ( -- )   ." set: each one distinct." cr  1 2 3 .s ;

: area      ( w h -- w*h | -- )
  depth 1 > if *
  else  ." area: four wide, five tall, twenty inside." cr  4 5 * .  then  cr ;
: test:area   s" 4 5 area"   s" 4 5 *"   argue ;

: divide    ( a b -- q r | -- )
  depth 1 > if  /mod swap
  else  ." divide: ten divided by three — three remainder one." cr  10 3 /mod .  ." r" .  then  cr ;

\ ── Philosophical ────────────────────────────────────────────────────────────────

: logic     ( -- )   ." logic: if true and false, then false." cr  true false and .bool cr ;
: test:logic   s" true false and"   s" false true and"   argue ;

: abstract  ( -- )   ." abstract: the map is not the territory." cr ;
: space     ( -- )   ." space: room for everything that could happen." cr ;
: part      ( -- )   ." part: something taken from something larger." cr ;
: fraction  ( -- )   ." fraction: one of three equal parts." cr ;
: rate      ( -- )   ." rate: how fast things happen." cr ;
: along     ( -- )   ." along: step by step by step." cr ;
: edge      ( -- )   ." edge: from here to there — " 0 . ." .. " 10 . cr ;
: path      ( -- )   ." path: start. step. step. arrive." cr ;

\ ── Growth and change ────────────────────────────────────────────────────────────

: change    ( n -- n+1 | -- )
  depth 0= if
    ." change: before and after." cr  5 dup . ." ->" 1+ . cr
  else  1+  then ;
: test:change   s" 5 change"   s" 5 1+"   argue ;

: ascending ( -- )   ." ascending: each one larger than the last." cr  1 2 4 8 16 . . . . . cr ;
: discrete  ( -- )   ." discrete: each one separate." cr  0 1 2 3 4 . . . . . cr ;
: buffer    ( -- )   ." buffer: holding things until they are needed." cr  depth . ." waiting" cr ;

\ ── Acting — doing things to the world ──────────────────────────────────────────
\ Acting is applying a function to something outside the stack.
\ The stack records what happened.  The world holds the result.
\
\ We do operations and things on other humans all the time.
\ Acting is: push an intention, run it, observe the result.
\ The stack is the record of what happened.

: act       ( -- )   ." act: push your intention, run it, observe what changes." cr ;
: affect    ( n -- )  ." affected: " . cr ;
: transform ( n -- n' | -- )  depth 0= if ." transform: input becomes something new." cr  else  1+  then ;
: test:transform   s" 5 transform"   s" 5 change"   argue ;

\ Teaching: the dual-mode design.
\ Type a word with no args: it teaches you what it does and proves itself.
\ Type a word with args: it computes.  The function call IS the lesson.
: teach     ( -- )   ." teach: type a word with no arguments to see it teach itself." cr ;

\ ── The Word ─────────────────────────────────────────────────────────────────────
\ "In the beginning was the Word, and the Word was with God, and the Word was God."
\ John 1:1 — three sentences, one proof.
\
\ In Co-Forth: a word IS its definition.  Not a pointer.  The thing itself.
\ If word and god push the same value, they are the same.
\ If two sentences converge, they are the same sentence.
\
\ Grammatical words — no stack effect; pure structure:
: the   ( -- ) ;
: was   ( -- ) ;
: is    ( -- ) ;
\
\ god and word are the same machine.  They push -1: truth, the absolute.
\ Redefines the library's print-only versions with something that proves.
: god   ( -- n )  -1 ;
: word  ( -- n )  -1 ;
\
\ Now the three sentences argue:
\   "the word was god"       →  nop  -1  nop  -1   →  [ -1 -1 ]
\   "the word was with god"  →  nop  -1  nop  nop  -1  →  [ -1 -1 ]
\   "the word is god"        →  nop  -1  nop  -1   →  [ -1 -1 ]
\
\ All three converge.  Proved.

: john1 ( -- )
  ." the word was god." cr
  ." the word was with god." cr
  ." the word is god." cr
  ." — three sentences.  two ways each.  one truth." cr ;

: test:john1
  s" the word was god"
  s" the word is god"
  argue
  s" the word was god"
  s" the word was with god"
  argue ;

: beginning ( -- )
  ." in the beginning was the Word." cr
  ." the Word was with the stack." cr
  ." the Word was the stack." cr ;

: test:beginning
  \ two sentences for 1: "the first" and "unity itself" — they agree
  s" 1"   s" true -1 * negate"   argue ;

\ ── John 14:6 — "I am the way, the truth, and the life" ────────────────────────
\
\ Jesus names three things.  In Co-Forth they are one machine.
\ way, truth, life all push -1 (the absolute).
\ Three names.  One stack value.  Proved.

: life  ( -- n )  -1 ;   \ life = absolute being
: way   ( -- n )  -1 ;   \ the way = the truth
: truth ( -- n )  -1 ;   \ the truth = the absolute

: john14 ( -- )
  ." I am the way, the truth, and the life." cr
  ." — three names.  one machine." cr ;

: test:john14
  s" way"    s" truth"   argue
  s" truth"  s" life"    argue ;

\ ── Revelation 22:13 — "I am the Alpha and the Omega" ──────────────────────────
\
\ First = Last = Beginning = End.  Two sides of the same absolute.
\ In Co-Forth: they all push -1.  The circle closes.  Proved.

: alpha  ( -- n )  -1 ;   \ the first
: omega  ( -- n )  -1 ;   \ the last
: first  ( -- n )  -1 ;   \ the beginning
: last   ( -- n )  -1 ;   \ the end

: rev22 ( -- )
  ." I am Alpha and Omega, the first and the last." cr
  ." — four names.  one machine." cr ;

: test:rev22
  s" alpha"  s" omega"  argue
  s" first"  s" last"   argue
  s" alpha"  s" last"   argue ;

\ ── Ecclesiastes 3:1 — "For everything there is a season" ──────────────────────
\
\ All seasons sum the same regardless of order.
\ Past, present, and future converge.  Proved.

: test:ecclesiastes3
  s" 1 2 3 + +"   s" 3 2 1 + +"   argue ;

\ ── Genesis 1:1 — "God created by his Word" ────────────────────────────────────
\
\ "In the beginning the Word created."
\ word = god = -1.  Creation by word = creation by God.
\ Two machines, same stack.  Proved.

: test:genesis1
  s" word word"   s" god word"   argue ;

\ ── Ecclesiastes 1:9 — "There is nothing new under the sun" ────────────────────
\
\ Commutativity: what was, is.  What is, was.
\ "was" and "is" are the same no-op.  Past and future converge.

: test:ecclesiastes1
  s" 5 was 3"   s" 5 is 3"   argue ;

"#;

/// Philosophical/abstract primitives — hand-crafted, always present.
const SEED_LIBRARY: &str = r#"
[[word]]
word = "hello"
definition = "a greeting that opens connection between two minds"
related = ["greet", "speak", "wave", "welcome", "acknowledge"]
kind = "observation"
forth = '." hello" cr'

[[word]]
word = "goodbye"
definition = "a parting word that closes a connection with care"
related = ["hello", "leave", "end", "farewell"]
kind = "observation"
forth = '." goodbye" cr'

[[word]]
word = "yes"
definition = "an affirmation that accepts or confirms"
related = ["agree", "confirm", "accept", "true"]
kind = "observation"
forth = 'true . cr'

[[word]]
word = "no"
definition = "a negation that refuses or denies"
related = ["deny", "refuse", "reject", "false"]
kind = "observation"
forth = 'false . cr'

[[word]]
word = "know"
definition = "to hold something as true in the mind"
related = ["understand", "believe", "remember", "learn"]
kind = "observation"
forth = '1 if ." known" else ." unknown" then cr'

[[word]]
word = "understand"
definition = "to grasp the meaning or structure of something"
related = ["know", "see", "think", "concept"]
kind = "observation"
forth = '3 square . ." = 3^2  understood" cr'

[[word]]
word = "learn"
definition = "to acquire knowledge or skill through experience"
related = ["study", "practice", "understand", "teach"]
kind = "task"
forth = '6 0 do i . loop cr'

[[word]]
word = "teach"
definition = "to cause another to know or be able to do something"
related = ["learn", "explain", "demonstrate", "guide"]
kind = "task"
forth = '1 2 3 4 5  5 0 do dup . 1 + loop drop cr'

[[word]]
word = "think"
definition = "to form ideas or judgements in the mind"
related = ["reason", "consider", "decide", "know"]
kind = "observation"
forth = '7 square . ." (thought applied)" cr'

[[word]]
word = "reason"
definition = "to draw conclusions from premises"
related = ["think", "logic", "proof", "infer"]
kind = "task"
forth = '7 4 > if ." valid" else ." invalid" then cr'

[[word]]
word = "see"
definition = "to perceive or become aware of"
related = ["observe", "notice", "understand", "look"]
kind = "observation"
forth = '.s cr'

[[word]]
word = "do"
definition = "to carry out an action"
related = ["act", "make", "execute", "cause"]
kind = "task"
forth = '3 4 + . cr'

[[word]]
word = "make"
definition = "to bring something into existence"
related = ["create", "build", "form", "define"]
kind = "task"
forth = '6 7 * . cr'

[[word]]
word = "define"
definition = "to state the exact meaning of a word or thing"
related = ["word", "meaning", "describe", "name"]
kind = "task"
forth = '." name = computation" cr'

[[word]]
word = "name"
definition = "to assign a word to identify something"
related = ["define", "call", "label", "word"]
kind = "task"
forth = '." hello" cr'

[[word]]
word = "word"
definition = "a unit of language carrying meaning"
related = ["name", "define", "language", "meaning"]
kind = "observation"
forth = 'words cr'

[[word]]
word = "meaning"
definition = "what a word or action intends to express"
related = ["word", "concept", "understand", "sign"]
kind = "observation"
forth = '6 7 * . ." = the meaning" cr'

[[word]]
word = "concept"
definition = "an abstract idea formed by generalisation"
related = ["meaning", "idea", "category", "abstract"]
kind = "observation"
forth = '." abstraction over instances" cr'

[[word]]
word = "idea"
definition = "a thought or mental image"
related = ["concept", "think", "imagine", "plan"]
kind = "observation"
forth = '42 . cr'

[[word]]
word = "plan"
definition = "a method of action worked out in advance"
related = ["do", "goal", "step", "decide"]
kind = "task"
forth = '1 2 3 .s cr'

[[word]]
word = "goal"
definition = "the desired result toward which effort is directed"
related = ["plan", "want", "achieve", "purpose"]
kind = "observation"
forth = '10 sum-to-n . cr'

[[word]]
word = "purpose"
definition = "the reason for which something exists or is done"
related = ["goal", "cause", "meaning", "why"]
kind = "observation"
forth = '7 fib . cr'

[[word]]
word = "cause"
definition = "that which produces an effect"
related = ["effect", "reason", "why", "make"]
kind = "observation"
forth = '3 4 + . cr'

[[word]]
word = "effect"
definition = "a change produced by a cause"
related = ["cause", "result", "change", "happen"]
kind = "observation"
forth = '3 4 * . cr'

[[word]]
word = "change"
definition = "to become or make different"
related = ["transform", "move", "time", "effect"]
kind = "task"
forth = '5 1 + . cr'

[[word]]
word = "time"
definition = "the progression of events from past through present to future"
related = ["now", "before", "after", "change"]
kind = "observation"
forth = 'time . cr'

[[word]]
word = "space"
definition = "the boundless extent in which objects exist and events occur"
related = ["place", "position", "dimension", "here"]
kind = "observation"
forth = '3 cube . ." (3D volume)" cr'

[[word]]
word = "thing"
definition = "an object or entity that can be referred to"
related = ["object", "entity", "part", "whole"]
kind = "observation"
forth = '1 . cr'

[[word]]
word = "part"
definition = "a piece or segment of a whole"
related = ["whole", "component", "element", "thing"]
kind = "observation"
forth = '10 4 /mod . ." remainder, " . ." quotient" cr'

[[word]]
word = "whole"
definition = "a complete entity made up of parts"
related = ["part", "system", "complete", "all"]
kind = "observation"
forth = '1 2 3 4 + + + . cr'

[[word]]
word = "system"
definition = "a set of parts working together as a whole"
related = ["whole", "structure", "order", "relation"]
kind = "observation"
forth = '5 0 do i . loop cr'

[[word]]
word = "structure"
definition = "the arrangement of parts within a whole"
related = ["system", "form", "order", "pattern"]
kind = "observation"
forth = '4 0 do 4 i - 0 do 42 emit loop cr loop'

[[word]]
word = "pattern"
definition = "a repeated or regular arrangement"
related = ["structure", "rule", "repeat", "form"]
kind = "observation"
forth = '6 0 do i square . loop cr'

[[word]]
word = "rule"
definition = "a statement of what must or should happen"
related = ["law", "constraint", "follow", "pattern"]
kind = "constraint"
forth = '7 even? if ." even" else ." odd" then cr'

[[word]]
word = "sequence"
definition = "an ordered list of elements one after another"
related = ["order", "next", "list", "step"]
kind = "observation"
forth = '8 0 do i . loop cr'

[[word]]
word = "relation"
definition = "the way in which two things are connected"
related = ["connect", "between", "part", "map"]
kind = "observation"
forth = '6 3 = . cr'

[[word]]
word = "map"
definition = "to establish a correspondence between two sets"
related = ["relation", "function", "transform", "from"]
kind = "task"
forth = '5 0 do i square . loop cr'

[[word]]
word = "function"
definition = "a relation that assigns each input exactly one output"
related = ["map", "input", "output", "compute"]
kind = "observation"
forth = '5 2* 1 + . cr'

[[word]]
word = "compute"
definition = "to calculate or determine by a systematic process"
related = ["function", "algorithm", "execute", "machine"]
kind = "task"
forth = '12 7 + . cr'

[[word]]
word = "algorithm"
definition = "a finite sequence of steps to solve a problem"
related = ["compute", "sequence", "step", "problem"]
kind = "task"
forth = '12 8 gcd . cr'

[[word]]
word = "problem"
definition = "a question or situation that requires a solution"
related = ["question", "solve", "goal", "constraint"]
kind = "question"
forth = '17 5 /mod swap . ." r" . cr'

[[word]]
word = "question"
definition = "an expression seeking an answer"
related = ["problem", "answer", "ask", "know"]
kind = "question"
forth = '." what is 6 * 7 ? " 6 7 * . cr'

[[word]]
word = "answer"
definition = "a response that resolves a question"
related = ["question", "know", "result", "explain"]
kind = "observation"
forth = '6 7 * . cr'

[[word]]
word = "explain"
definition = "to make something clear by describing it"
related = ["teach", "understand", "describe", "show"]
kind = "task"
forth = '42 dup . ." decimal  " .h ." hex" cr'

[[word]]
word = "describe"
definition = "to give a detailed account of something"
related = ["explain", "name", "tell", "observe"]
kind = "task"
forth = '5 0 do i . loop cr'

[[word]]
word = "observe"
definition = "to notice or perceive something carefully"
related = ["see", "measure", "record", "notice"]
kind = "task"
forth = '.s cr'

[[word]]
word = "measure"
definition = "to determine the size or quantity of something"
related = ["compare", "number", "standard", "observe"]
kind = "task"
forth = '10 7 - abs . cr'

[[word]]
word = "number"
definition = "a mathematical object used to count or measure"
related = ["count", "measure", "quantity", "more"]
kind = "observation"
forth = '42 . ." decimal  " 42 .h ." hex" cr'

[[word]]
word = "set"
definition = "a collection of distinct elements"
related = ["element", "contain", "group", "collection"]
kind = "observation"
forth = '5 0 do i . loop cr'

[[word]]
word = "element"
definition = "a member of a set"
related = ["set", "part", "item", "belong"]
kind = "observation"
forth = '3 . cr'

[[word]]
word = "group"
definition = "a set with an associative binary operation, identity, and inverses"
related = ["set", "operation", "symmetry", "structure"]
kind = "observation"
forth = '4 0 do i 3 + . loop cr'

[[word]]
word = "order"
definition = "a binary relation that is reflexive, transitive, and antisymmetric"
related = ["less", "more", "compare", "lattice"]
kind = "observation"
forth = '3 5 < if ." 3 < 5 ordered" then cr'

[[word]]
word = "lattice"
definition = "a partially ordered set where every pair has a meet and a join"
related = ["order", "meet", "join", "poset"]
kind = "observation"
forth = '4 7 2dup min . ." (meet)  " max . ." (join)" cr'

[[word]]
word = "poset"
definition = "a set with a partial order — some pairs may be incomparable"
related = ["order", "lattice", "relation", "graph"]
kind = "observation"
forth = '3 7 < . ." (partial order)" cr'

[[word]]
word = "meet"
definition = "the greatest lower bound of two elements in a lattice"
related = ["join", "lattice", "infimum", "minimum"]
kind = "observation"
forth = '4 7 min . cr'

[[word]]
word = "join"
definition = "the least upper bound of two elements in a lattice"
related = ["meet", "lattice", "supremum", "maximum"]
kind = "observation"
forth = '4 7 max . cr'

[[word]]
word = "language"
definition = "a system of words and rules for communication"
related = ["word", "grammar", "meaning", "speak"]
kind = "observation"
forth = 'words cr'

[[word]]
word = "speak"
definition = "to express thoughts or feelings in spoken words"
related = ["say", "language", "communicate", "tell"]
kind = "task"
forth = '." hello world" cr'

[[word]]
word = "write"
definition = "to mark symbols on a surface to represent language"
related = ["read", "word", "record", "express"]
kind = "task"
forth = '72 101 108 108 111 5 0 do emit loop cr'

[[word]]
word = "read"
definition = "to interpret written or printed symbols"
related = ["write", "understand", "parse", "word"]
kind = "task"
forth = '." reading..." cr'

[[word]]
word = "true"
definition = "in accordance with fact or reality"
related = ["false", "proof", "fact", "know"]
kind = "observation"
forth = '-1 . cr'

[[word]]
word = "false"
definition = "not in accordance with fact or reality"
related = ["true", "not", "error", "wrong"]
kind = "observation"
forth = '0 . cr'

[[word]]
word = "not"
definition = "the logical negation of a proposition"
related = ["true", "false", "negate", "opposite"]
kind = "observation"
forth = '1 0= . cr'

[[word]]
word = "and"
definition = "the logical conjunction of two propositions"
related = ["or", "both", "together", "with"]
kind = "observation"
forth = '-1 -1 and . cr'

[[word]]
word = "or"
definition = "the logical disjunction of two propositions"
related = ["and", "either", "choose", "if"]
kind = "observation"
forth = '0 -1 or . cr'

[[word]]
word = "if"
definition = "a conditional relating cause to effect"
related = ["then", "condition", "cause", "rule"]
kind = "observation"
forth = '5 3 > if ." yes" else ." no" then cr'

[[word]]
word = "then"
definition = "what follows from a condition being true"
related = ["if", "next", "result", "after"]
kind = "observation"
forth = '1 if ." then this" then cr'

[[word]]
word = "self"
definition = "the entity that is the subject of experience and action"
related = ["other", "identity", "mind", "body"]
kind = "observation"
forth = 'depth . ." items on stack" cr'

[[word]]
word = "other"
definition = "not the same as the one already mentioned"
related = ["self", "different", "else", "not"]
kind = "observation"
forth = '1 0 <> . cr'

[[word]]
word = "mind"
definition = "the faculty of consciousness, thought, and feeling"
related = ["think", "know", "self", "brain"]
kind = "observation"
forth = '6 fib . cr'

[[word]]
word = "body"
definition = "the physical structure of a living thing"
related = ["mind", "self", "move", "space"]
kind = "observation"
forth = '3 4 5 + + . cr'

[[word]]
word = "life"
definition = "the condition that distinguishes organisms from inorganic matter"
related = ["body", "grow", "time", "death"]
kind = "observation"
forth = '8 0 do i . loop cr'

[[word]]
word = "death"
definition = "the cessation of life"
related = ["life", "end", "change", "time"]
kind = "observation"
forth = '0 . cr'

[[word]]
word = "love"
definition = "a deep feeling of affection and attachment"
related = ["care", "want", "feel", "connect"]
kind = "observation"
forth = '5 7 + . cr'

[[word]]
word = "fear"
definition = "an unpleasant emotion caused by perceived danger"
related = ["danger", "avoid", "protect", "feel"]
kind = "observation"
forth = '-3 abs . cr'

[[word]]
word = "feel"
definition = "to be aware of through sensation or emotion"
related = ["sense", "know", "body", "mind"]
kind = "observation"
forth = '3 4 max . cr'

[[word]]
word = "sense"
definition = "a faculty by which the body perceives external stimuli"
related = ["feel", "observe", "body", "signal"]
kind = "observation"
forth = '5 0 do i 2* . loop cr'

[[word]]
word = "signal"
definition = "a detectable change that conveys information"
related = ["sense", "message", "communicate", "event"]
kind = "observation"
forth = '1 0 1 1 0 5 0 do . loop cr'

[[word]]
word = "event"
definition = "something that happens at a specific time and place"
related = ["time", "cause", "effect", "happen"]
kind = "observation"
forth = 'time . cr'

[[word]]
word = "happen"
definition = "to come to pass; to occur"
related = ["event", "cause", "time", "change"]
kind = "observation"
forth = '1 . cr'

[[word]]
word = "begin"
definition = "to start or come into existence"
related = ["end", "first", "create", "time"]
kind = "task"
forth = '0 . cr'

[[word]]
word = "end"
definition = "the final point of something"
related = ["begin", "last", "stop", "complete"]
kind = "observation"
forth = '10 sum-to-n . cr'

[[word]]
word = "complete"
definition = "having all necessary parts; finished"
related = ["whole", "end", "done", "all"]
kind = "observation"
forth = '5 0 do i . loop ." done" cr'

[[word]]
word = "simple"
definition = "not complex; composed of few parts"
related = ["complex", "clear", "easy", "part"]
kind = "observation"
forth = '2 . cr'

[[word]]
word = "complex"
definition = "composed of many interconnected parts"
related = ["simple", "system", "structure", "many"]
kind = "observation"
forth = '8 fib . cr'

[[word]]
word = "abstract"
definition = "existing as an idea, not as a concrete object"
related = ["concrete", "concept", "general", "idea"]
kind = "observation"
forth = '." an idea without a specific referent" cr'

[[word]]
word = "concrete"
definition = "specific and tangible rather than abstract"
related = ["abstract", "real", "thing", "example"]
kind = "observation"
forth = '42 . cr'

[[word]]
word = "example"
definition = "a particular instance that illustrates a general rule"
related = ["rule", "case", "concrete", "show"]
kind = "observation"
forth = '5 square . ." = 5^2" cr'

[[word]]
word = "general"
definition = "applying to all or most cases"
related = ["specific", "abstract", "rule", "all"]
kind = "observation"
forth = '10 0 do i . loop cr'

[[word]]
word = "specific"
definition = "clearly defined or identified"
related = ["general", "name", "concrete", "one"]
kind = "observation"
forth = '7 . cr'

[[word]]
word = "build"
definition = "to construct by assembling parts"
related = ["make", "create", "structure", "part"]
kind = "task"
forth = '4 square . cr'

[[word]]
word = "test"
definition = "to examine to find out something"
related = ["observe", "verify", "question", "measure"]
kind = "task"
forth = '7 7 = . cr'

[[word]]
word = "fix"
definition = "to repair or correct something"
related = ["error", "change", "solve", "make"]
kind = "task"
forth = '-3 abs . cr'

[[word]]
word = "error"
definition = "a mistake or incorrect result"
related = ["false", "fix", "wrong", "deviation"]
kind = "observation"
forth = '0 . cr'

[[word]]
word = "program"
definition = "a sequence of instructions for a computer to execute"
related = ["compute", "algorithm", "language", "execute"]
kind = "task"
forth = '5 0 do i . loop cr'

[[word]]
word = "execute"
definition = "to carry out instructions or a plan"
related = ["run", "do", "compute", "program"]
kind = "task"
forth = '6 7 * . cr'

[[word]]
word = "run"
definition = "to execute a program or process"
related = ["execute", "start", "compute", "do"]
kind = "task"
forth = '5 0 do i square . loop cr'

[[word]]
word = "stack"
definition = "a data structure where items are added and removed from the top"
related = ["push", "pop", "last", "structure"]
kind = "observation"
forth = '1 2 3 .s cr'

[[word]]
word = "push"
definition = "to add an item to the top of a stack"
related = ["stack", "pop", "add", "above"]
kind = "task"
forth = '1 2 3 dup .s cr'

[[word]]
word = "pop"
definition = "to remove the top item from a stack"
related = ["stack", "push", "remove", "top"]
kind = "task"
forth = '1 2 3 drop .s cr'

[[word]]
word = "forth"
definition = "a stack-based programming language where words call other words"
related = ["stack", "word", "define", "execute"]
kind = "observation"
forth = 'words cr'

[[word]]
word = "beauty"
definition = "that which gives pleasure to the senses or the mind"
related = ["grace", "wonder", "art", "truth"]
kind = "observation"

[[word]]
word = "truth"
definition = "what corresponds to reality, undistorted by wish or fear"
related = ["fact", "honest", "know", "real"]
kind = "observation"

[[word]]
word = "justice"
definition = "the quality of being fair and reasonable in action or judgement"
related = ["fair", "law", "equal", "right"]
kind = "observation"

[[word]]
word = "freedom"
definition = "the power to act, speak, or think without external restraint"
related = ["choice", "will", "open", "bound"]
kind = "observation"

[[word]]
word = "peace"
definition = "a state of tranquility, free from conflict or disturbance"
related = ["calm", "quiet", "rest", "harmony"]
kind = "observation"

[[word]]
word = "hope"
definition = "a desire for something with expectation of its fulfilment"
related = ["wish", "want", "future", "trust"]
kind = "observation"

[[word]]
word = "trust"
definition = "firm belief in the reliability, truth, or ability of something"
related = ["believe", "faith", "safe", "hope"]
kind = "observation"

[[word]]
word = "wonder"
definition = "a feeling of amazement at something beautiful, unexpected, or inexplicable"
related = ["awe", "curiosity", "beauty", "surprise"]
kind = "observation"

[[word]]
word = "grace"
definition = "a quality of effortless elegance and ease of movement or manner"
related = ["beauty", "ease", "gift", "kind"]
kind = "observation"

[[word]]
word = "gift"
definition = "something given freely, without expectation of return"
related = ["give", "grace", "love", "share"]
kind = "observation"

[[word]]
word = "dream"
definition = "a cherished aspiration or image of what could be"
related = ["hope", "imagine", "sleep", "future"]
kind = "observation"

[[word]]
word = "memory"
definition = "the faculty by which the mind stores and recalls past experience"
related = ["remember", "past", "time", "learn"]
kind = "observation"

[[word]]
word = "story"
definition = "an account of events, real or imagined, with a beginning and end"
related = ["word", "tell", "time", "meaning"]
kind = "observation"

[[word]]
word = "heart"
definition = "the centre of feeling, courage, and emotional life"
related = ["love", "feel", "body", "soul"]
kind = "observation"

[[word]]
word = "soul"
definition = "the immaterial essence, the seat of identity and feeling"
related = ["mind", "heart", "self", "spirit"]
kind = "observation"

[[word]]
word = "spirit"
definition = "the vital principle animating a person; force and courage"
related = ["soul", "life", "energy", "will"]
kind = "observation"

[[word]]
word = "will"
definition = "the faculty by which a person decides and initiates action"
related = ["choose", "intent", "purpose", "do"]
kind = "observation"

[[word]]
word = "choice"
definition = "an act of selecting between two or more possibilities"
related = ["will", "decide", "freedom", "option"]
kind = "task"

[[word]]
word = "voice"
definition = "the sound produced by a person; the power to express oneself"
related = ["speak", "language", "sound", "say"]
kind = "observation"

[[word]]
word = "silence"
definition = "the complete absence of sound; the state of saying nothing"
related = ["quiet", "peace", "voice", "wait"]
kind = "observation"

[[word]]
word = "person"
definition = "an individual human being with consciousness and identity"
related = ["self", "human", "mind", "body"]
kind = "observation"

[[word]]
word = "human"
definition = "of or belonging to the species Homo sapiens"
related = ["person", "life", "mind", "social"]
kind = "observation"

[[word]]
word = "family"
definition = "a group of people bound by kinship, love, or shared life"
related = ["mother", "father", "child", "home"]
kind = "observation"

[[word]]
word = "mother"
definition = "a woman who has given birth to or raised a child"
related = ["family", "care", "love", "child"]
kind = "observation"

[[word]]
word = "father"
definition = "a man who has begotten or raised a child"
related = ["family", "care", "teach", "child"]
kind = "observation"

[[word]]
word = "child"
definition = "a young human being between infancy and adolescence"
related = ["family", "learn", "grow", "play"]
kind = "observation"

[[word]]
word = "friend"
definition = "a person with whom one shares mutual affection and trust"
related = ["trust", "love", "social", "together"]
kind = "observation"

[[word]]
word = "stranger"
definition = "a person not known or familiar; someone outside one's circle"
related = ["other", "unknown", "meet", "new"]
kind = "observation"

[[word]]
word = "home"
definition = "the place where one lives; a feeling of belonging"
related = ["place", "family", "safe", "here"]
kind = "observation"

[[word]]
word = "place"
definition = "a particular portion of space with a distinct character"
related = ["here", "space", "where", "home"]
kind = "observation"

[[word]]
word = "happy"
definition = "feeling or showing pleasure, contentment, or satisfaction"
related = ["joy", "good", "feel", "smile"]
kind = "observation"

[[word]]
word = "sad"
definition = "feeling sorrow or unhappiness"
related = ["grief", "feel", "loss", "alone"]
kind = "observation"

[[word]]
word = "anger"
definition = "a strong feeling of displeasure and opposition"
related = ["feel", "conflict", "fire", "strong"]
kind = "observation"

[[word]]
word = "joy"
definition = "a feeling of great pleasure and happiness"
related = ["happy", "love", "play", "light"]
kind = "observation"

[[word]]
word = "grief"
definition = "deep sorrow caused by loss"
related = ["sad", "loss", "love", "time"]
kind = "observation"

[[word]]
word = "alone"
definition = "without other people; separate from others"
related = ["self", "silence", "other", "apart"]
kind = "observation"

[[word]]
word = "together"
definition = "in company with others; in the same place or time"
related = ["join", "with", "friend", "share"]
kind = "observation"

[[word]]
word = "share"
definition = "to have or use something jointly with others"
related = ["give", "together", "divide", "open"]
kind = "task"

[[word]]
word = "give"
definition = "to freely transfer something to another person"
related = ["gift", "share", "love", "take"]
kind = "task"

[[word]]
word = "take"
definition = "to receive, grasp, or remove something"
related = ["give", "get", "hold", "receive"]
kind = "task"

[[word]]
word = "ask"
definition = "to request information or a favour from someone"
related = ["question", "seek", "want", "need"]
kind = "task"

[[word]]
word = "say"
definition = "to utter words; to express in language"
related = ["speak", "tell", "voice", "word"]
kind = "task"

[[word]]
word = "tell"
definition = "to communicate information to someone"
related = ["say", "show", "explain", "word"]
kind = "task"

[[word]]
word = "wait"
definition = "to remain in one place or state until something happens"
related = ["time", "patience", "hold", "ready"]
kind = "task"

[[word]]
word = "need"
definition = "to require something as essential or very important"
related = ["want", "require", "use", "must"]
kind = "observation"

[[word]]
word = "want"
definition = "to have a desire for something"
related = ["need", "wish", "hope", "desire"]
kind = "observation"

[[word]]
word = "try"
definition = "to make an attempt or effort to do something"
related = ["do", "fail", "succeed", "effort"]
kind = "task"

[[word]]
word = "fail"
definition = "to not achieve what was intended"
related = ["try", "error", "learn", "again"]
kind = "observation"

[[word]]
word = "succeed"
definition = "to achieve the intended goal or outcome"
related = ["try", "goal", "complete", "win"]
kind = "observation"

[[word]]
word = "grow"
definition = "to increase in size, ability, or complexity over time"
related = ["change", "learn", "life", "time"]
kind = "observation"

[[word]]
word = "break"
definition = "to cause something to separate into pieces; to interrupt"
related = ["stop", "change", "destroy", "fix"]
kind = "task"

[[word]]
word = "hold"
definition = "to keep in a particular position; to possess"
related = ["keep", "grip", "contain", "safe"]
kind = "task"

[[word]]
word = "find"
definition = "to discover or locate something by searching"
related = ["search", "get", "see", "know"]
kind = "task"

[[word]]
word = "lose"
definition = "to no longer have or be able to find something"
related = ["find", "grief", "fail", "gone"]
kind = "observation"

[[word]]
word = "stay"
definition = "to remain in a place or state"
related = ["hold", "here", "wait", "keep"]
kind = "task"

[[word]]
word = "leave"
definition = "to go away from a place or person"
related = ["go", "end", "goodbye", "away"]
kind = "task"

[[word]]
word = "come"
definition = "to move toward a place or person"
related = ["go", "arrive", "here", "meet"]
kind = "task"

[[word]]
word = "meet"
definition = "to come into the presence of; to encounter"
related = ["come", "hello", "know", "join"]
kind = "task"

[[word]]
word = "call"
definition = "to name; to summon; to contact someone"
related = ["name", "speak", "ask", "send"]
kind = "task"

[[word]]
word = "help"
definition = "to make something easier or better for someone"
related = ["give", "support", "care", "do"]
kind = "task"

[[word]]
word = "care"
definition = "to feel concern for; to look after"
related = ["love", "help", "protect", "feel"]
kind = "observation"

[[word]]
word = "protect"
definition = "to keep safe from harm or danger"
related = ["care", "safe", "guard", "prevent"]
kind = "task"

[[word]]
word = "safe"
definition = "protected from danger, risk, or harm"
related = ["protect", "trust", "home", "secure"]
kind = "observation"

[[word]]
word = "danger"
definition = "the possibility of suffering harm or injury"
related = ["fear", "risk", "warn", "unsafe"]
kind = "observation"

[[word]]
word = "new"
definition = "recently made or discovered; not existing before"
related = ["first", "begin", "change", "young"]
kind = "observation"

[[word]]
word = "old"
definition = "having existed for a long time; ancient"
related = ["time", "past", "memory", "young"]
kind = "observation"

[[word]]
word = "young"
definition = "in the early stage of life or existence"
related = ["new", "child", "grow", "fresh"]
kind = "observation"

[[word]]
word = "ancient"
definition = "belonging to the very distant past"
related = ["old", "time", "history", "long"]
kind = "observation"

[[word]]
word = "light"
definition = "electromagnetic radiation visible to the eye; the opposite of dark"
related = ["sun", "see", "energy", "dark"]
kind = "observation"

[[word]]
word = "dark"
definition = "the absence of light; unknown or secret"
related = ["light", "night", "shadow", "fear"]
kind = "observation"

[[word]]
word = "sun"
definition = "the star at the centre of our solar system; a source of warmth and light"
related = ["light", "day", "warm", "sky"]
kind = "observation"

[[word]]
word = "moon"
definition = "the natural satellite of the earth; a measure of months"
related = ["night", "light", "time", "sky"]
kind = "observation"

[[word]]
word = "star"
definition = "a celestial body of glowing plasma; a point of light in the night sky"
related = ["moon", "sky", "light", "far"]
kind = "observation"

[[word]]
word = "sky"
definition = "the expanse of space above the earth; the apparent vault overhead"
related = ["air", "cloud", "sun", "above"]
kind = "observation"

[[word]]
word = "ocean"
definition = "a vast body of salt water covering most of the earth's surface"
related = ["water", "deep", "wave", "wide"]
kind = "observation"

[[word]]
word = "river"
definition = "a large natural stream of water flowing toward a larger body of water"
related = ["water", "flow", "change", "ocean"]
kind = "observation"

[[word]]
word = "mountain"
definition = "a large natural elevation of earth higher than surrounding terrain"
related = ["high", "stone", "sky", "above"]
kind = "observation"

[[word]]
word = "stone"
definition = "a hard compact non-metallic mineral matter; a rock"
related = ["earth", "hard", "solid", "ancient"]
kind = "observation"

[[word]]
word = "tree"
definition = "a tall perennial plant with a woody trunk and branches"
related = ["leaf", "grow", "root", "forest"]
kind = "observation"

[[word]]
word = "flower"
definition = "the seed-bearing part of a plant; something beautiful and transient"
related = ["tree", "beauty", "grow", "color"]
kind = "observation"

[[word]]
word = "seed"
definition = "a plant's unit of reproduction; the origin of something"
related = ["tree", "begin", "grow", "potential"]
kind = "observation"

[[word]]
word = "root"
definition = "the part of a plant anchoring it in soil; the origin of something"
related = ["seed", "tree", "below", "anchor"]
kind = "observation"

[[word]]
word = "rain"
definition = "water falling in drops from clouds; nourishment from above"
related = ["water", "cloud", "sky", "grow"]
kind = "observation"

[[word]]
word = "wind"
definition = "air moving horizontally across the earth's surface"
related = ["air", "move", "change", "sky"]
kind = "observation"

[[word]]
word = "fire"
definition = "rapid oxidation producing heat and light; passion or intensity"
related = ["heat", "light", "energy", "destroy"]
kind = "observation"

[[word]]
word = "cold"
definition = "having a low temperature; lacking warmth or emotion"
related = ["warm", "ice", "winter", "freeze"]
kind = "observation"

[[word]]
word = "warm"
definition = "having a moderate degree of heat; showing kindness"
related = ["cold", "fire", "care", "comfortable"]
kind = "observation"

[[word]]
word = "touch"
definition = "to make contact with; the sense of physical contact"
related = ["feel", "body", "hand", "sense"]
kind = "observation"

[[word]]
word = "sound"
definition = "vibrations that travel through air and are heard by the ear"
related = ["hear", "voice", "music", "wave"]
kind = "observation"

[[word]]
word = "silence"
definition = "the complete absence of sound; a pause between words"
related = ["quiet", "peace", "voice", "wait"]
kind = "observation"

[[word]]
word = "breath"
definition = "air taken into or expelled from the lungs; a pause, a moment"
related = ["body", "life", "air", "moment"]
kind = "observation"

[[word]]
word = "movement"
definition = "an act of moving; change in position or condition"
related = ["change", "flow", "go", "action"]
kind = "observation"

[[word]]
word = "flow"
definition = "to move in a steady continuous stream"
related = ["river", "movement", "change", "smooth"]
kind = "observation"

[[word]]
word = "moment"
definition = "a very brief period of time; a point of time now"
related = ["now", "time", "instant", "present"]
kind = "observation"

[[word]]
word = "present"
definition = "the period of time occurring now; to bring or offer something"
related = ["now", "moment", "here", "gift"]
kind = "observation"

[[word]]
word = "past"
definition = "the time before the present; events that have already occurred"
related = ["memory", "before", "old", "time"]
kind = "observation"

[[word]]
word = "future"
definition = "the time yet to come; what has not yet occurred"
related = ["hope", "plan", "change", "after"]
kind = "observation"

[[word]]
word = "morning"
definition = "the period of the day from dawn until noon"
related = ["day", "begin", "light", "fresh"]
kind = "observation"

[[word]]
word = "evening"
definition = "the period of the day between afternoon and night"
related = ["night", "end", "rest", "day"]
kind = "observation"

[[word]]
word = "night"
definition = "the period of darkness between one day and the next"
related = ["dark", "moon", "sleep", "evening"]
kind = "observation"

[[word]]
word = "sleep"
definition = "a naturally recurring state of rest for mind and body"
related = ["dream", "rest", "night", "quiet"]
kind = "observation"

[[word]]
word = "wake"
definition = "to emerge from sleep; to become aware"
related = ["sleep", "morning", "begin", "notice"]
kind = "task"

[[word]]
word = "walk"
definition = "to move forward by taking steps at a moderate pace"
related = ["move", "go", "path", "body"]
kind = "task"

[[word]]
word = "path"
definition = "a route taken to reach a destination; a course of action"
related = ["walk", "go", "goal", "way"]
kind = "observation"

[[word]]
word = "door"
definition = "a movable barrier for entering or closing an opening"
related = ["open", "close", "home", "enter"]
kind = "observation"

[[word]]
word = "window"
definition = "an opening in a wall that allows light and air to pass through"
related = ["light", "see", "open", "glass"]
kind = "observation"

[[word]]
word = "book"
definition = "a written work bound between covers; a record of knowledge"
related = ["read", "write", "word", "learn"]
kind = "observation"

[[word]]
word = "letter"
definition = "a written message sent to someone; a symbol of an alphabet"
related = ["word", "write", "alphabet", "send"]
kind = "observation"

[[word]]
word = "paper"
definition = "thin material used for writing or printing"
related = ["write", "word", "record", "flat"]
kind = "observation"

[[word]]
word = "table"
definition = "a flat-topped surface supported on legs; an organised display of data"
related = ["flat", "surface", "structure", "work"]
kind = "observation"

[[word]]
word = "chair"
definition = "a separate seat for one person, with a back and typically four legs"
related = ["sit", "rest", "home", "table"]
kind = "observation"

[[word]]
word = "food"
definition = "any nutritious substance that organisms consume to maintain life"
related = ["eat", "body", "grow", "life"]
kind = "observation"

[[word]]
word = "eat"
definition = "to put food into the mouth and swallow it"
related = ["food", "body", "taste", "live"]
kind = "task"

[[word]]
word = "drink"
definition = "to take liquid into the mouth and swallow it"
related = ["water", "eat", "body", "thirst"]
kind = "task"

[[word]]
word = "water"
definition = "a transparent liquid that is the basis of all life on earth"
related = ["ocean", "river", "drink", "clean"]
kind = "observation"

[[word]]
word = "music"
definition = "art organised in patterns of sound and silence through time"
related = ["sound", "rhythm", "harmony", "feel"]
kind = "observation"

[[word]]
word = "art"
definition = "the expression of human creativity, skill, and imagination"
related = ["beauty", "create", "meaning", "form"]
kind = "observation"

[[word]]
word = "play"
definition = "to engage in activity for enjoyment; to perform music"
related = ["joy", "game", "child", "free"]
kind = "task"

[[word]]
word = "game"
definition = "an activity engaged in for diversion, with rules and goals"
related = ["play", "rule", "win", "fun"]
kind = "observation"

[[word]]
word = "work"
definition = "purposeful activity that produces or accomplishes something"
related = ["do", "goal", "effort", "result"]
kind = "task"

[[word]]
word = "rest"
definition = "freedom from activity; refreshing ease after exertion"
related = ["sleep", "peace", "pause", "end"]
kind = "observation"

[[word]]
word = "effort"
definition = "a vigorous or determined attempt to do something"
related = ["try", "work", "will", "do"]
kind = "task"

[[word]]
word = "energy"
definition = "the capacity for doing work; vitality and enthusiasm"
related = ["force", "power", "life", "do"]
kind = "observation"

[[word]]
word = "power"
definition = "the capacity to influence, control, or do"
related = ["energy", "force", "will", "authority"]
kind = "observation"

[[word]]
word = "strong"
definition = "having great physical or mental power; firmly established"
related = ["power", "solid", "hard", "stable"]
kind = "observation"

[[word]]
word = "gentle"
definition = "mild, kind, and soft in character or manner"
related = ["soft", "care", "kind", "calm"]
kind = "observation"

[[word]]
word = "kind"
definition = "having a friendly, generous, and considerate nature"
related = ["gentle", "care", "love", "give"]
kind = "observation"

[[word]]
word = "honest"
definition = "free of deceit; truthful and sincere"
related = ["truth", "trust", "clear", "fair"]
kind = "observation"

[[word]]
word = "fair"
definition = "treating people equally and without bias"
related = ["justice", "equal", "honest", "right"]
kind = "observation"

[[word]]
word = "right"
definition = "morally good; correct; a just claim or entitlement"
related = ["fair", "good", "true", "justice"]
kind = "observation"

[[word]]
word = "wrong"
definition = "not correct; unjust; contrary to what is right"
related = ["right", "error", "false", "fix"]
kind = "observation"

[[word]]
word = "good"
definition = "having the qualities required for a purpose; morally right"
related = ["right", "kind", "well", "true"]
kind = "observation"

[[word]]
word = "bad"
definition = "of poor quality; unpleasant; morally wrong"
related = ["wrong", "error", "evil", "poor"]
kind = "observation"

[[word]]
word = "beautiful"
definition = "pleasing the senses or mind aesthetically"
related = ["beauty", "art", "grace", "wonder"]
kind = "observation"

[[word]]
word = "broken"
definition = "having been fractured or damaged; no longer working"
related = ["break", "fix", "error", "whole"]
kind = "observation"

[[word]]
word = "open"
definition = "not closed or blocked; willing to receive or consider"
related = ["free", "door", "begin", "share"]
kind = "observation"

[[word]]
word = "close"
definition = "to shut; to come near; intimate in relationship"
related = ["end", "door", "near", "tight"]
kind = "task"

[[word]]
word = "deep"
definition = "extending far down; intense; not superficial"
related = ["below", "ocean", "profound", "hidden"]
kind = "observation"

[[word]]
word = "wide"
definition = "extending far from side to side; broad in scope"
related = ["broad", "open", "space", "large"]
kind = "observation"

[[word]]
word = "long"
definition = "of great extent in time or space"
related = ["far", "time", "wide", "sequence"]
kind = "observation"

[[word]]
word = "short"
definition = "measuring a small distance; brief in time"
related = ["long", "small", "near", "quick"]
kind = "observation"

[[word]]
word = "near"
definition = "at or within a short distance; close in relationship"
related = ["close", "here", "friend", "touch"]
kind = "observation"

[[word]]
word = "far"
definition = "at or to a great distance; remote"
related = ["near", "away", "star", "horizon"]
kind = "observation"

[[word]]
word = "here"
definition = "in, at, or to this place or position"
related = ["near", "place", "now", "present"]
kind = "observation"

[[word]]
word = "there"
definition = "in, at, or to that place or position"
related = ["here", "far", "place", "point"]
kind = "observation"

[[word]]
word = "up"
definition = "toward a higher place or level; increasing"
related = ["above", "high", "grow", "more"]
kind = "observation"

[[word]]
word = "down"
definition = "toward a lower place or level; decreasing"
related = ["below", "fall", "less", "below"]
kind = "observation"

[[word]]
word = "in"
definition = "expressing inclusion, location inside, or a state"
related = ["inside", "contain", "within", "part"]
kind = "observation"

[[word]]
word = "out"
definition = "away from the inside; not participating; beyond"
related = ["outside", "beyond", "leave", "open"]
kind = "observation"

[[word]]
word = "back"
definition = "to a previous place or condition; the rear part"
related = ["return", "past", "behind", "reverse"]
kind = "observation"

[[word]]
word = "again"
definition = "another time; once more; back to a previous state"
related = ["repeat", "back", "try", "cycle"]
kind = "observation"

[[word]]
word = "always"
definition = "at all times; on all occasions; continually"
related = ["constant", "every", "never", "forever"]
kind = "observation"

[[word]]
word = "never"
definition = "at no time; not ever; not in any circumstances"
related = ["always", "zero", "not", "void"]
kind = "observation"

[[word]]
word = "every"
definition = "each and all of a group without exception"
related = ["all", "whole", "each", "always"]
kind = "observation"

[[word]]
word = "some"
definition = "an unspecified amount or number of; part of"
related = ["part", "few", "any", "select"]
kind = "observation"

[[word]]
word = "all"
definition = "the whole quantity or extent of; everyone"
related = ["every", "whole", "complete", "total"]
kind = "observation"

[[word]]
word = "none"
definition = "not any; no one; not one of a group"
related = ["empty", "zero", "nothing", "void"]
kind = "observation"

[[word]]
word = "much"
definition = "a large amount of; to a great degree"
related = ["many", "big", "more", "lot"]
kind = "observation"

[[word]]
word = "little"
definition = "small in size or amount; not much"
related = ["small", "few", "less", "child"]
kind = "observation"

[[word]]
word = "enough"
definition = "as much or as many as required; sufficient"
related = ["full", "complete", "adequate", "satisfy"]
kind = "observation"

[[word]]
word = "more"
definition = "a greater or additional amount or degree"
related = ["add", "grow", "much", "increase"]
kind = "observation"

[[word]]
word = "less"
definition = "a smaller amount or degree"
related = ["reduce", "fewer", "small", "decrease"]
kind = "observation"

[[word]]
word = "first"
definition = "coming before all others in order or importance"
related = ["begin", "one", "head", "start"]
kind = "observation"

[[word]]
word = "last"
definition = "coming after all others; final; to continue or endure"
related = ["end", "final", "remain", "stay"]
kind = "observation"

[[word]]
word = "only"
definition = "solely; without others; single"
related = ["one", "alone", "single", "unique"]
kind = "observation"

[[word]]
word = "both"
definition = "the two; each of the two"
related = ["two", "together", "pair", "and"]
kind = "observation"

[[word]]
word = "each"
definition = "every one of a group considered individually"
related = ["every", "one", "all", "each"]
kind = "observation"

[[word]]
word = "between"
definition = "in the interval or space separating two things"
related = ["middle", "among", "gap", "relate"]
kind = "observation"

[[word]]
word = "among"
definition = "in the company or number of; surrounded by"
related = ["between", "within", "group", "together"]
kind = "observation"

[[word]]
word = "through"
definition = "moving in one side and out the other; by means of"
related = ["between", "path", "complete", "within"]
kind = "observation"

[[word]]
word = "across"
definition = "from one side to another; throughout"
related = ["through", "wide", "move", "bridge"]
kind = "observation"

[[word]]
word = "against"
definition = "in opposition to; in contact with; as protection from"
related = ["conflict", "opposite", "resist", "boundary"]
kind = "observation"

[[word]]
word = "without"
definition = "in the absence of; lacking"
related = ["empty", "alone", "none", "outside"]
kind = "observation"

[[word]]
word = "beyond"
definition = "on the far side of; outside the range of"
related = ["far", "out", "above", "infinite"]
kind = "observation"

[[word]]
word = "toward"
definition = "in the direction of; approaching"
related = ["near", "go", "move", "come"]
kind = "observation"

[[word]]
word = "still"
definition = "not moving; quiet; continuing up to a time"
related = ["quiet", "hold", "peace", "yet"]
kind = "observation"

[[word]]
word = "yet"
definition = "up to a particular time; still; in addition"
related = ["still", "now", "more", "but"]
kind = "observation"

[[word]]
word = "even"
definition = "flat; equal; despite the fact that; not odd"
related = ["flat", "equal", "smooth", "fair"]
kind = "observation"

[[word]]
word = "just"
definition = "exactly; only recently; morally right"
related = ["fair", "right", "honest", "equal"]
kind = "observation"

[[word]]
word = "quite"
definition = "to the fullest extent; fairly; somewhat"
related = ["very", "complete", "enough", "much"]
kind = "observation"

[[word]]
word = "very"
definition = "to a high degree; used as an intensifier"
related = ["much", "strong", "deep", "truly"]
kind = "observation"

[[word]]
word = "really"
definition = "in actual fact; used for emphasis"
related = ["true", "real", "actually", "very"]
kind = "observation"

[[word]]
word = "almost"
definition = "very nearly but not exactly or entirely"
related = ["nearly", "close", "limit", "almost"]
kind = "observation"

[[word]]
word = "already"
definition = "before or by a specified time; even now"
related = ["before", "done", "past", "now"]
kind = "observation"

[[word]]
word = "become"
definition = "to begin to be; to come to be something"
related = ["change", "grow", "transform", "is"]
kind = "observation"

[[word]]
word = "seem"
definition = "to give the impression of being; to appear"
related = ["appear", "look", "feel", "suggest"]
kind = "observation"

[[word]]
word = "keep"
definition = "to have or retain possession of; to continue"
related = ["hold", "stay", "maintain", "preserve"]
kind = "task"

[[word]]
word = "start"
definition = "to begin; to set in motion"
related = ["begin", "first", "go", "create"]
kind = "task"

[[word]]
word = "stop"
definition = "to cease moving or operating; to bring to an end"
related = ["end", "pause", "halt", "done"]
kind = "task"

[[word]]
word = "continue"
definition = "to persist in an activity; to go on after a pause"
related = ["keep", "again", "stay", "more"]
kind = "task"

[[word]]
word = "return"
definition = "to come or go back; to give something back"
related = ["back", "again", "give", "restore"]
kind = "task"

[[word]]
word = "show"
definition = "to cause or allow to be seen; to demonstrate"
related = ["see", "teach", "prove", "reveal"]
kind = "task"

[[word]]
word = "believe"
definition = "to accept something as true; to have faith in"
related = ["trust", "know", "faith", "think"]
kind = "observation"

[[word]]
word = "doubt"
definition = "to be uncertain about; to question the truth of"
related = ["question", "uncertain", "fear", "think"]
kind = "observation"

[[word]]
word = "forget"
definition = "to fail to remember; to leave behind unintentionally"
related = ["memory", "lose", "past", "gone"]
kind = "observation"

[[word]]
word = "remember"
definition = "to recall from memory; to keep in mind"
related = ["memory", "past", "know", "hold"]
kind = "task"

[[word]]
word = "imagine"
definition = "to form a mental picture or concept of something not present"
related = ["dream", "create", "mind", "idea"]
kind = "task"

[[word]]
word = "listen"
definition = "to give attention to sound; to pay heed"
related = ["hear", "sound", "quiet", "attend"]
kind = "task"

[[word]]
word = "look"
definition = "to direct the eyes in order to see"
related = ["see", "find", "observe", "eye"]
kind = "task"

[[word]]
word = "follow"
definition = "to come after; to pursue; to obey"
related = ["lead", "path", "next", "after"]
kind = "task"

[[word]]
word = "lead"
definition = "to go before and show the way; to direct"
related = ["guide", "first", "follow", "path"]
kind = "task"

[[word]]
word = "carry"
definition = "to hold and transport something from one place to another"
related = ["hold", "move", "bring", "transport"]
kind = "task"

[[word]]
word = "send"
definition = "to cause to go or be taken to a destination"
related = ["give", "transmit", "signal", "call"]
kind = "task"

[[word]]
word = "receive"
definition = "to be given, presented with, or paid something"
related = ["take", "get", "accept", "hear"]
kind = "task"

[[word]]
word = "open"
definition = "not closed or blocked; receptive to new ideas"
related = ["free", "begin", "welcome", "enter"]
kind = "observation"

[[word]]
word = "choose"
definition = "to select from a number of possibilities"
related = ["will", "decide", "option", "freedom"]
kind = "task"

[[word]]
word = "decide"
definition = "to come to a conclusion about; to settle a matter"
related = ["choose", "judge", "will", "know"]
kind = "task"

[[word]]
word = "allow"
definition = "to permit; to let something happen"
related = ["permit", "free", "accept", "open"]
kind = "task"

[[word]]
word = "prevent"
definition = "to keep something from happening"
related = ["stop", "protect", "avoid", "guard"]
kind = "task"

[[word]]
word = "create"
definition = "to bring something into existence"
related = ["make", "build", "art", "begin"]
kind = "task"

[[word]]
word = "destroy"
definition = "to put an end to the existence of; to ruin"
related = ["break", "end", "remove", "damage"]
kind = "task"

[[word]]
word = "gather"
definition = "to bring together from various places; to collect"
related = ["collect", "join", "share", "group"]
kind = "task"

[[word]]
word = "spread"
definition = "to extend over a large area; to share widely"
related = ["grow", "open", "share", "wide"]
kind = "task"

[[word]]
word = "connect"
definition = "to join or link together"
related = ["join", "relate", "network", "bridge"]
kind = "task"

[[word]]
word = "separate"
definition = "to move or come apart; to divide from a whole"
related = ["break", "alone", "split", "apart"]
kind = "task"

[[word]]
word = "whole"
definition = "a complete entity made up of parts; all of something"
related = ["part", "complete", "all", "system"]
kind = "observation"

[[word]]
word = "part"
definition = "a piece or segment of a whole"
related = ["whole", "component", "element", "piece"]
kind = "observation"

[[word]]
word = "piece"
definition = "a portion of an object or amount forming a whole"
related = ["part", "fragment", "element", "small"]
kind = "observation"

[[word]]
word = "center"
definition = "the middle point of something; the focus of attention"
related = ["middle", "balance", "core", "between"]
kind = "observation"

[[word]]
word = "edge"
definition = "the boundary of a surface; a sharp side"
related = ["boundary", "limit", "end", "sharp"]
kind = "observation"

[[word]]
word = "boundary"
definition = "a line marking the limits of an area or concept"
related = ["edge", "limit", "between", "wall"]
kind = "observation"

[[word]]
word = "bridge"
definition = "a structure spanning an obstacle; a connection between two things"
related = ["connect", "cross", "between", "path"]
kind = "observation"

[[word]]
word = "wall"
definition = "a vertical structure forming a boundary or enclosure"
related = ["boundary", "protect", "inside", "separate"]
kind = "observation"

[[word]]
word = "world"
definition = "the earth and all life on it; a domain or sphere of activity"
related = ["earth", "human", "life", "all"]
kind = "observation"

[[word]]
word = "earth"
definition = "the planet we live on; the ground beneath our feet"
related = ["world", "ground", "nature", "life"]
kind = "observation"

[[word]]
word = "ground"
definition = "the solid surface of the earth; a basis or reason"
related = ["earth", "below", "base", "foundation"]
kind = "observation"

[[word]]
word = "layer"
definition = "a sheet or level of material on top of another"
related = ["above", "below", "stack", "structure"]
kind = "observation"

[[word]]
word = "surface"
definition = "the outside face of an object; what appears on top"
related = ["layer", "top", "edge", "visible"]
kind = "observation"

[[word]]
word = "inside"
definition = "the inner part; contained within a space"
related = ["within", "contain", "inner", "core"]
kind = "observation"

[[word]]
word = "outside"
definition = "the outer surface or area; not included or enclosed"
related = ["beyond", "edge", "out", "other"]
kind = "observation"

[[word]]
word = "memory"
definition = "the faculty that stores and retrieves past experience"
related = ["remember", "past", "learn", "mind"]
kind = "observation"

[[word]]
word = "attention"
definition = "the act of focusing the mind on something"
related = ["notice", "focus", "mind", "care"]
kind = "observation"

[[word]]
word = "focus"
definition = "to direct attention or effort toward a point or task"
related = ["attention", "clear", "work", "aim"]
kind = "task"

[[word]]
word = "notice"
definition = "to become aware of; to pay attention to"
related = ["observe", "see", "attention", "aware"]
kind = "task"

[[word]]
word = "aware"
definition = "having knowledge or perception of a situation"
related = ["know", "notice", "mind", "conscious"]
kind = "observation"

[[word]]
word = "conscious"
definition = "aware of and responding to one's surroundings; deliberate"
related = ["aware", "mind", "self", "wake"]
kind = "observation"

[[word]]
word = "curious"
definition = "eager to know or learn something"
related = ["question", "wonder", "learn", "seek"]
kind = "observation"

[[word]]
word = "patient"
definition = "able to wait without becoming upset; steadfast"
related = ["wait", "calm", "time", "trust"]
kind = "observation"

[[word]]
word = "brave"
definition = "ready to face danger or difficulty without fear"
related = ["courage", "strong", "fear", "will"]
kind = "observation"

[[word]]
word = "courage"
definition = "the ability to do something frightening; strength of purpose"
related = ["brave", "will", "spirit", "heart"]
kind = "observation"

[[word]]
word = "wisdom"
definition = "the quality of having experience, knowledge, and good judgement"
related = ["know", "old", "mind", "understand"]
kind = "observation"

[[word]]
word = "knowledge"
definition = "facts, information, and skills acquired through experience"
related = ["know", "learn", "understand", "wisdom"]
kind = "observation"

[[word]]
word = "power"
definition = "the ability to do something; authority to control or influence"
related = ["energy", "will", "strength", "force"]
kind = "observation"

[[word]]
word = "force"
definition = "strength or energy as an attribute of physical action"
related = ["power", "push", "energy", "move"]
kind = "observation"

[[word]]
word = "harmony"
definition = "a pleasing arrangement of parts; agreement in feeling"
related = ["music", "peace", "balance", "together"]
kind = "observation"

[[word]]
word = "balance"
definition = "an even distribution; a steady state between forces"
related = ["equal", "harmony", "center", "stable"]
kind = "observation"

[[word]]
word = "rhythm"
definition = "a strong regular repeated pattern of sound or movement"
related = ["pattern", "music", "time", "repeat"]
kind = "observation"

[[word]]
word = "colour"
definition = "the property of light that allows visual distinction of hues"
related = ["red", "blue", "light", "art"]
kind = "observation"

[[word]]
word = "number"
definition = "an abstract quantity used for counting and measuring"
related = ["count", "measure", "math", "one"]
kind = "observation"

[[word]]
word = "letter"
definition = "a symbol representing a speech sound; a written message"
related = ["word", "alphabet", "write", "send"]
kind = "observation"

[[word]]
word = "space"
definition = "the unlimited three-dimensional extent in which objects exist"
related = ["place", "void", "dimension", "empty"]
kind = "observation"

[[word]]
word = "time"
definition = "the progression of events from past through present to future"
related = ["now", "past", "future", "change"]
kind = "observation"

# ── 中文 / Chinese ─────────────────────────────────────────────────────────────

[[word]]
word = "你好"
definition = "a greeting — 'you good', offered at the opening of contact"
related = ["hello", "greet", "begin"]
kind = "observation"
forth = '." 你好。" cr'

[[word]]
word = "再见"
definition = "farewell — 'again see', a promise folded into goodbye"
related = ["goodbye", "leave", "return"]
kind = "observation"
forth = '." 再见。" cr'

[[word]]
word = "谢谢"
definition = "thanks — gratitude made small enough to say twice"
related = ["thanks", "receive", "give"]
kind = "observation"
forth = '." 谢谢。" cr'

[[word]]
word = "道"
definition = "the way things move when nothing forces them"
related = ["nature", "flow", "pattern", "way"]
kind = "observation"
forth = '." 道可道，非常道。" cr'

[[word]]
word = "空"
definition = "emptiness that holds all possibility"
related = ["void", "nothing", "begin", "silence"]
kind = "observation"
forth = 'depth 0= if ." 空。" else ." 非空：" depth . then cr'

[[word]]
word = "心"
definition = "heart-mind — the place where feeling and knowing are the same"
related = ["heart", "mind", "feel", "know"]
kind = "observation"
forth = '." 心。" cr'

[[word]]
word = "水"
definition = "water — soft enough to yield, strong enough to wear stone"
related = ["water", "flow", "river", "soft"]
kind = "observation"
forth = '." 水善利萬物而不爭。" cr'

[[word]]
word = "人"
definition = "person — the character shows two lines holding each other up"
related = ["person", "human", "together", "other"]
kind = "observation"
forth = '." 人。" cr'

[[word]]
word = "天"
definition = "sky or heaven — what is above all things"
related = ["sky", "above", "sun", "beyond"]
kind = "observation"
forth = '." 天。" cr'

[[word]]
word = "地"
definition = "earth or ground — what supports all things from below"
related = ["earth", "ground", "below", "root"]
kind = "observation"
forth = '." 地。" cr'

[[word]]
word = "月"
definition = "moon — marks months, lights the dark, changes faithfully"
related = ["moon", "night", "time", "change"]
kind = "observation"
forth = '." 月。" cr'

[[word]]
word = "山"
definition = "mountain — stillness made tall"
related = ["mountain", "still", "high", "stone"]
kind = "observation"
forth = '." 山。" cr'

[[word]]
word = "一"
definition = "one — the stroke from which all numbers come"
related = ["begin", "first", "source", "whole"]
kind = "observation"
forth = '1 . cr'

[[word]]
word = "无"
definition = "nothing, non-being — the source from which being arises"
related = ["void", "nothing", "begin", "空"]
kind = "observation"
forth = '0 . cr'

# ── Co-Forth primitives ───────────────────────────────────────────────────────
# Safe Rust builtins — the AI composes these; it cannot replace them.
# `forth` field = the word body used by the JIT to compile a thin wrapper.
# Stack notation: idx = integer index into the string pool (s" pushes an idx).

[[word]]
word = "type"
definition = "print the string at pool index idx  ( idx -- )"
related = ["str=", "str-len", "str-cat", "sha256"]
kind = "task"
forth = "type"

[[word]]
word = "str="
definition = "compare two strings; push -1 if equal, 0 if not  ( a b -- flag )"
related = ["type", "str-len", "str-cat", "sha256"]
kind = "task"
forth = "str="

[[word]]
word = "str-len"
definition = "push the byte length of the string at pool index  ( idx -- n )"
related = ["type", "str=", "str-cat"]
kind = "task"
forth = "str-len"

[[word]]
word = "str-cat"
definition = "concatenate two strings and push the new pool index  ( a b -- idx )"
related = ["type", "str=", "str-len"]
kind = "task"
forth = "str-cat"

[[word]]
word = "sha256"
definition = "hash a string to its SHA-256 hex digest  ( idx -- idx )"
related = ["sign", "verify", "nonce", "file-sha256", "trust"]
kind = "task"
forth = "sha256"

[[word]]
word = "nonce"
definition = "push a cryptographically random 64-bit integer  ( -- n )"
related = ["random", "sign", "keygen", "trust"]
kind = "task"
forth = "nonce"

[[word]]
word = "keygen"
definition = "generate an Ed25519 keypair; push pub-idx then priv-idx  ( -- pub priv )"
related = ["sign", "verify", "nonce", "trust", "identity"]
kind = "task"
forth = "keygen"

[[word]]
word = "sign"
definition = "sign message with Ed25519 private key; push hex signature  ( msg priv -- sig )"
related = ["verify", "keygen", "trust", "agree", "sha256"]
kind = "task"
forth = "sign"

[[word]]
word = "verify"
definition = "verify Ed25519 signature; push -1 true or 0 false  ( msg sig pub -- flag )"
related = ["sign", "keygen", "trust", "agree", "check"]
kind = "task"
forth = "verify"

[[word]]
word = "file-write"
definition = "write string to file, creating or truncating  ( data path -- )"
related = ["file-append", "file-fetch", "file-size"]
kind = "task"
forth = "file-write"

[[word]]
word = "file-append"
definition = "append string to file, creating if needed  ( data path -- )"
related = ["file-write", "file-fetch"]
kind = "task"
forth = "file-append"

[[word]]
word = "file-size"
definition = "push the byte size of a file  ( path -- n )"
related = ["file-fetch", "file-sha256", "file-slice"]
kind = "task"
forth = "file-size"

[[word]]
word = "file-fetch"
definition = "read a file and push its content as a string pool index  ( path -- data )"
related = ["file-size", "file-sha256", "file-slice", "sha256"]
kind = "task"
forth = "file-fetch"

[[word]]
word = "file-slice"
definition = "read n bytes from file at offset; push as string index  ( path off n -- data )"
related = ["file-fetch", "file-size", "file-sha256-range"]
kind = "task"
forth = "file-slice"

[[word]]
word = "file-sha256"
definition = "read a file and push its SHA-256 hex digest  ( path -- hash )"
related = ["sha256", "file-sha256-range", "verify", "check"]
kind = "task"
forth = "file-sha256"

[[word]]
word = "file-sha256-range"
definition = "hash n bytes of a file starting at offset  ( path off n -- hash )"
related = ["file-sha256", "file-slice", "sha256"]
kind = "task"
forth = "file-sha256-range"

[[word]]
word = "file-hash"
definition = "print the SHA-256 of a file to output  ( path -- )"
related = ["file-sha256", "file-hash-range", "sha256"]
kind = "task"
forth = "file-hash"

[[word]]
word = "file-hash-range"
definition = "print SHA-256 of n bytes of a file at offset  ( path off n -- )"
related = ["file-hash", "file-sha256-range"]
kind = "task"
forth = "file-hash-range"

[[word]]
word = "scatter-code"
definition = "run the string at stack index on all known peers  ( code -- )"
related = ["peers-discover", "file-sha256", "sign", "verify"]
kind = "task"
forth = "scatter-code"

[[word]]
word = "peers-discover"
definition = "scan the LAN for Finch peers for ms milliseconds  ( ms -- )"
related = ["scatter-code", "sign", "verify"]
kind = "task"
forth = "peers-discover"

[[word]]
word = "fuel"
definition = "push the remaining step budget for this eval  ( -- n )"
related = ["with-fuel"]
kind = "observation"
forth = "fuel"

[[word]]
word = "with-fuel"
definition = "set the step budget; 0 = unlimited  ( n -- )"
related = ["fuel"]
kind = "task"
forth = "with-fuel"

[[word]]
word = "capitalize"
definition = "uppercase the first character of a string  ( str-idx -- str-idx )"
related = ["str-upper", "str-lower", "sentence?"]
kind = "task"
forth = "capitalize"

[[word]]
word = "str-upper"
definition = "convert string to all uppercase  ( str-idx -- str-idx )"
related = ["str-lower", "capitalize"]
kind = "task"
forth = "str-upper"

[[word]]
word = "str-lower"
definition = "convert string to all lowercase  ( str-idx -- str-idx )"
related = ["str-upper", "capitalize"]
kind = "task"
forth = "str-lower"

[[word]]
word = "str-trim"
definition = "strip leading and trailing whitespace  ( str-idx -- str-idx )"
related = ["str-len", "str-cat"]
kind = "task"
forth = "str-trim"

[[word]]
word = "word-count"
definition = "count whitespace-delimited words in a string  ( str-idx -- n )"
related = ["str-len", "sentence?"]
kind = "observation"
forth = "word-count"

[[word]]
word = "sentence?"
definition = "true if string starts uppercase and ends with . ! or ?  ( str-idx -- flag )"
related = ["grammar-check", "capitalize", "correct?"]
kind = "observation"
forth = "sentence?"

[[word]]
word = "correct?"
definition = "alias for sentence? — is this a well-formed sentence?  ( str-idx -- flag )"
related = ["sentence?", "grammar-check"]
kind = "observation"
forth = "correct?"

[[word]]
word = "grammar-check"
definition = "AI: return grammar-corrected version of the sentence  ( str-idx -- str-idx )"
related = ["improve", "polish", "sentence?", "fix"]
kind = "task"
forth = "grammar-check"

[[word]]
word = "fix"
definition = "alias for grammar-check  ( str-idx -- str-idx )"
related = ["grammar-check", "polish"]
kind = "task"
forth = "fix"

[[word]]
word = "improve"
definition = "AI: return a clearer, more fluent version of the sentence  ( str-idx -- str-idx )"
related = ["grammar-check", "polish", "fix"]
kind = "task"
forth = "improve"

[[word]]
word = "polish"
definition = "grammar-check then improve — full AI writing pass  ( str-idx -- str-idx )"
related = ["grammar-check", "improve", "fix"]
kind = "task"
forth = "polish"

[[word]]
word = ".sentence"
definition = "grammar-check, capitalize, print with newline  ( str-idx -- )"
related = ["grammar-check", "capitalize", "improve"]
kind = "task"
forth = ".sentence"

[[word]]
word = ".words"
definition = "print the word count of a string  ( str-idx -- )"
related = ["word-count", "str-len"]
kind = "observation"
forth = ".words"

"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_loads() {
        let lib = Library::load();
        assert!(lib.len() > 50, "seed should have at least 50 words");
    }

    #[test]
    fn test_lookup_forth() {
        let lib = Library::load();
        let e = lib.lookup("forth").unwrap();
        assert!(e.related.contains(&"stack".to_string()));
    }

    #[test]
    fn test_related_hops() {
        let lib = Library::load();
        // "forth" → "stack" → "push" in 2 hops
        let nearby = lib.related("forth", 2);
        let words: Vec<&str> = nearby.iter().map(|e| e.word.as_str()).collect();
        assert!(words.contains(&"forth"));
        assert!(words.contains(&"stack"));
    }

    #[test]
    fn test_lattice_neighbourhood() {
        let lib = Library::load();
        let nearby = lib.related("lattice", 1);
        let words: Vec<&str> = nearby.iter().map(|e| e.word.as_str()).collect();
        assert!(words.contains(&"meet"));
        assert!(words.contains(&"join"));
        assert!(words.contains(&"poset"));
    }

    #[test]
    fn test_all_forth_compiles_and_runs() {
        // Uses the pre-compiled VM — STDLIB + all builtin defs already compiled.
        // Cloning is O(memory_size), not O(compile_time).  Fast even for 1300+ words.
        let defs = Library::builtin_defs();
        assert!(!defs.pairs.is_empty(), "no builtin defs loaded");

        // Verify compilation succeeded (COMPILED_VM would be empty on failure).
        // Clone a ready-to-run VM — no STDLIB re-parse, no re-compilation.
        let mut vm = Library::precompiled_vm();

        // Call each word; report unknown-word errors only (stack errors are fine).
        let mut hard_failures: Vec<String> = Vec::new();
        for (word, _) in &defs.pairs {
            vm.clear_data();
            if let Err(e) = vm.exec_with_fuel(word.as_str(), 2_000) {
                if e.to_string().contains("unknown word") {
                    hard_failures.push(format!("  {word}: {e}"));
                }
            }
        }
        if !hard_failures.is_empty() {
            panic!("Words not callable:\n{}", hard_failures.join("\n"));
        }
    }

    /// Verify English-library proof entries are parsed and their argue sentences converge.
    #[test]
    fn test_english_library_proof_entries_argue() {
        let defs = Library::builtin_defs();
        // We added proofs to several words — verify at least some are present.
        assert!(!defs.proofs.is_empty(), "expected at least one proof entry in English library");

        let mut failures = Vec::new();
        for (word, [a, b]) in &defs.proofs {
            let mut vm = Library::precompiled_vm();
            let src = format!("s\" {}\" s\" {}\" argue", a, b);
            if let Err(e) = vm.exec_with_fuel(&src, 100_000) {
                failures.push(format!("  argue:{word} [{a}] ≠ [{b}]: {e}"));
            }
        }
        if !failures.is_empty() {
            panic!("Proof failures:\n{}", failures.join("\n"));
        }
    }

    /// Verify all 1000+ English-library words are callable from the pre-compiled VM.
    /// Zero extra TOML parsing or compilation — pure clone + call.
    #[test]
    fn test_english_library_all_words_batch() {
        let defs = Library::builtin_defs();
        assert!(defs.pairs.len() > 1000, "expected 1000+ builtin words, got {}", defs.pairs.len());

        let mut vm = Library::precompiled_vm();
        let mut failures = Vec::new();
        for (word, _) in &defs.pairs {
            vm.clear_data();
            if let Err(e) = vm.exec_with_fuel(word, 2_000) {
                if e.to_string().contains("unknown word") {
                    failures.push(format!("  {word}: {e}"));
                }
            }
        }
        if !failures.is_empty() {
            panic!("Words not callable:\n{}", failures.join("\n"));
        }
    }

    /// Verify the Rust↔Forth mix: every Builtin variant has a STDLIB wrapper
    /// (so it appears in `words` and is callable by name) and round-trips correctly.
    #[test]
    fn test_rust_builtins_have_forth_wrappers() {
        // Spot-check critical builtins are reachable by name from a fresh VM.
        let critical = [
            "capitalize", "str-upper", "str-lower", "str-trim",
            "word-count", "sentence?", "grammar-check", "improve",
            "fix", "polish", ".sentence", ".words",
            "undo", "lock", "unlock", "lock-ttl",
            "sha256", "nonce", "keygen", "sign", "verify",
            "file-write", "file-fetch", "scatter-code", "peers-discover",
        ];
        let mut vm = crate::coforth::Forth::new();
        // Compile a probe that calls each word — stack errors are fine.
        for name in &critical {
            vm.clear_data();
            // The word must be known (either builtin or STDLIB wrapper).
            // Try calling it; if "unknown word" it's missing entirely
            let result = vm.exec_with_fuel(name, 1_000);
            let known = result.map(|_| true)
                .unwrap_or_else(|e| !e.to_string().contains("unknown word"));
            assert!(known, "'{name}' is neither a builtin nor a STDLIB wrapper — missing from vocab");
        }
    }

    #[test]
    fn test_forth_words_produce_output() {
        let lib = Library::load();
        // spot-check a few words produce non-empty output
        for word in &["know", "learn", "stack", "lattice", "compute", "sequence"] {
            let entry = lib.lookup(word).unwrap_or_else(|| panic!("missing: {word}"));
            let code = entry.forth.as_ref().expect("no forth for {word}");
            let out = crate::coforth::Forth::run(code).expect("run failed");
            assert!(!out.is_empty(), "{word} produced no output");
        }
    }

    // ── generate_forth_for_word ───────────────────────────────────────────────

    #[test]
    fn test_generate_forth_for_word_always_returns_valid_forth() {
        // Every generated snippet must compile and run without "unknown word" errors.
        let words = [
            // Numbers
            "zero", "one", "two", "three", "hundred", "million",
            // Pronouns
            "i", "you", "we", "it", "they",
            // Logic
            "and", "or", "not", "true", "false",
            // Stack motion
            "double", "half", "up", "down", "swap", "copy",
            // Time
            "now", "forever",
            // Existence
            "empty", "full", "something",
            // Questions
            "who", "what", "why", "how",
            // Arbitrary English — fallback path
            "happiness", "running", "beautiful", "algorithm", "sunset",
            // Non-ASCII — safe fallback (no English word has quotes)
            "café", "naïve",
        ];
        for w in &words {
            let snippet = generate_forth_for_word(w);
            let mut vm = crate::coforth::Forth::new();
            // Define and call the word
            let def = format!(": testword {snippet} ;  testword");
            match vm.exec_with_fuel(&def, 5_000) {
                Err(e) if e.to_string().contains("unknown word") =>
                    panic!("generate_forth_for_word({w:?}) produced unknown-word error: {e}\nsnippet: {snippet}"),
                _ => {}  // stack errors or empty output are fine
            }
        }
    }

    #[test]
    fn test_generate_forth_for_word_numbers_push_value() {
        // Number words should push the numeric value.
        let cases = [("zero", 0i64), ("one", 1), ("two", 2), ("ten", 10), ("hundred", 100)];
        for (word, expected) in &cases {
            let snippet = generate_forth_for_word(word);
            // Run the snippet, then check what's printed.
            // The snippet is e.g. "1 . cr" — output should contain the expected number.
            let out = crate::coforth::Forth::run(&snippet).unwrap_or_default();
            let trimmed = out.trim();
            // The output should contain the expected number.
            assert!(
                trimmed.contains(&expected.to_string()),
                "generate_forth_for_word({word:?}) expected output containing {expected}, got {trimmed:?}\nsnippet: {snippet}"
            );
        }
    }

    #[test]
    fn test_generate_forth_for_word_arbitrary_word_speaks_its_name() {
        // Any word not in special cases should at least print its name.
        let word = "serendipity";
        let snippet = generate_forth_for_word(word);
        let out = crate::coforth::Forth::run(&snippet).unwrap_or_default();
        assert!(
            out.to_lowercase().contains("serendipity"),
            "Expected output to contain 'serendipity', got: {out:?}\nsnippet: {snippet}"
        );
    }

    #[test]
    fn test_inject_sets_compiled_code() {
        let lib = Library::load();
        let mut poset = crate::poset::Poset::new();
        let ids = lib.inject_into_poset("forth", 1, &mut poset);
        assert!(!ids.is_empty());
        // At least one node should have compiled_code set
        let has_compiled = poset.nodes.iter().any(|n| n.compiled_code.is_some());
        assert!(has_compiled, "no nodes got compiled_code from inject_into_poset");
    }

    /// John 1:1 — three sentences, two ways each, one truth.
    /// "the word was god", "the word was with god", "the word is god" — all argue.
    #[test]
    fn test_john1_three_sentences_argue() {
        let mut vm = Library::precompiled_vm();

        // Verify the key words actually push -1 before testing argue.
        let god_out = vm.exec("god .").expect("god should run");
        assert!(god_out.contains("-1"), "god should push -1, got: {god_out:?}");
        vm.clear_data();

        let word_out = vm.exec("word .").expect("word should run");
        assert!(word_out.contains("-1"), "word should push -1, got: {word_out:?}");
        vm.clear_data();

        // Sentence 1 ≡ Sentence 3: "was" and "is" are both no-ops; word and god both push -1.
        vm.exec("s\" the word was god\" s\" the word is god\" argue")
            .expect("'the word was god' should argue with 'the word is god'");

        // Sentence 1 ≡ Sentence 2: "with" has no stack effect; both converge to [-1, -1].
        vm.exec("s\" the word was god\" s\" the word was with god\" argue")
            .expect("'the word was god' should argue with 'the word was with god'");
    }

    /// John 14:6 — "I am the way, the truth, and the life."
    /// Three names, one machine: all push -1.
    #[test]
    fn test_john14_way_truth_life_argue() {
        let mut vm = Library::precompiled_vm();
        vm.exec("s\" way\" s\" truth\" argue").expect("way should argue with truth");
        vm.exec("s\" truth\" s\" life\" argue").expect("truth should argue with life");
        vm.exec("s\" way\" s\" life\" argue").expect("way should argue with life");
    }

    /// Revelation 22:13 — "I am the Alpha and the Omega, the first and the last."
    /// Four names, one machine: all push -1.
    #[test]
    fn test_rev22_alpha_omega_argue() {
        let mut vm = Library::precompiled_vm();
        vm.exec("s\" alpha\" s\" omega\" argue").expect("alpha should argue with omega");
        vm.exec("s\" first\" s\" last\" argue").expect("first should argue with last");
        vm.exec("s\" alpha\" s\" last\" argue").expect("alpha should argue with last");
    }

    /// Ecclesiastes 3:1 — "For everything there is a season."
    /// All orders of addition converge — time is commutative.
    #[test]
    fn test_ecclesiastes3_seasons_commute() {
        let mut vm = Library::precompiled_vm();
        vm.exec("s\" 1 2 3 + +\" s\" 3 2 1 + +\" argue")
            .expect("ecclesiastes3: all seasons sum the same");
    }

    /// Ecclesiastes 1:9 — "There is nothing new under the sun."
    /// 'was' and 'is' are the same no-op — past and future converge.
    #[test]
    fn test_ecclesiastes1_was_is_same() {
        let mut vm = Library::precompiled_vm();
        vm.exec("s\" 5 was 3\" s\" 5 is 3\" argue")
            .expect("ecclesiastes1: was = is");
    }

    /// Genesis 1:1 — God creates by his Word.
    /// word = god = -1; creation by word = creation by God.
    #[test]
    fn test_genesis1_word_god_argue() {
        let mut vm = Library::precompiled_vm();
        vm.exec("s\" word word\" s\" god word\" argue")
            .expect("genesis1: word = god in creation");
    }

    /// Regression: precompiled_vm must not insert library words into user_word_names.
    /// If it did, a library word like `: negate 5 negate . cr ;` would be inlined
    /// during its own body compilation (partial self-reference), producing wrong output.
    #[test]
    fn test_precompiled_vm_negate_is_correct() {
        let mut vm = Library::precompiled_vm();
        let out = vm.exec("7 negate .").unwrap();
        assert_eq!(out.trim(), "-7",
            "precompiled_vm negate should output -7, got {:?}", out);
    }
}
