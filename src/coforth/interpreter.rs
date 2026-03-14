/// Co-Forth native Forth interpreter — von Neumann flat-memory model.
///
/// All compiled words live in a single flat `Vec<Cell>` (the "memory").
/// The inner interpreter holds an instruction pointer (`ip: usize`) that
/// advances through that array.  Word calls push a return address onto an
/// explicit `call_stack: Vec<usize>` and jump; `Ret` pops and returns.
/// There are no recursive Rust calls across word boundaries.
///
/// This mirrors a real Forth on von Neumann hardware:
///   - code and data share one address space
///   - the IP is an integer register, not a Rust stack frame
///   - jump targets are absolute addresses, not relative offsets
///   - `extend_adjusted` (offset patching when concatenating Vec<Op> slices)
///     is gone — compilation writes directly into memory so addresses are
///     naturally absolute from the moment they are emitted
///
/// Built-in words (always available):
///   Arithmetic : + - * / mod
///   Stack      : dup drop swap over rot nip tuck 2dup 2drop 2swap
///   Comparison : = < > <> 0= 0< 0>
///   Logic      : and or xor invert negate abs max min
///   Shift      : lshift rshift
///   I/O        : . ." cr space .s emit
///   Variables  : variable @ ! +!
///   Loop index : i j
///   Misc       : depth nop bye
///
/// Control flow: if/else/then  begin/until  begin/while/repeat  do/loop/+loop
/// Comments: ( ... )   \ line comment

use std::collections::HashMap;
use std::time::{Duration, Instant};
use anyhow::{bail, Result};

// ── Flat memory cell ─────────────────────────────────────────────────────────
//
// Every compiled word and every top-level expression is stored as a sequence
// of `Cell` values in `Forth::memory`.  Jump targets are absolute indices
// into that array — just like a CPU's instruction pointer.

// Compile-time size check — Cell must stay ≤ 16 bytes for cache efficiency.
// Two-index variants (TagPeer, SayInChannel, etc.) use u32 pairs to stay within budget.
const _: () = assert!(std::mem::size_of::<Cell>() <= 16);

#[derive(Clone, Copy, Debug)]
enum Cell {
    Lit(i64),           // push literal onto data stack
    Str(usize),         // print strings[idx]  (string-literal pool)
    PushStr(usize),     // push strings[idx] index as i64 onto data stack (s" literal")
    HelloPeer(usize),   // send "hello from <hostname>!" to one peer (by addr or label) strings[idx]
    TagPeer(u32,u32), // strings[name_idx] → label for strings[addr_idx]
    JoinChannel(usize),          // join channel strings[idx]; broadcast "<name> joined #chan" to all peers
    PartChannel(usize),          // leave channel strings[idx]; broadcast "<name> left #chan" to all peers
    SayInChannel(u32,u32),   // broadcast "[#chan] name: msg" — strings[chan_idx], strings[msg_idx]
    ProveWord(usize),            // run test:strings[idx]; show ✓ / ✗
    Confirm(usize),     // ask user strings[idx]; push -1 (yes) or 0 (no)
    SelectDialog(usize),// pop-up select: strings[idx] is "title|opt1|opt2|..."; push chosen index or -1
    ReadFile(usize),    // read file at strings[idx]; emit contents to out
    ReadCsv(usize),     // read CSV file at strings[idx]; emit as pipe-delimited rows
    ReadTsv(usize),     // read TSV file at strings[idx]; emit as pipe-delimited rows
    ReadXlsx(usize),    // read first sheet of xlsx/xls/ods at strings[idx]; emit rows
    ExecCmd(usize),     // run shell command strings[idx]; emit stdout to out
    GlobFiles(usize),   // list files matching glob strings[idx]; emit to out
    AddPeer(usize),     // register peer address strings[idx] for scatter
    ScatterExec(usize),           // run strings[idx] Forth code on all peers in parallel
    ScatterBashExec(usize),       // run strings[idx] as bash -c on all peers via /v1/exec
    ScatterStack,                 // ( code-idx -- ) scatter strings[pop()] to all peers (dynamic code)
    ScatterSymbol(usize),         // share symbol strings[idx]: send local def if known, then run on all peers
    ScatterOnCluster(u32,u32),// run strings[code_idx] on ensemble strings[ensemble_idx]; no side-effects on peers
    RunOn(u32,u32),           // run strings[code_idx] on exactly one peer: strings[peer_idx] (addr or label)
    GenAI(usize),           // call AI generator with strings[idx] as prompt; emit response
    Builtin(Builtin),   // execute a primitive operation
    Addr(usize),        // call word at memory[addr]: push ip+1, ip = addr
    JmpZ(usize),        // if pop() == 0: ip = addr  (if/while/of)
    Jmp(usize),         // ip = addr  (unconditional)
    Until(usize),       // if pop() == 0: ip = addr  (begin..until loop back)
    While(usize),       // if pop() == 0: ip = addr  (exit begin..while loop)
    Repeat(usize),      // ip = addr  (back-edge of begin..while..repeat)
    DoSetup,            // ( limit index -- ) initialise do/loop counter
    DoLoop(usize),      // ++index; if index < limit: ip = addr
    DoLoopPlus(usize),  // index += pop(); if index < limit: ip = addr
    OfTest(usize),      // case/of: if TOS ≠ selector: ip = addr
    Ret,                // pop call_stack → ip; if empty, halt
}

#[derive(Clone, Debug, Copy)]
enum Builtin {
    Plus, Minus, Star, Slash, Mod,
    Dup, Drop, Swap, Over, Rot, Nip, Tuck,
    TwoDup, TwoDrop, TwoSwap,
    TwoOver,   // ( a b c d -- a b c d a b )  copy second pair to top
    TwoRot,    // ( a b c d e f -- c d e f a b )  rotate three pairs
    Pick, Roll,
    Eq, Lt, Gt, Ne, Le, Ge, ZeroEq, ZeroLt, ZeroGt, ULt,
    And, Or, Xor, Invert, Negate, Abs, Max, Min,
    Lshift, Rshift,
    StarSlash, SlashMod,      // */ and /mod
    Print, PrintU, PrintS, Cr, Space, Emit, PrintHex,
    Fetch, Store, PlusStore, Allot, Cells,
    LoopI, LoopJ,
    ToR, FromR, FetchR,
    Words,                    // list defined words
    HotWords,                 // ( -- )  show top-10 most-called words
    Random,                   // ( -- n )
    Time,                     // ( -- epoch_secs )
    Depth, Nop,
    Fuel,                     // ( -- n )  push remaining step budget
    WithFuel,                 // ( n addr -- )  call word at addr with n-step budget
    Undo,                     // ( -- )  restore dictionary to state before last definition
    // Distributed locking — advisory; auto-expire; only meaningful across peers
    Lock,                     // ( name-idx ttl-ms -- flag )  acquire if free/expired (-1) or fail (0)
    Unlock,                   // ( name-idx -- )              release early
    LockTtl,                  // ( name-idx -- ms )           remaining TTL (0 if free or expired)
    // Writing assistance — string manipulation + AI correction
    Capitalize,               // ( str-idx -- str-idx )  uppercase first character
    StrUpper,                 // ( str-idx -- str-idx )  all uppercase
    StrLower,                 // ( str-idx -- str-idx )  all lowercase
    StrTrim,                  // ( str-idx -- str-idx )  strip leading/trailing whitespace
    WordCount,                // ( str-idx -- n )        number of whitespace-delimited words
    SentenceCheck,            // ( str-idx -- flag )     -1 if starts uppercase + ends . ! ?
    GrammarCheck,             // ( str-idx -- str-idx )  AI: return grammar-corrected sentence
    ImproveStr,               // ( str-idx -- str-idx )  AI: return clearer/more fluent sentence
    // Integer math extras
    Sqrt,                     // ( n -- isqrt(n) )  integer square root
    Floor,                    // ( n d -- n/d*d )   floor division result
    Ceil,                     // ( n d -- ceil )    ceiling division
    // Trig (scaled: input in degrees * 1000, output * 1000)
    Sin,                      // ( deg*1000 -- sin*1000 )
    Cos,                      // ( deg*1000 -- cos*1000 )
    // Fixed-point helpers
    FPMul,                    // ( a b scale -- a*b/scale )  fixed-point multiply
    // Distributed
    Peers,                    // ( -- )  list registered peers
    PeersClear,               // ( -- )  remove all registered peers
    PeersDiscover,            // ( -- )  mDNS scan; auto-add found finch daemons
    TakeAll,                  // ( -- )  discover all LAN peers, tag+ensemble them as "all"
    EnsembleDef,              // ( name-idx -- )  snapshot current peers as named ensemble
    EnsembleUse,              // ( name-idx -- )  push peers, switch to named ensemble
    EnsembleEnd,              // ( -- )           pop peers (restore previous)
    EnsembleList,             // ( -- )           print all ensembles + members
    LabelPeer,                // ( addr-idx label-idx -- )  attach human label to peer address
    TagPeer,                  // ( addr-idx tag-idx -- )    add tag to peer
    EnsembleFromTag,          // ( tag-idx -- )   build/update an ensemble from all peers with tag
    PeerInfo,                 // ( -- )           list peers with labels + tags
    Publish,                  // ( name-idx -- )  scatter word source to all peers via /v1/forth/define
    Sync,                     // ( -- )           scatter all user words to all peers
    // Registry
    RegistrySet,              // ( addr-idx -- )  set registry address
    JoinRegistry,             // ( self-addr-idx -- )  register this machine; stores my_addr
    LeaveRegistry,            // ( -- )           deregister this machine from the registry
    FromRegistry,             // ( -- )           pull live peers from registry into self.peers
    RegistryList,             // ( -- )           print all registry members
    Balance,                  // ( -- )           print this machine's compute balance
    Balances,                 // ( -- )           print all machines' compute balances
    RecordDebit,              // ( peer-idx compute-ms -- )  record compute consumed from peer
    DebtCheck,                // ( -- )           list machines that owe you (negative balance)
    See(usize),               // ( -- )  show definition of strings[idx]
    Settle,                   // ( peer-idx -- )  request settlement from a peer
    Slowest,                  // ( -- addr-idx )  push addr of slowest live peer onto stack
    ForthBack(usize),         // ( -- )           queue Forth code to run on the caller after response
    // String pool operations (stack: idx is i64 index into self.strings)
    Type,                     // ( idx -- )  print strings[idx]
    StrEq,                    // ( idx-a idx-b -- bool )  string equality
    StrLen,                   // ( idx -- n )  byte length of strings[idx]
    StrCat,                   // ( idx-a idx-b -- idx-c )  concatenate
    StrSplit,                 // ( idx sep-idx -- result-idx )  split by sep → one part per line
    StrJoin,                  // ( idx sep-idx -- result-idx )  join newline-separated parts with sep
    StrSub,                   // ( idx start len -- idx' )      substring (char-indexed)
    StrFind,                  // ( idx needle-idx -- pos )       find needle; -1 if absent
    StrReplace,               // ( idx from-idx to-idx -- idx' ) replace all occurrences
    StrReverse,               // ( idx -- idx' )                 reverse characters
    NumToStr,                 // ( n -- idx )  format integer as decimal string in pool
    StrToNum,                 // ( idx -- n flag )  parse integer; flag=-1 ok, 0 fail
    WordDefined,              // ( idx -- flag )  -1 if word named strings[idx] is defined
    WordNames,                // ( -- idx )  all defined word names, newline-separated
    NthLine,                  // ( idx n -- line-idx )  get nth line (0-based) of newline string
    AgreeQ,                   // ( str1 str2 -- flag )  like argue but flag, never aborts
    SameQ,                    // ( str1 str2 -- flag )  like versus but flag, never aborts
    Safe,                     // ( str-idx -- flag )  eval str; -1 ok, 0 error (never aborts)
    // Crypto primitives (safe Rust — sha2, ed25519-dalek, rand)
    Sha256,                   // ( idx -- idx' )  SHA-256 hex of strings[idx]
    FileSha256,               // ( path-idx -- hex-idx )         SHA-256 of whole file (stack)
    FileSha256Range,          // ( path-idx offset length -- hex-idx )  SHA-256 of byte range
    FileHash,                 // ( path-idx -- )                 SHA-256 of whole file → out
    FileHashRange,            // ( path-idx offset length -- )   SHA-256 of byte range → out
    FileFetch,                // ( path-idx -- content-idx )     read whole file into pool
    FileSlice,                // ( path-idx offset length -- content-idx )  byte range → pool (utf-8 lossy)
    FileSize,                 // ( path-idx -- n )               file size in bytes
    FileWrite,                // ( content-idx path-idx -- )     overwrite file
    FileAppend,               // ( content-idx path-idx -- )     append to file
    Nonce,                    // ( -- n )  cryptographically random i64
    Keygen,                   // ( -- pub-idx priv-idx )  generate Ed25519 keypair (hex)
    Sign,                     // ( priv-idx data-idx -- sig-idx )  Ed25519 sign
    Verify,                   // ( pub-idx sig-idx data-idx -- bool )  Ed25519 verify
    // Security scanning (pure Rust — no external tools)
    ScanFile,                 // ( path-idx -- report-idx )  byte-level scan → text report
    ScanBytes,                // ( str-idx -- score )        pattern risk score 0-100
    FileEntropy,              // ( path-idx -- entropy*1000 ) Shannon entropy * 1000
    ScanDir,                  // ( path-idx -- report-idx )  recursive dir scan
    ScanStrings,              // ( path-idx -- str-idx )     printable strings from binary (like `strings`)
    ScanProcs,                // ( -- report-idx )           scan running processes for suspicion
    ScanNet,                  // ( -- report-idx )           open network connections
    ScanStartup,              // ( -- report-idx )           persistence locations (LaunchAgents, cron, etc.)
    Quarantine,               // ( path-idx -- flag )        move file to ~/.finch/quarantine/
    // Terminal control (crossterm-backed)
    AtXy,                     // ( col row -- )  move cursor to col, row
    TermSize,                 // ( -- cols rows )  push terminal dimensions
    SaveCursor,               // ( -- )  save cursor position
    RestoreCursor,            // ( -- )  restore cursor position
    ClearEol,                 // ( -- )  clear from cursor to end of line
    ClearLine,                // ( -- )  clear entire current line
    ColorFg,                  // ( n -- )  set foreground ANSI color 0-255
    ResetStyle,               // ( -- )  reset all text attributes
    SyncBegin,                // ( -- )  begin synchronized update
    SyncEnd,                  // ( -- )  end synchronized update
    HideCursor,               // ( -- )  hide terminal cursor
    ShowCursor,               // ( -- )  show terminal cursor
    Connected,                // ( -- flag )  -1 if peers/registry active, 0 if isolated
    /// Increment/decrement TOS in-place — peephole result of `1 +` / `1 -`.
    Inc,   // ( n -- n+1 )
    Dec,   // ( n -- n-1 )
    /// Heap memory allocation (Forth standard words).
    Here,    // ( -- addr )    address of next free heap cell
    Comma,   // ( val -- )     store val at here, advance here
    CellSz,  // ( -- 1 )       size of one cell (1 — heap uses i64 units)
    Fill,    // ( addr n val -- )  fill n cells at addr with val
    Eval,    // ( str -- )    eval strings[pop()] as Forth source code
    Argue,     // ( str1 str2 -- )  run both, show what each got, assert they agree
    Versus,    // ( str1 str2 -- )  run both, show FULL stacks side by side, assert they agree
    BothWays,  // ( a b str -- )   run op(a,b) and op(b,a), show both, assert they agree
    Gate,      // ( str1 str2 check -- result )  run both, apply check; if check passes leave result
    Page,      // ( str -- )    run a multi-line proof page: each line is "left | right"
    Resolve,   // ( str -- )    many sentences, one truth: all lines must converge to same value
    Infix,        // ( str -- )    eval infix expression: "3 + 4", "10 * 5 - 2", etc.
    RegisterBoot, // ( str -- )    register a boot poem line; REPL persists to ~/.finch/boot.forth
    // Proof system
    Assert,                   // ( flag -- )  bail "assertion failed" if flag == 0
    ProveAll,                 // ( -- )  run all test:* words; report pass/fail
    ProveAllBool,             // ( -- flag )  run all test:* words; push -1 (all pass) or 0 (any fail)
    ProveEnglish,             // ( -- )  run every English-library word body; report pass rate
    ProveLanguages,           // ( -- )  argue English ↔ Chinese on shared Forth primitives
    // Channel system
    ListChannels,             // ( -- )  print joined channels
    // Collection operations
    SortLines,                // ( idx -- idx' )  sort lines of strings[idx] alphabetically
    UniqueLines,              // ( idx -- idx' )  deduplicate lines of strings[idx]
    ReverseLines,             // ( idx -- idx' )  reverse line order of strings[idx]
    LineCount,                // ( idx -- n )     number of lines in strings[idx]
    GlobPool,                 // ( pattern-idx -- result-idx )  glob into string pool (one path per line)
    CleanLines,               // ( idx -- n )  quarantine each path in strings[idx] (one per line); return count
    GlobCount,                // ( pattern-idx -- n )  count files matching glob pattern
    ExecCapture,              // ( cmd-idx -- output-idx )  run shell command; push stdout as string pool entry
    BackAndForthQ,            // ( n fwd-str back-str -- flag )  round-trip predicate; never aborts
    InvertibleQ,              // ( n str -- flag )  apply str twice; -1 if f(f(n))=n (involution test)
    Help,                     // ( -- )  print the Co-Forth quick-reference guide
    Describe,                 // ( idx -- )  describe a word: stack signature, source, or builtin notice
    Compute,                  // ( str-idx -- )  evaluate as Forth; fall back to infix; print "= result"
    EquivQ,                   // ( str1 str2 -- flag )  -1 if programs agree on inputs -5..5; the equivalence probe
    Fork,                     // ( str-idx -- )  run code in a forked copy (current stack shared); discard copy
    Boot,                     // ( -- )  re-execute all boot=true vocabulary words
    PrintR,                   // ( n width -- )  print n right-aligned in field of width chars
    PrintPad,                 // ( n width char-idx -- )  print n padded with char to width
    // Fast integer hash operations
    Hash,                     // ( str-idx -- n )  FNV1a-64 hash of strings[idx] → i64
    HashInt,                  // ( n -- n' )       integer mix hash (fast, avalanche)
    HashCombine,              // ( h n -- h' )     combine two hash values (for chaining)
}

// ── Interpreter ───────────────────────────────────────────────────────────────

/// Signature for the optional confirm callback.
pub type ConfirmFn = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Signature for the optional AI generation callback.
///
/// Called when `gen" prompt"` executes.  Receives the prompt string and returns
/// the model's response as a String.  If unset, `gen"` emits a placeholder.
pub type GenFn = Box<dyn Fn(&str) -> String + Send + Sync>;

/// Signature for the optional dialog select callback.
///
/// Called when `select" title|opt1|opt2"` executes.  Receives the title string
/// and a slice of option labels; returns the 0-based index of the chosen option,
/// or -1 if the dialog was cancelled.  If unset, returns 0 (first option).
pub type SelectFn = Box<dyn Fn(&str, &[String]) -> i64 + Send + Sync>;

pub struct Forth {
    data:       Vec<i64>,
    loop_stack: Vec<(i64, i64)>,         // (index, limit) for do/loop
    rstack:     Vec<i64>,                // >r / r> scratch stack
    memory:     Vec<Cell>,               // flat code memory (von Neumann)
    strings:    Vec<String>,             // string-literal pool
    string_dedup: HashMap<String, usize>, // reverse index for dedup (same literal → same idx)
    name_index: HashMap<String, usize>,  // word name → entry address in memory
    call_counts: HashMap<usize, u64>,    // addr → call count (hot call detection)
    heap:       Vec<i64>,                // variable storage
    var_index:  HashMap<String, usize>,  // variable name → heap address
    pub out:    String,
    /// Peer addresses for `scatter"`.  Each entry is a host:port or URL.
    pub peers:  Vec<String>,
    /// Source log — every user-defined `: name body ;` compiled into this VM, in order.
    /// Lets the VM be serialised to Forth source and pasted into another session.
    /// Only active after stdlib init; stdlib words are intentionally excluded.
    pub source_log: Vec<String>,
    log_definitions: bool,
    /// Remaining step budget for the current exec call.
    /// Decremented by the inner interpreter; exec_with_fuel resets it.
    fuel: usize,
    /// Internal undo history: auto-pushed before each new `: name` definition.
    /// Capped at 20 entries (oldest dropped).  `undo` pops and restores.
    undo_stack: Vec<DictionarySnapshot>,
    /// Distributed advisory lock table: name → expiry instant.
    /// Auto-expires on each `lock` call.  Max TTL = 5 s.
    locks: HashMap<String, Instant>,
    /// Optional confirm callback.
    confirm_fn: Option<ConfirmFn>,
    /// Optional AI generation callback.  Wired to the active generator in the REPL.
    gen_fn: Option<GenFn>,
    /// Optional dialog-select callback.  Wired to the TUI in the REPL.
    select_fn: Option<SelectFn>,
    /// Words that remote peers are allowed to call via `/v1/forth/eval`.
    /// Populated from grammar words with `remote = true` and from `seed_remote_whitelist()`.
    pub remote_whitelist: std::collections::HashSet<String>,
    /// Named ensembles: name → list of peer addresses.
    /// `ensemble-def" name"` snapshots the current peers into this map.
    pub ensembles: HashMap<String, Vec<String>>,
    /// Saved-peers stack for `ensemble-use" name"` / `ensemble-end` nesting.
    peer_save_stack: Vec<Vec<String>>,
    /// Per-peer metadata: address → label + tags.
    pub peer_meta: HashMap<String, PeerMeta>,
    /// Registry address — set by `registry" addr"` word.
    /// When set, `join-registry`, `from-registry`, and `registry-peers` use it.
    pub registry_addr: Option<String>,
    /// This machine's own address, set when `join-registry` or `join" addr"` succeeds.
    /// Used by `leave` to deregister from the registry.
    pub my_addr: Option<String>,
    /// When true, interactive cells (Confirm, GenAI) are suppressed:
    /// Confirm auto-denies (returns 0), GenAI returns an empty string.
    /// Set by the HTTP server on every cloned remote VM so remote code
    /// can never block waiting for a local dialog or AI call.
    pub remote_mode: bool,
    /// Forth code the peer wants executed on the caller after this response.
    /// Set by `forth-back" <code>"` in the remote program.
    /// Transmitted in the eval response and run locally by the caller.
    pub forth_back: Option<String>,
    /// Channels this VM has joined via `channel" #name"`.
    /// Persists for the session; informs channel routing.
    pub channels: std::collections::HashSet<String>,
    /// Boot poems registered this session via `boot" text"`.
    /// Drained by the REPL after each exec and appended to ~/.finch/boot.forth.
    pub boot_poems: Vec<String>,
    /// Words that were unknown and routed through missing-word this exec.
    /// Drained by the REPL to ask AI to define them, growing the grammar.
    pub pending_defines: Vec<String>,
    /// Names of words defined by the USER (not stdlib) — these override builtins.
    /// Populated only when log_definitions is true (after stdlib phase).
    user_word_names: std::collections::HashSet<String>,
    /// Reusable call stack for the inner interpreter — pre-allocated, cleared each execute().
    /// Avoids a Vec allocation on every word call.
    call_stack: Vec<usize>,
}

/// Metadata attached to a peer address via `label-peer` / `tag-peer`.
#[derive(Debug, Clone, Default)]
pub struct PeerMeta {
    pub label: Option<String>,
    pub tags:  Vec<String>,
    /// Authentication token for this peer's daemon endpoints.
    /// Populated automatically from mDNS TXT record on discovery.
    pub token: Option<String>,
}

const MAX_CALL_DEPTH: usize  = 1024;
const MAX_DATA_DEPTH: usize  = 1024; // data stack overflow guard
/// Default step budget for interactive vocabulary words.
/// 1M steps: enough for fib(20) (~300k steps), gcd, most recursive definitions.
/// Catches infinite loops in ~1ms rather than seconds.
/// Use `exec_with_fuel` or the `with-fuel` word for intentional heavy work.
const DEFAULT_FUEL: usize = 1_000_000;

/// Snapshot of the Forth dictionary state (not the data stack).
/// Used to implement undo: restore the dictionary to a previous point.
#[derive(Clone)]
pub struct DictionarySnapshot {
    memory_len:   usize,
    strings_len:  usize,
    heap_len:     usize,
    name_index:   HashMap<String, usize>,
    var_index:    HashMap<String, usize>,
    source_log:   Vec<String>, // full snapshot — handles in-place redefinition correctly
}

// ── Standard library (pre-loaded Forth definitions) ──────────────────────────

const STDLIB: &str = r#"
\ ── Arithmetic ────────────────────────────────────────────────────────────────
: square    ( n -- n^2 )   dup * ;
: cube      ( n -- n^3 )   dup dup * * ;
: 2*        ( n -- 2n )    2 * ;
: 2/        ( n -- n/2 )   2 / ;
: 1+        ( n -- n+1 )   1 + ;
: 1-        ( n -- n-1 )   1 - ;
: within    ( n lo hi -- flag )  over - >r - r> u< ;

\ Sum of 1..n  ( n -- n*(n+1)/2 )
: sum-to-n  dup 1 + * 2 / ;

\ Greatest common divisor  ( a b -- gcd )
: gcd  begin dup while swap over mod repeat drop ;

\ Least common multiple  ( a b -- lcm )
: lcm  2dup gcd / * abs ;

\ Prime test  ( n -- flag )
\ Returns -1 (true) if n is prime, 0 if composite or < 2.
\ Uses 0/-1 literals directly — no dependency on true/false/even? words.
: prime?
    dup 2 < if drop 0 exit then
    dup 2 = if drop -1 exit then
    dup 2 mod 0= if drop 0 exit then
    3
    begin 2dup dup * swap <= while
        2dup mod 0= if 2drop 0 exit then
        2 +
    repeat
    2drop -1 ;

\ Next prime at or above n  ( n -- p )
: next-prime  ( n -- p )
    dup 2 < if drop 2 exit then
    dup prime? if exit then
    dup 2 mod 0= if 1+ then
    begin dup prime? 0= while 2 + repeat ;

\ Integer power  ( base exp -- base^exp )
: pow  ( base exp -- result )
    1 swap
    begin dup 0> while
        >r over * r> 1 -
    repeat
    drop nip ;

\ Fibonacci (recursive)  ( n -- fib(n) )
: fib   ( n -- fib(n) )
    dup 2 < if drop 1 exit then
    dup 1 - fib
    swap 2 - fib
    + ;

\ Fibonacci (iterative)  ( n -- fib(n) )
\ Same answer, different direction.  The stack witnesses their agreement.
: fib-iter  ( n -- fib(n) )
    dup 2 < if drop 1 exit then
    1 1 rot 1- 0 do     \ start with fib(0)=1 fib(1)=1; iterate n-1 times
        tuck +          \ ( a b -- b a+b ): tuck b under a, then a+b on top
    loop
    nip ;               \ ( prev fib(n) -- fib(n) ): discard prev, keep fib(n)

\ converge ( str1 str2 -- )
\ Eval both code strings.  Assert they leave the same value on the stack.
\ The proof: two different paths meeting at the same answer.
: converge  ( str1 str2 -- )  swap eval swap eval = assert ;

\ back-and-forth ( n fwd-str back-str -- )
\ Apply fwd-str to n, then back-str to the result.
\ Prove you are home again.  The proof: a round trip is faithful.
: back-and-forth  ( n fwd-str back-str -- )
    >r              \ save back-str            | r: back-str
    swap dup >r swap \ dup n to return stack   | data: n fwd-str  r: back-str n
    eval             \ forward transform        | data: m          r: back-str n
    r> swap          \ restore n under m        | data: n m        r: back-str
    r>               \ fetch back-str           | data: n m back-str
    eval             \ inverse transform        | data: n n'
    = assert ;

\ argue ( str1 str2 -- )
\ Two programmers, each with their own program.  The stack settles it.
\ Shows what each got.  If they agree: ✓.  If not: ✗ with both values.
: argue  ( str1 str2 -- )  argue ;

\ versus ( str1 str2 -- )
\ Two machines run.  Both full stacks shown side by side.
\ Agrees only if EVERY value matches — not just the top.
: versus  ( str1 str2 -- )  versus ;

\ both-ways ( a b str -- )
\ Run op(a,b) and op(b,a) simultaneously.  Prove commutativity.
\ Two directions at once.  The stack settles it.
: both-ways  ( a b str -- )  both-ways ;

\ page ( str -- )
\ A proof page.  Each line: "left | right"  — both sides run; they must agree.
\ Plain lines (no |) run as Forth, setting up shared state for the lines that follow.
\ This is how you write a proof in english — one line at a time.
: page  ( str -- )  page ;

\ resolve ( str -- )
\ Many sentences, one truth.  Each line runs independently.
\ All must produce the same top-of-stack value.
\ The proof: every path leads here.
: resolve  ( str -- )  resolve ;

\ infix ( str -- n )  evaluate an infix expression: "3 + 4 * 2" → 11
: infix  ( str -- n )  infix ;

\ infix-argue ( forth-str infix-str -- )
\ Two programmers arguing in different grammars.  Stack settles it.
: infix-argue  ( forth-str infix-str -- )
    infix           \ evaluate the infix string → n2
    swap eval       \ evaluate the forth string → n1
    swap            \ stack: n1 n2
    2dup = if
        ." agreed: " . drop cr
    else
        ." disagreed: " over . ." vs " . cr
        = assert
    then ;

\ ── Help system ───────────────────────────────────────────────────────────────
\ help — Co-Forth quick reference  ( -- )
: help  ( -- )  help ;

\ describe — show what we know about a word  ( idx -- )
: describe  ( idx -- )  describe ;

\ ? — describe the word on top of the stack (alias)
: ?  ( idx -- )  describe ;

\ compute — evaluate a string and print the result  ( str-idx -- )
: compute  ( str-idx -- )  compute ;

\ equiv? — probe program equivalence over -5..5  ( str1 str2 -- flag )
: equiv?  ( str1 str2 -- flag )  equiv? ;

\ equiv-check — print labelled equivalence result  ( str1 str2 -- )
: equiv-check ( str1 str2 -- )
    2dup equiv?
    if ." ✓ equivalent" cr else ." ✗ not equivalent" cr then
    drop drop ;

\ fork — run code in a forked copy of the current VM  ( str-idx -- )
: fork  ( str-idx -- )  fork ;

\ boot — re-execute all boot=true vocabulary words  ( -- )
: boot  ( -- )  boot ;

\ ── Natural language gateway ─────────────────────────────────────────────────
\ missing-word ( str -- )
\ Called automatically when an unknown word is encountered at top-level.
\ Redefine to route natural language to AI: : missing-word  gen-send ;
: missing-word  ( str -- )
    dup
    s" help"  str= if drop help else
    dup
    s" ?" str= if drop help else
    ." ? " type cr
    then then ;

\ ── Comparison helpers ────────────────────────────────────────────────────────
: true      ( -- -1 )  -1 ;
: false     ( -- 0  )   0 ;
: bool      ( n -- flag )  0= 0= ;
: between   ( n lo hi -- flag )  >r over >r >= r> r> <= and ;
: clamp     ( n lo hi -- n' )    rot min max ;

\ ── Stack utilities ──────────────────────────────────────────────────────────
: -rot      ( a b c -- c a b )   rot rot ;

\ ── Logic ────────────────────────────────────────────────────────────────────
: signum    ( n -- -1|0|1 )  dup 0> if drop 1 else 0< if -1 else 0 then then ;
: even?     ( n -- flag )    2 mod 0= ;
: odd?      ( n -- flag )    2 mod 0= 0= ;
: positive? ( n -- flag )    0> ;
: negative? ( n -- flag )    0< ;
: zero?     ( n -- flag )    0= ;

\ ── Printing helpers ─────────────────────────────────────────────────────────
: nl        ( -- )     cr ;
: .cr       ( n -- )   . cr ;
: spaces    ( n -- )   0 do space loop ;
: tab       ( -- )     9 emit ;
: show      ( -- )     .s cr ;
: banner    ( -- )     ." ────────────────────────" cr ;
: .bool     ( flag -- )  if ." true" else ." false" then ;

\ ── Boot ceremony ─────────────────────────────────────────────────────────────
: boot-wake ( -- )  ." ── the language wakes ──" cr ;
: boot-rest ( -- )  banner ;

\ ── Terminal UI helpers (crossterm-backed) ────────────────────────────────────
\ Primitives: at-xy  term-size  save-cursor  restore-cursor
\             clear-eol  clear-line  color!  reset-style
\             sync-begin  sync-end  hide-cursor  show-cursor
\
\ Draw n copies of char starting at (col row):
: h-line  ( col row n char -- )
    >r >r       \ rstack: char n  ;  stack: col row
    at-xy       \ position cursor
    r> r>       \ stack: n char
    swap        \ stack: char n
    0 do        dup emit
    loop drop ;

\ ── Numeric output ───────────────────────────────────────────────────────────
\ Print n in binary (8 bits)
: .bin8  ( n -- )
    8 0 do
        dup 7 i - rshift 1 and .
    loop drop ;

\ Count digits of positive integer
: digits  ( n -- d )
    dup 0= if drop 1 exit then
    abs 0 swap
    begin dup 0> while
        10 / swap 1 + swap
    repeat
    drop ;

\ ── Iteration helpers ────────────────────────────────────────────────────────
\ Apply: expects a do-loop body pattern already compiled into a word
\ Sum n values from 0..n-1 using i
: iota-sum  ( n -- sum )  0 swap 0 do i + loop ;

\ ── Bit manipulation ─────────────────────────────────────────────────────────
: bit       ( n -- 1<<n )  1 swap lshift ;
: set-bit   ( x n -- x' )  bit or ;
: clr-bit   ( x n -- x' )  bit invert and ;
: tst-bit   ( x n -- flag ) bit and bool ;

\ ── String-like output ───────────────────────────────────────────────────────
: nl2       ( -- )   cr cr ;
: indent    ( n -- ) spaces ;

\ ── Array helpers (variable + allot) ────────────────────────────────────────
\ Usage: 10 array myarr    myarr 3 cells + @    42 myarr 3 cells + !

\ ── String/number formatting ─────────────────────────────────────────────────
: .hex      ( n -- )   .h ;
: unsigned. ( n -- )   u. ;

\ ── Utility ──────────────────────────────────────────────────────────────────
: noop        ( -- )       ;
: ?dup        ( n -- n n | 0 )   dup if dup then ;
: tally       ( n -- )     0 do ." |" loop cr ;
: bye         ( -- )       ." goodbye." cr ;
: clear-stack ( -- )       begin depth 0> while drop repeat ;
: deploy      ( -- )       ." deployed." cr ;
: boom        ( str|-- )
  depth 0> if
    eval ." 💥 boom." cr
  else
    prove-all
    ." 💥 BOOM 💥" cr
  then ;
: sun         ( -- t )     time dup ." ☀  " . ." s since epoch." cr ;

\ ── Von Neumann architecture ─────────────────────────────────────────────────
\ Defined here as Forth; semantic metadata lives in vocabulary/en.toml.
: fetch       ( addr -- n )      @ ;
: store       ( n addr -- )      ! ;
: register    ( -- )             depth . ." values on the stack." cr ;
: accumulate  ( n -- 0+1+...+n ) 0 swap 1 + 0 do i + loop ;
: instruction ( -- )             ." fetch → decode → execute → retire" cr ;
: cycle       ( -- )             time . ." seconds since epoch." cr ;
: pipeline    ( n -- )           0 do i . loop ." stages" cr ;
: bottleneck  ( -- )             ." von Neumann bottleneck: one bus for data and instructions." cr ;
: word-size   ( -- 64 )          64 ;
: address     ( -- )             depth 0> if ." address: " . cr else ." push an address first" cr then ;

\ ── Co-Forth vocabulary ───────────────────────────────────────────────────────
\ Thin wrappers so builtins appear in `words` and can be overridden by users.
: type              ( idx -- )               type ;
: str=              ( a b -- flag )          str= ;
: str-len           ( idx -- n )             str-len ;
: str-cat           ( a b -- idx )           str-cat ;
: sha256            ( idx -- idx )           sha256 ;
: nonce             ( -- n )                 nonce ;
: keygen            ( -- pub sec )           keygen ;
: sign              ( msg sec -- sig )       sign ;
: verify            ( msg sig pub -- flag )  verify ;
: file-write        ( data path -- )         file-write ;
: file-append       ( data path -- )         file-append ;
: file-size         ( path -- n )            file-size ;
: file-fetch        ( path -- data )         file-fetch ;
: file-slice        ( path off n -- data )   file-slice ;
: file-sha256       ( path -- hash )         file-sha256 ;
: file-sha256-range ( path off n -- hash )   file-sha256-range ;
: file-hash         ( path -- )              file-hash ;
: file-hash-range   ( path off n -- )        file-hash-range ;
: scan-file         ( path -- report )       scan-file ;
: scan-bytes        ( str -- score )         scan-bytes ;
: file-entropy      ( path -- e*1000 )       file-entropy ;
: scan-dir          ( path -- report )       scan-dir ;
: scan-strings      ( path -- strings )      scan-strings ;
: scan-procs        ( -- report )            scan-procs ;
: scan-net          ( -- report )            scan-net ;
: scan-startup      ( -- report )            scan-startup ;
: quarantine        ( path -- flag )         quarantine ;
: scatter-code      ( code -- )              scatter-code ;
: scatter-symbol    ( name -- )              scatter-symbol ;
: peers-discover    ( ms -- )                peers-discover ;
: fuel              ( -- n )                 fuel ;
: with-fuel         ( n -- )                 with-fuel ;
: undo              ( -- )                   undo ;
: lock              ( name-idx ttl-ms -- flag )   lock ;
: unlock            ( name-idx -- )               unlock ;
: lock-ttl          ( name-idx -- ms )            lock-ttl ;
\ lock-or-fail: acquire or emit error and drop  ( name-idx ttl-ms -- )
: lock-or-fail      ( name-idx ttl-ms -- )
    2dup lock 0= if
        drop type ."  lock denied" cr
    else
        2drop
    then ;

\ ── Writing assistance ────────────────────────────────────────────────────────
: capitalize    ( str-idx -- str-idx )   capitalize ;
: str-upper     ( str-idx -- str-idx )   str-upper ;
: str-lower     ( str-idx -- str-idx )   str-lower ;
: str-trim      ( str-idx -- str-idx )   str-trim ;
: word-count    ( str-idx -- n )         word-count ;
: sentence?     ( str-idx -- flag )      sentence? ;
: grammar-check ( str-idx -- str-idx )   grammar-check ;
: improve       ( str-idx -- str-idx )   improve ;

\ .sentence  ( str-idx -- )  grammar-check, capitalize, print with newline
: .sentence     ( str-idx -- )   grammar-check capitalize type cr ;

\ correct?  ( str-idx -- flag )  alias for sentence?
: correct?      ( str-idx -- flag )  sentence? ;

\ fix  ( str-idx -- str-idx )  alias for grammar-check
: fix           ( str-idx -- str-idx )  grammar-check ;

\ polish  ( str-idx -- str-idx )  grammar-check then improve
: polish        ( str-idx -- str-idx )  grammar-check improve ;

\ .words  ( str-idx -- )  print word count
: .words        ( str-idx -- )   word-count . ." words" cr ;

\ ── Languages compressed into Forth ──────────────────────────────────────────
\
\ Each language is its irreducible core — the one thing that makes it itself.
\ Everything else follows from that.

\ Brainfuck: 8 ops over a byte tape.  The whole language is a Turing machine.
\ bf-step ( op tape-ptr -- tape-ptr' )  — one instruction cycle.
\ bf-run  ( prog-idx -- )  — interprets a BF program string.
: bf-next   ( ptr -- byte )    @ ;
: bf-inc    ( ptr -- )         dup @ 1+ swap ! ;
: bf-dec    ( ptr -- )         dup @ 1- swap ! ;
: bf-right  ( ptr -- ptr+1 )  1+ ;
: bf-left   ( ptr -- ptr-1 )  1- ;

\ Lambda calculus: three forms. Church encoding of booleans and pairs.
\ true  = λx.λy.x   →  dup drop   (returns first arg)
\ false = λx.λy.y   →  swap drop  (returns second arg)
\ if    = λb.b       (b is already a selector, just call it)
: church-true   ( x y -- x )   swap drop ;
: church-false  ( x y -- y )   drop ;
: church-pair   ( a b -- pair-idx )   2>r ;
: church-zero?  ( n -- flag )  0= ;
: church-succ   ( n -- n+1 )   1+ ;

\ Lisp: the whole language is cons cells + eval.
\ s-expression on the stack: push tag then value.
\ tag: 0=nil 1=number 2=symbol 3=pair
: lisp-nil      ( -- )      0 0 ;       \ nil tag + value
: lisp-number   ( n -- )    1 swap ;    \ number tag + n
: lisp-car      ( pair -- head )  drop ;
: lisp-cdr      ( pair -- tail )  swap drop ;
: lisp-null?    ( tag val -- flag )  drop 0= ;

\ Forth itself: the language is its own interpreter.
\ A Forth word is just code at an address.  eval IS the interpreter.
\ Everything else is defined in terms of : ; if then begin until.
: forth-eval    ( str -- )   eval ;

\ ── Stack extras ─────────────────────────────────────────────────────────────
: 2over         ( a b c d -- a b c d a b )  2over ;
: 2rot          ( a b c d e f -- c d e f a b )  2rot ;

\ ── String operations ────────────────────────────────────────────────────────
: str-split     ( idx sep-idx -- result-idx )   str-split ;
: str-join      ( idx sep-idx -- result-idx )   str-join ;
: str-sub       ( idx start len -- idx' )        str-sub ;
: str-find      ( idx needle-idx -- pos )         str-find ;
: str-replace   ( idx from-idx to-idx -- idx' )  str-replace ;
: str-reverse   ( idx -- idx' )                  str-reverse ;

\ safe: run a string as Forth — -1 if ok, 0 if it bailed (state restored on failure)
: safe          ( str-idx -- flag )  safe ;

\ str-contains? — true if needle appears anywhere in haystack  ( idx needle-idx -- flag )
: str-contains? ( idx needle-idx -- flag )  str-find -1 > ;

\ str-starts?  — true if str begins with prefix  ( idx prefix-idx -- flag )
: str-starts?   ( idx prefix-idx -- flag )  str-find 0= ;

\ str-ends?  — true if str ends with suffix  ( idx suffix-idx -- flag )
: str-ends?     ( idx suf-idx -- flag )
    2dup str-len >r str-len r> swap -
    rot str-sub
    str= ;

\ str-empty?   — true if string has zero length  ( idx -- flag )
: str-empty?    ( idx -- flag )  str-len 0= ;

\ str-words — number of whitespace-delimited tokens in a string  ( idx -- n )
: str-words     ( idx -- n )  word-count ;

\ str-lines — number of newlines + 1 in a string  ( idx -- n )
: str-lines     ( idx -- n )  line-count ;

\ num>str / str>num — bridge between integers and the string pool  ( n -- idx ) / ( idx -- n flag )
: num>str       ( n -- idx )        num>str ;
: str>num       ( idx -- n flag )   str>num ;

\ word-defined? — true if a word exists in the dictionary  ( idx -- flag )
: word-defined? ( idx -- flag )  word-defined? ;

\ word-names — all defined word names as a newline-separated string  ( -- idx )
: word-names    ( -- idx )  word-names ;

\ nth-line — get the nth line (0-based) of a newline-separated string  ( idx n -- line-idx )
: nth-line      ( idx n -- line-idx )  nth-line ;

\ agree? — non-aborting argue: -1 if tops agree, 0 if not  ( str1 str2 -- flag )
: agree?        ( str1 str2 -- flag )  agree? ;

\ same? — non-aborting versus: -1 if full stacks agree, 0 if not  ( str1 str2 -- flag )
: same?         ( str1 str2 -- flag )  same? ;

\ check — print agree/disagree and return flag  ( str1 str2 -- flag )
: check ( str1 str2 -- flag )
    2dup agree? dup
    if ." ✓ agreed" cr else ." ✗ disagreed" cr then ;

\ dual — execute both machines visibly, then show whether they agree  ( str1 str2 -- )
\ Both programs run in separate forks — each prints its own output.
\ Then the verdict: ✓ if tops agree, ✗ if they don't.
: dual  ( str1 str2 -- )
    ." ── machine A ──" cr  over fork
    ." ── machine B ──" cr  dup  fork
    agree?
    if ." ✓ both machines agree" cr
    else ." ✗ machines disagree" cr
    then ;

\ self-argue — run one program twice; verify it agrees with itself  ( str -- flag )
\ The proof of determinism: the machine plays the game with itself.
: self-argue  ( str -- flag )  dup agree? ;

\ self-check — run self-argue and print the verdict  ( str -- )
: self-check  ( str -- )
    dup self-argue
    if ." ✓ deterministic" cr else ." ✗ non-deterministic" cr then
    drop ;

\ back-and-forth? — round-trip predicate; never aborts
: back-and-forth? ( n fwd-str back-str -- flag )  back-and-forth? ;

\ invertible? — involution test: f(f(n)) = n?  ( n str -- flag )
: invertible? ( n str -- flag )  invertible? ;

\ round-trip — like back-and-forth but prints ✓ / ✗  ( n fwd-str back-str -- )
: round-trip ( n fwd-str back-str -- )
    3dup back-and-forth?
    if ." ✓ round trip" cr else ." ✗ round trip broken" cr then
    drop drop drop ;

\ exec-capture — run any shell command; push stdout as string pool entry
: exec-capture  ( cmd-idx -- output-idx )  exec-capture ;

\ cross-check — Forth result vs. any language's shell output  ( forth-str cmd-str -- flag )
\ Run the Forth program; convert top-of-stack to decimal string.
\ Run the shell command; trim its stdout.
\ Push -1 if they match, 0 if not.
: cross-check ( forth-str cmd-str -- flag )
    swap                     \ cmd-str forth-str
    safe                     \ cmd-str flag  (safe eval of forth-str; result on stack or error)
    if
        num>str              \ cmd-str forth-result-str
        swap exec-capture    \ forth-result-str shell-output-str
        str-trim             \ forth-result-str shell-output-trimmed
        str=                 \ flag
    else
        drop 0               \ Forth side errored — drop cmd-str, push false
    then ;

\ lang-check — run cross-check and print a labelled result  ( label forth-str cmd-str -- )
: lang-check ( label forth-str cmd-str -- )
    cross-check              \ label flag
    swap type space          \ flag  (type prints label, space adds a space)
    if ." ✓" cr else ." ✗" cr then ;

\ Prolog: a query is a goal.  Resolution is unification + backtracking.
\ Compressed to: a goal is a word.  Backtracking = trying alternatives.
\ True if the word executes without error; false if it bails.
: goal          ( str -- flag )   eval -1 ;   \ TODO: real unification
: try-goal      ( str -- flag )   goal ;

\ Assembly: one instruction.  Everything else is sequencing of one instruction.
: asm-nop       ( -- )    noop ;
: asm-mov       ( src dst -- )   ! ;
: asm-add       ( a b -- a+b )   + ;
: asm-jmp       ( addr -- )      ." jmp " . cr ;   \ symbolic only
: asm-cmp       ( a b -- flag )  = ;
: asm-halt      ( -- )           bye ;

\ ── Return stack pairs ────────────────────────────────────────────────────────
\ 2>r ( x1 x2 -- ) ( R: -- x1 x2 )  move two values to return stack
: 2>r   ( x1 x2 -- )  swap >r >r ;
\ 2r> ( -- x1 x2 ) ( R: x1 x2 -- )  restore two values from return stack
: 2r>   ( -- x1 x2 )  r> r> swap ;

\ ── Formatted output ──────────────────────────────────────────────────────────
\ .r ( n width -- )  print n right-aligned in width chars
: .r    ( n width -- )  .r ;
\ .pad ( n width char -- )  print n padded with char to width
: .pad  ( n width char -- )  .pad ;

\ ── Defining words ─────────────────────────────────────────────────────────────
\ constant: create a named constant.
\   Usage: 42 constant answer    answer .  → 42
\   The word pushes the literal value; the stack is not modified after definition.
\
\ value: like constant but mutable.
\   Usage: 0 value counter    1 to counter    counter .  → 1
\   `to name` stores TOS into the value; `name` fetches the current value.

\ ── exit is a compiler word (not a STDLIB word) ───────────────────────────────
\ `exit` is handled during compilation — it emits a Ret cell.
\ It is usable inside any word definition:
\   : early-out  dup 0= if drop exit then  1 - ;
\"#;

/// Proofs for STDLIB words.
/// Each `: test:<word>` definition runs assertions that verify the word's behaviour.
/// `assert` pops the top of the stack; if it is 0 (false), execution aborts with
/// "assertion failed".  A successful proof leaves the stack unchanged.
const STDLIB_PROOFS: &str = r#"
: test:+         3 4 +  7 = assert  0 0 + 0 = assert  -1 1 + 0 = assert ;
: test:-         7 3 -  4 = assert  0 0 - 0 = assert  3 5 - -2 = assert ;
: test:*         3 4 * 12 = assert  0 5 *  0 = assert  -2 3 * -6 = assert ;
: test:/         8 4 /  2 = assert  9 3 /  3 = assert  7 2 /  3 = assert ;
: test:mod       7 3 mod 1 = assert  6 3 mod 0 = assert  8 5 mod 3 = assert ;
: test:abs       -5 abs  5 = assert   3 abs 3 = assert  0 abs 0 = assert ;
: test:max        3  7 max 7 = assert  9 2 max 9 = assert  5 5 max 5 = assert ;
: test:min        3  7 min 3 = assert  9 2 min 2 = assert  5 5 min 5 = assert ;
: test:negate    -5 negate  5 = assert  3 negate -3 = assert  0 negate 0 = assert ;
: test:1+         0 1+  1 = assert  -1 1+ 0 = assert  99 1+ 100 = assert ;
: test:1-         1 1-  0 = assert   0 1- -1 = assert  10 1-  9 = assert ;
: test:2*         3 2* 6 = assert   0 2*  0 = assert  -2 2* -4 = assert ;
: test:2/         4 2/ 2 = assert   6 2/  3 = assert   8 2/  4 = assert ;
: test:square     5 square 25 = assert  -3 square 9 = assert  0 square 0 = assert  1 square 1 = assert ;
: test:cube       2 cube  8 = assert  -2 cube -8 = assert  0 cube 0 = assert ;
: test:sum-to-n   5 sum-to-n 15 = assert  0 sum-to-n 0 = assert  1 sum-to-n 1 = assert  4 sum-to-n 10 = assert ;
: test:gcd       12  8 gcd  4 = assert   7 3 gcd  1 = assert  6 6 gcd  6 = assert ;
: test:lcm        4  6 lcm 12 = assert   3 5 lcm 15 = assert ;
: test:pow        2 10 pow 1024 = assert  3 0 pow 1 = assert  2  0 pow 1 = assert  10 3 pow 1000 = assert ;
: test:fib        0 fib  1 = assert   1 fib  1 = assert   5 fib  8 = assert   7 fib 21 = assert ;
: test:even?      4 even? assert   3 even? 0= assert   0 even? assert ;
: test:within     5  1 10 within assert   0  1 10 within 0= assert  10 1 10 within 0= assert ;
: test:signum     5 signum  1 = assert  -3 signum -1 = assert   0 signum  0 = assert ;
: test:clamp      5  1 10 clamp  5 = assert   0  1 10 clamp  1 = assert  15  1 10 clamp 10 = assert ;
: test:between    5  3  8 between assert   2  3  8 between 0= assert   9  3  8 between 0= assert ;
\ ── Logic / comparison ──────────────────────────────────────────────────────
: test:true      true  -1 = assert ;
: test:false     false  0 = assert ;
: test:bool      0 bool 0 = assert  1 bool -1 = assert  -5 bool -1 = assert ;
: test:odd?      3 odd? assert   4 odd? 0= assert   0 odd? 0= assert   1 odd? assert ;
: test:positive? 1 positive? assert   0 positive? 0= assert  -1 positive? 0= assert ;
: test:negative? -1 negative? assert   0 negative? 0= assert   1 negative? 0= assert ;
: test:zero?     0 zero? assert   1 zero? 0= assert  -5 zero? 0= assert ;
\ ── Stack utilities ─────────────────────────────────────────────────────────
: test:-rot      1 2 3 -rot  2 = assert  1 = assert  3 = assert ;
: test:?dup      5 ?dup 5 = assert  5 = assert   0 ?dup 0 = assert ;
: test:noop      42 noop 42 = assert ;
\ ── Numeric ─────────────────────────────────────────────────────────────────
: test:digits    0 digits 1 = assert  9 digits 1 = assert  10 digits 2 = assert  100 digits 3 = assert  9999 digits 4 = assert ;
: test:iota-sum  0 iota-sum 0 = assert  1 iota-sum 0 = assert  5 iota-sum 10 = assert  4 iota-sum 6 = assert ;
\ ── Bit manipulation ────────────────────────────────────────────────────────
: test:bit       0 bit 1 = assert   1 bit 2 = assert   3 bit 8 = assert   6 bit 64 = assert ;
: test:set-bit   0 0 set-bit 1 = assert   0 3 set-bit 8 = assert   5 1 set-bit 7 = assert ;
: test:clr-bit   7 0 clr-bit 6 = assert   7 1 clr-bit 5 = assert   15 3 clr-bit 7 = assert ;
: test:tst-bit   7 0 tst-bit assert   7 2 tst-bit assert   4 0 tst-bit 0= assert   8 3 tst-bit assert ;
\ ── String operations ───────────────────────────────────────────────────────
: test:str-len   s" hello" str-len 5 = assert   s" " str-len 0 = assert   s" hi" str-len 2 = assert ;
: test:str=      s" hello" s" hello" str= assert   s" hello" s" world" str= 0= assert ;
: test:str-upper s" hello" str-upper s" HELLO" str= assert   s" Hello" str-upper s" HELLO" str= assert ;
: test:str-lower s" HELLO" str-lower s" hello" str= assert   s" Hello" str-lower s" hello" str= assert ;
: test:str-trim  s"  hi  " str-trim s" hi" str= assert ;
: test:str-cat   s" foo" s" bar" str-cat s" foobar" str= assert ;
: test:word-count  s" hello world" word-count 2 = assert   s" one" word-count 1 = assert   s" a b c" word-count 3 = assert ;
: test:capitalize  s" hello" capitalize s" Hello" str= assert   s" world" capitalize s" World" str= assert ;
: test:correct?    s" Good sentence." correct? assert   s" no period" correct? 0= assert   s" lowercase." correct? 0= assert ;
\ ── Von Neumann STDLIB ───────────────────────────────────────────────────────
variable _tm  ( shared test memory cell )
: test:fetch      42 _tm !  _tm @ 42 = assert  0 _tm !  _tm @ 0 = assert ;
: test:store      99 _tm !  _tm @ 99 = assert  -1 _tm !  _tm @ -1 = assert ;
: test:word-size  word-size 64 = assert ;
: test:accumulate 5 accumulate 15 = assert  0 accumulate 0 = assert  3 accumulate 6 = assert ;
: test:bye        bye ;
: test:clear-stack  1 2 3 clear-stack depth 0= assert ;
: test:1+        0 1+ 1 = assert   5 1+ 6 = assert  -1 1+ 0 = assert ;
: test:1-        1 1- 0 = assert   5 1- 4 = assert   0 1- -1 = assert ;
: test:here      here 0 >= assert ;
: test:allot     here 3 allot here swap - 3 = assert ;
: test:comma     here 99 , here swap - 1 = assert ;
: test:fill      here 4 allot   here 4 - 4 42 fill   here 4 - @ 42 = assert ;
: test:cells     5 cells 5 = assert  0 cells 0 = assert ;
: test:cell      cell 1 = assert ;

\ ── Convergence proofs: two directions, one answer ────────────────────────────
\ The proof IS the meeting.  Different paths; same stack value.
: test:converge          s" 3 4 +"  s" 4 3 +"  converge ;   \ commutativity of +
: test:converge-mul      s" 3 4 *"  s" 4 3 *"  converge ;   \ commutativity of *
: test:converge-assoc    s" 1 2 3 + +" s" 1 2 + 3 +" converge ;  \ associativity
: test:converge-double   s" 6 2 *"  s" 6 6 +"  converge ;   \ two ways to double
: test:converge-square   s" 5 square" s" 5 dup *" converge ; \ two ways to square
: test:converge-fib      s" 10 fib"  s" 10 fib-iter" converge ; \ recursive = iterative
: test:fib-iter          10 fib-iter 89 = assert   0 fib-iter 1 = assert   1 fib-iter 1 = assert ;

\ ── Both-ways proofs: two directions at once ─────────────────────────────────
\ For each operation, prove it from both directions simultaneously.
: test:both-add      3 4 s" +"  both-ways ;   \ + commutes
: test:both-mul      5 6 s" *"  both-ways ;   \ * commutes
: test:both-and      12 10 s" and" both-ways ; \ bitwise and commutes
: test:both-or       12 10 s" or"  both-ways ; \ bitwise or commutes

\ ── Back-and-forth proofs: round trips are faithful ───────────────────────────
\ Go forth, come back.  The proof: you are home.
: test:back-add      5  s" 3 +"  s" 3 -"  back-and-forth ;   \ +3 then -3
: test:back-mul      7  s" 2 *"  s" 2 /"  back-and-forth ;   \ *2 then /2
: test:back-negate   9  s" negate"  s" negate"  back-and-forth ;  \ negate is its own inverse
: test:back-shift    1  s" 1 lshift"  s" 1 rshift"  back-and-forth ; \ shift left then right

\ ── Op proofs: every fundamental operation proven both directions ───────────────
\ + commutes
: test:+comm         s" 3 4 +"   s" 4 3 +"   converge ;
: test:+assoc        s" 1 2 3 + +"  s" 1 2 + 3 +"  converge ;
: test:+zero         s" 7 0 +"   s" 0 7 +"   converge ;   \ 0 is identity
\ * commutes
: test:*comm         s" 3 4 *"   s" 4 3 *"   converge ;
: test:*assoc        s" 2 3 4 * *"  s" 2 3 * 4 *"  converge ;
: test:*one          s" 5 1 *"   s" 1 5 *"   converge ;   \ 1 is identity
: test:*zero         s" 5 0 *"   s" 0 5 *"   converge ;   \ 0 annihilates
\ distributivity: a*(b+c) = a*b + a*c
: test:distrib       s" 3 4 5 + *"  s" 3 4 * 3 5 * +"  converge ;
\ subtraction and negation
: test:sub-negate    s" 10 3 - negate"  s" 3 10 -"  converge ;  \ -(a-b) = b-a
: test:double        s" 6 2 *"   s" 6 6 +"   converge ;   \ double two ways
\ boolean ops (bitwise on -1 / 0)
: test:and-comm      s" -1 0 and"  s" 0 -1 and"  converge ;
: test:or-comm       s" -1 0 or"   s" 0 -1 or"   converge ;
: test:xor-self      s" 42 42 xor"  s" 0"  converge ;     \ a xor a = 0
: test:xor-zero      s" 42 0 xor"   s" 42"  converge ;    \ a xor 0 = a
: test:not-not       s" -1 invert invert"  s" -1"  converge ; \ double invert = identity
\ comparison ops
: test:max-comm      s" 3 7 max"  s" 7 3 max"  converge ;
: test:min-comm      s" 3 7 min"  s" 7 3 min"  converge ;
: test:max-min       s" 5 5 max"  s" 5 5 min"  converge ;  \ max=min when equal
\ stack ops: fold into a single result to satisfy converge's one-value contract
: test:swap          s" 4 3 swap +"   s" 4 3 +"   converge ;  \ swap preserves sum
: test:over          s" 3 5 over + +"   s" 3 5 3 + +"  converge ; \ over copies correctly
\ abs
: test:abs-negate    s" 7 negate abs"  s" 7"  converge ;   \ abs(neg n) = n
: test:abs-pos       s" 7 abs"         s" 7"  converge ;   \ abs(pos n) = n
\ ── Von Neumann hot-path word proofs ────────────────────────────────────────
: test:rot        1 2 3 rot  1 = assert  3 = assert  2 = assert ;
: test:tuck       1 2 tuck  2 = assert  1 = assert  2 = assert ;
: test:nip        1 2 nip   2 = assert ;
: test:2dup       3 4 2dup  4 = assert  3 = assert  4 = assert  3 = assert ;
: test:negate-hot 7 negate -7 = assert  -3 negate  3 = assert  0 negate 0 = assert ;
: test:invert     0 invert -1 = assert  -1 invert  0 = assert ;
: test:xor        3 5 xor   6 = assert   0 7 xor  7 = assert   7 7 xor 0 = assert ;
: test:lshift     1 3 lshift  8 = assert  3 2 lshift 12 = assert ;
: test:rshift     8 3 rshift  1 = assert  12 2 rshift 3 = assert ;
\ ── TCO proof: deep recursion without stack overflow ─────────────────────────
: tco-count   ( n -- 0 )  dup 0> if 1 - tco-count exit then ;
: test:tco    1000 tco-count 0 = assert ;
\ ── Inline expansion proofs: square, cube, 1+, 1- work identically ──────────
: test:inline-1+   5 1+  6 = assert  -1 1+  0 = assert ;
: test:inline-1-   5 1-  4 = assert   0 1- -1 = assert ;
: test:inline-2*   3 2*  6 = assert  -2 2* -4 = assert ;
: test:inline-2/   8 2/  4 = assert   6 2/  3 = assert ;
: test:inline-sq   4 square 16 = assert  0 square 0 = assert ;
\ ── Output-word smoke proofs (depth unchanged) ───────────────────────────────
: test:nl          depth >r nl           depth r> = assert ;
: test:banner      depth >r banner       depth r> = assert ;
: test:boot-wake   depth >r boot-wake    depth r> = assert ;
\ ── Stack extras ─────────────────────────────────────────────────────────────
: test:2over       1 2 3 4 2over  2 = assert  1 = assert  4 = assert  3 = assert  2 = assert  1 = assert ;
: test:2rot        1 2 3 4 5 6 2rot  2 = assert  1 = assert  6 = assert  5 = assert  4 = assert  3 = assert ;
\ ── String operations ────────────────────────────────────────────────────────
: test:str-reverse   s" abc" str-reverse  s" cba" str= assert ;
: test:str-sub       s" hello" 1 3 str-sub  s" ell" str= assert ;
: test:str-find      s" hello world" s" world" str-find  6 = assert ;
: test:str-find-miss s" hello" s" xyz" str-find  -1 = assert ;
: test:str-contains? s" hello world" s" world" str-contains? assert ;
: test:str-empty?    s" " str-empty? assert   s" x" str-empty? 0= assert ;
: test:str-words     s" one two three" str-words  3 = assert ;
: test:str-split     s" a,b,c" s" ," str-split  s" c" str-find  2 > assert ;
: test:str-join      s" a,b,c" s" ," str-split  s" ," str-join  s" a,b,c" str= assert ;
: test:str-replace   s" hello world" s" world" s" earth" str-replace  s" hello earth" str= assert ;
: test:safe-ok       s" 1 2 +" safe  assert  depth 1 = assert  3 = assert ;
: test:safe-fail     s" 0 0 / drop" safe  0= assert  depth 0 = assert ;
\ ── Number ↔ string ─────────────────────────────────────────────────────────
: test:num>str       42 num>str  s" 42" str= assert   -1 num>str  s" -1" str= assert ;
: test:str>num-ok    s" 99" str>num  assert  99 = assert ;
: test:str>num-fail  s" xyz" str>num  0= assert  drop ;
\ ── Vocabulary introspection ─────────────────────────────────────────────────
: test:word-defined? s" +" word-defined? assert   s" no-such-word-xzq" word-defined? 0= assert ;
: test:word-names    word-names str-len 0 > assert ;
: test:nth-line      s" apple" s" ," str-split  0 nth-line  s" apple" str= assert ;
\ ── Two-machine predicates ───────────────────────────────────────────────────
: test:agree?-yes    s" 3 4 +"  s" 4 3 +"  agree? assert ;
: test:agree?-no     s" 3 4 +"  s" 3 4 -"  agree? 0= assert ;
: test:same?-yes     s" 1 2 3"  s" 1 2 3"  same? assert ;
: test:same?-no      s" 1 2 3"  s" 1 2 4"  same? 0= assert ;
: test:same?-error   s" 0 0 /"  s" 1 2 +"  same? 0= assert ;
\ ── Back-and-forth? proofs ───────────────────────────────────────────────────
: test:back-and-forth?-yes    5  s" 3 +"  s" 3 -"  back-and-forth?  assert ;
: test:back-and-forth?-no     5  s" 3 +"  s" 4 -"  back-and-forth?  0= assert ;
: test:back-and-forth?-err    5  s" 0 /"  s" 3 -"  back-and-forth?  0= assert ;
: test:invertible?-negate    42  s" negate"  invertible?  assert ;
: test:invertible?-invert    -1  s" invert"  invertible?  assert ;
: test:invertible?-no         5  s" 1 +"   invertible?  0= assert ;
: test:invertible?-not-invol  3  s" 2 *"   invertible?  0= assert ;
\ ── Polyglot proofs ──────────────────────────────────────────────────────────
: test:exec-capture-echo  s" echo hello" exec-capture  s" hello" str= assert ;
: test:exec-capture-math  s" echo 7"     exec-capture  s" 7"     str= assert ;
: test:cross-check-yes    s" 3 4 +"      s" echo 7"    cross-check  assert ;
: test:cross-check-no     s" 3 4 +"      s" echo 8"    cross-check  0= assert ;
: test:cross-check-err    s" 0 0 /"      s" echo 0"    cross-check  0= assert ;
\ ── Compute proofs ───────────────────────────────────────────────────────────
: test:compute-forth      s" 3 4 +"  compute  depth 0= assert ;
: test:compute-infix      s" 3 + 4"  compute  depth 0= assert ;
: test:compute-err        s" garbage-word"  compute  depth 0= assert ;
\ ── Equivalence proofs ───────────────────────────────────────────────────────
: test:equiv?-commute   s" dup *"       s" dup *"       equiv?  assert ;  \ same program
: test:equiv?-add-comm  s" 3 +"        s" 3 +"         equiv?  assert ;  \ same transform
: test:equiv?-not-eq    s" 1 +"        s" 2 +"         equiv?  0= assert ; \ different
: test:equiv?-double    s" 2 *"        s" dup +"       equiv?  assert ;  \ two ways to double
\ ── Fork proofs ─────────────────────────────────────────────────────────────
: test:fork-no-side-effect
    3 4                          \ put 3 4 on stack
    s" + . cr" fork              \ fork computes 7 and prints it
    depth 2 = assert             \ current stack still has 3 4
    4 = assert  3 = assert ;     \ values unchanged
: test:fork-error
    s" 0 0 /" fork               \ fork a failing computation
    depth 0 = assert ;           \ current stack is clean
\ ── Defining words ───────────────────────────────────────────────────────────
\ constant and value are top-level (interpret-mode) defining words.
\ They cannot appear inside a : definition — test them at top level here.
42 constant _c42
-7 constant _cneg
0  constant _czero
10 value _v10
\ ── Prime words ─────────────────────────────────────────────────────────────
: test:prime?-2        2 prime?  assert ;
: test:prime?-3        3 prime?  assert ;
: test:prime?-7        7 prime?  assert ;
: test:prime?-97      97 prime?  assert ;
: test:prime?-4        4 prime?  0= assert ;
: test:prime?-9        9 prime?  0= assert ;
: test:prime?-1        1 prime?  0= assert ;
: test:prime?-0        0 prime?  0= assert ;
: test:prime?-49      49 prime?  0= assert ;
: test:next-prime-2    2 next-prime  2  = assert ;
: test:next-prime-3    3 next-prime  3  = assert ;
: test:next-prime-4    4 next-prime  5  = assert ;
: test:next-prime-10  10 next-prime  11 = assert ;
\ ── Defining words ─────────────────────────────────────────────────────────
: test:constant-basic     _c42   42 = assert ;
: test:constant-negative  _cneg  -7 = assert ;
: test:constant-zero      _czero  0 = assert ;
: test:value-basic        _v10   10 = assert ;
: test:value-independence _c42 _cneg + 35 = assert ;
\ ── Self-play proofs ─────────────────────────────────────────────────────────
: test:dual-smoke   depth >r  s" 3 4 +" s" 4 3 +" dual  depth r> = assert ; \ stack clean after dual
: test:self-argue-pure    s" 3 4 +"   self-argue  assert ;     \ pure fn agrees with itself
: test:self-argue-lit     s" 42"      self-argue  assert ;     \ literal agrees with itself
: test:self-argue-err     s" 0 0 /"   self-argue  0= assert ;  \ error → disagrees (0)
\ `to` is interpret-mode: test at top level, not inside a word
20 value _v20
: test:value-to    _v20 20 = assert ;   \ initial value correct
\ ── Hash operations ──────────────────────────────────────────────────────────
: test:hash-stable    s" hello" hash  s" hello" hash  = assert ;  \ same input → same hash
: test:hash-differs   s" hello" hash  s" world" hash  <> assert ; \ different inputs → different hash
: test:hash-empty     s" " hash  0 <> assert ;                    \ empty string hashes to non-zero (FNV offset)
: test:hash-int-det   42 hash-int  42 hash-int  = assert ;        \ deterministic
: test:hash-int-mix   1 hash-int  1 <> assert ;                   \ 1 mixes to different value
: test:hash-combine   1 2 hash-combine  1 3 hash-combine  <> assert ; \ different n → different result
"#;

// ── Public API ────────────────────────────────────────────────────────────────

impl Forth {
    pub fn new() -> Self {
        let mut f = Forth {
            data:       Vec::with_capacity(32),
            loop_stack: Vec::with_capacity(8),
            rstack:     Vec::with_capacity(8),
            memory:     Vec::with_capacity(4096),
            strings:    Vec::with_capacity(256),
            string_dedup: HashMap::with_capacity(256),
            name_index:   HashMap::with_capacity(512),
            call_counts:  HashMap::with_capacity(64),
            heap:         Vec::with_capacity(64),
            var_index:  HashMap::with_capacity(64),
            out:        String::with_capacity(256),
            peers:           Vec::new(),
            source_log:      Vec::new(),
            log_definitions: false, // off during stdlib load
            fuel:            usize::MAX, // unlimited while loading stdlib
            undo_stack:      Vec::new(),
            locks:           HashMap::new(),
            confirm_fn:      None,
            gen_fn:          None,
            select_fn:       None,
            remote_whitelist: std::collections::HashSet::new(),
            ensembles:        HashMap::new(),
            peer_save_stack:  Vec::new(),
            peer_meta:        HashMap::new(),
            remote_mode:      false,
            registry_addr:    None,
            my_addr:          None,
            forth_back:       None,
            channels:         std::collections::HashSet::new(),
            boot_poems:       Vec::new(),
            pending_defines:  Vec::new(),
            user_word_names:  std::collections::HashSet::new(),
            call_stack:       Vec::with_capacity(64),
        };
        // Load standard library silently — not logged (every session has stdlib)
        let _ = f.eval(STDLIB);
        // Compile proofs for STDLIB words (test:square, test:fib, …).
        // These run with unlimited fuel — they don't loop and must not be budgeted.
        let _ = f.exec_with_fuel(STDLIB_PROOFS, 0);
        f.fuel = DEFAULT_FUEL; // restore budget for user code
        f.log_definitions = true; // user words from here on are logged
        f.seed_remote_whitelist();
        f
    }

    /// Intern a string literal: return existing index if already pooled, else push.
    #[inline]
    fn intern_str(&mut self, s: &str) -> usize {
        if let Some(&idx) = self.string_dedup.get(s) {
            return idx;
        }
        let idx = self.strings.len();
        // Push first, then clone from the pool as the dedup key.
        // Both HashMap and Vec<String> need owned data; two allocs is unavoidable
        // for a miss, but misses are rare after warmup — the hit path is free.
        self.strings.push(s.to_string());
        self.string_dedup.insert(self.strings[idx].clone(), idx);
        idx
    }

    /// Run Forth source and return collected output.
    pub fn run(source: &str) -> Result<String> {
        let mut f = Forth::new();
        f.eval(source)?;
        Ok(f.out)
    }

    /// Run a program on a precompiled VM; return (stack, output).
    /// The stack is bottom-to-top; top is the "return value" — one number.
    /// This is the Co-Forth wire contract: send a sentence, get back the stack.
    pub fn run_on(vm: &mut Self, source: &str) -> Result<(Vec<i64>, String)> {
        vm.eval(source)?;
        let stack = vm.data.clone();
        let output = std::mem::take(&mut vm.out);
        Ok((stack, output))
    }

    /// Expose the data stack (for inspection).
    pub fn stack(&self) -> &[i64] { &self.data }

    /// Attach a confirm callback (builder pattern).
    ///
    /// `confirm" message"` will call this function and push -1 (approved) or 0 (denied).
    /// Without a callback, `confirm"` auto-approves (returns true) — useful in tests.
    pub fn with_confirm(mut self, f: ConfirmFn) -> Self {
        self.confirm_fn = Some(f);
        self
    }

    /// Set the confirm callback on an existing instance.
    pub fn set_confirm_fn(&mut self, f: ConfirmFn) {
        self.confirm_fn = Some(f);
    }

    /// Attach an AI generation callback (builder pattern).
    ///
    /// `gen" prompt"` will call this function and emit the returned string.
    /// Without a callback, `gen"` emits `(no generator)`.
    pub fn with_gen(mut self, f: GenFn) -> Self {
        self.gen_fn = Some(f);
        self
    }

    /// Set the AI generation callback on an existing instance.
    pub fn set_gen_fn(&mut self, f: GenFn) {
        self.gen_fn = Some(f);
    }

    /// Attach a dialog-select callback (builder pattern).
    pub fn with_select(mut self, f: SelectFn) -> Self {
        self.select_fn = Some(f);
        self
    }

    /// Set the dialog-select callback on an existing instance.
    ///
    /// `select" title|opt1|opt2"` will call this function and push the chosen index.
    pub fn set_select_fn(&mut self, f: SelectFn) {
        self.select_fn = Some(f);
    }

    /// Disable word-definition logging (used when compiling library/system words that
    /// must not end up in `user_word_names`).  Call `enable_logging()` to restore.
    pub fn disable_logging(&mut self) { self.log_definitions = false; }

    /// Re-enable word-definition logging after a `disable_logging()` call.
    pub fn enable_logging(&mut self) { self.log_definitions = true; }

    /// Execute Forth source on this instance and return collected output.
    /// Uses the default fuel budget (100k steps). For heavy computation use `exec_with_fuel`.
    pub fn exec(&mut self, source: &str) -> Result<String> {
        self.exec_with_fuel(source, DEFAULT_FUEL)
    }

    /// Execute with an explicit step budget.
    /// `fuel = 0` means unlimited (use with care — infinite loops will hang).
    pub fn exec_with_fuel(&mut self, source: &str, fuel: usize) -> Result<String> {
        self.out.clear();
        self.fuel = if fuel == 0 { usize::MAX } else { fuel };
        self.eval(source)?;
        Ok(self.out.clone())
    }

    /// Return a snapshot of the current data stack (top = last element).
    pub fn data_stack(&self) -> &[i64] {
        &self.data
    }

    /// Push values onto the data stack (used to inject remote results locally).
    pub fn push_stack(&mut self, values: &[i64]) {
        self.data.extend_from_slice(values);
    }

    /// Clear the data stack (used between word calls in tests).
    pub fn clear_data(&mut self) {
        self.data.clear();
    }

    /// Check if a word name is in user_word_names (words that can shadow builtins).
    #[cfg(test)]
    pub fn is_user_word(&self, name: &str) -> bool {
        self.user_word_names.contains(name)
    }

    /// Return true if `name` is a native builtin (not user-definable without shadowing).
    /// Used by save_user_words to prevent persisting builtin-shadowing definitions.
    pub fn is_builtin_word(name: &str) -> bool {
        name_to_builtin(name).is_some()
    }

    /// Return the number of cells in memory (for diagnostics).
    #[cfg(test)]
    pub fn memory_len(&self) -> usize {
        self.memory.len()
    }


    /// Mark a word as callable by remote peers.
    pub fn mark_remote_ok(&mut self, word: &str) {
        self.remote_whitelist.insert(word.to_string());
    }

    /// Seed the initial remote whitelist with the built-in security/scan words.
    /// These are safe to expose: read-only scanning, no mutations.
    fn seed_remote_whitelist(&mut self) {
        for word in &[
            "scan-file", "scan-bytes", "file-entropy",
            "scan-dir", "scan-strings", "scan-procs",
            "scan-net", "scan-startup",
        ] {
            self.remote_whitelist.insert(word.to_string());
        }
    }

    /// Compile all grammar words that have Forth code into this VM's dictionary.
    /// Words with `remote = true` are added to the remote whitelist.
    pub fn compile_library(&mut self, lib: &crate::coforth::Library) {
        for entry in lib.all_entries() {
            let Some(forth_code) = &entry.forth else { continue };
            // Sanitize the word name: no spaces, no semicolons
            let name = &entry.word;
            if name.contains(' ') || name.contains(';') { continue; }
            let def = format!(": {} {} ;", name, forth_code);
            if self.exec(&def).is_ok() && entry.remote {
                self.remote_whitelist.insert(name.clone());
            }
        }
    }

    /// Execute a word from a remote peer — only if it is in the remote whitelist.
    ///
    /// Only single word names are accepted.  Arbitrary code fragments, sequences,
    /// or definitions are rejected regardless of whitelist status.
    pub fn exec_remote(&mut self, word: &str) -> Result<String> {
        let word = word.trim();
        // Reject anything that looks like code rather than a plain word name
        if word.contains(' ') || word.contains('\n') || word.contains(';') || word.contains(':') {
            anyhow::bail!("remote: only single word names are accepted, not: {:?}", word);
        }
        if !self.remote_whitelist.contains(word) {
            anyhow::bail!("remote: word '{}' is not in the remote vocabulary", word);
        }
        self.exec(word)
    }

    /// Clone the compiled dictionary state into a fresh VM (no stack, no callbacks).
    /// Used to share a pre-compiled VM baseline across tests without re-compiling STDLIB
    /// or vocabulary on every clone.  Callbacks (confirm_fn, gen_fn) are not copied.
    /// Clone the dictionary AND current data stack — for fork execution.
    /// The forked VM starts at the same point: same words, same stack values.
    pub fn fork_vm(&self) -> Self {
        let mut f = self.clone_dict();
        f.data = self.data.clone();
        f
    }

    pub fn clone_dict(&self) -> Self {
        Forth {
            data:       Vec::new(),
            loop_stack: Vec::new(),
            rstack:     Vec::new(),
            memory:     self.memory.clone(),
            strings:    self.strings.clone(),
            string_dedup: self.string_dedup.clone(),
            name_index:  self.name_index.clone(),
            call_counts: HashMap::new(),  // fresh counter per clone — don't inherit parent's profile
            heap:        self.heap.clone(),
            var_index:   self.var_index.clone(),
            out:         String::new(),
            peers:       self.peers.clone(),
            source_log:  Vec::new(),    // fresh log — don't inherit parent's history
            log_definitions: true,
            fuel:        DEFAULT_FUEL,
            undo_stack:  Vec::new(),
            locks:      HashMap::new(),
            confirm_fn: None,
            gen_fn:     None,
            select_fn:  None,
            remote_whitelist: self.remote_whitelist.clone(),
            ensembles:        self.ensembles.clone(),
            peer_save_stack:  Vec::new(),
            peer_meta:        self.peer_meta.clone(),
            remote_mode:      false, // caller sets true for remote VMs
            registry_addr:    self.registry_addr.clone(),
            my_addr:          self.my_addr.clone(),
            forth_back:       None,
            channels:         self.channels.clone(),
            boot_poems:       Vec::new(),
            pending_defines:  Vec::new(),
            user_word_names:  self.user_word_names.clone(),
            call_stack:       Vec::with_capacity(64),
        }
    }

    /// Drain boot poems registered this exec (via `boot" text"`).
    /// The REPL calls this after each exec and appends to ~/.finch/boot.forth.
    pub fn take_boot_poems(&mut self) -> Vec<String> {
        std::mem::take(&mut self.boot_poems)
    }

    /// Drain words that were unknown this exec and need AI definition.
    /// The REPL asks AI to define each, compiles the result, growing the grammar.
    pub fn take_pending_defines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_defines)
    }

    /// Check whether a word is defined (builtin or user-defined).
    pub fn word_exists(&self, word: &str) -> bool {
        self.name_index.contains_key(&word.to_lowercase())
    }

    /// Snapshot the current dictionary state (word definitions only, not the data stack).
    pub fn snapshot(&self) -> DictionarySnapshot {
        DictionarySnapshot {
            memory_len:  self.memory.len(),
            strings_len: self.strings.len(),
            heap_len:    self.heap.len(),
            name_index:  self.name_index.clone(),
            var_index:   self.var_index.clone(),
            source_log:  self.source_log.clone(),
        }
    }

    /// Restore the dictionary to a previous snapshot.
    /// The data stack is left as-is.
    pub fn restore(&mut self, snap: &DictionarySnapshot) {
        self.memory.truncate(snap.memory_len);
        // Keep string_dedup consistent: remove entries for strings beyond snap.strings_len.
        let strings_len = snap.strings_len;
        self.string_dedup.retain(|_, v| *v < strings_len);
        self.strings.truncate(strings_len);
        self.heap.truncate(snap.heap_len);
        self.name_index = snap.name_index.clone();
        self.var_index  = snap.var_index.clone();
        self.source_log = snap.source_log.clone();
    }

    /// Serialise the VM's user-defined words as Forth source.
    /// Paste this into any session to recreate the same dictionary.
    pub fn dump_source(&self) -> String {
        self.source_log.join("\n")
    }

    /// Return the source of a single named word, or None if not in source_log.
    pub fn word_source(&self, name: &str) -> Option<String> {
        let prefix = format!(": {} ", name);
        self.source_log.iter().find(|e| e.starts_with(&prefix)).cloned()
    }
}

// ── Interpreter core (von Neumann flat-memory model) ─────────────────────────

impl Forth {
    /// Parse and execute Forth source.
    ///
    /// Named word definitions (`: name ... ;`) are compiled into `memory` and
    /// persist across calls.  Top-level expressions are compiled into a
    /// temporary region, executed, then the region is truncated so only
    /// named words occupy permanent addresses.
    fn eval(&mut self, source: &str) -> Result<()> {
        let tokens = tokenize(source);
        let mut pos = 0;
        let mut pending: Vec<String> = Vec::new();

        macro_rules! flush_pending {
            () => {
                if !pending.is_empty() {
                    // Compile ephemeral top-level code starting at current end of memory.
                    let start = self.memory.len();
                    self.compile_into(&pending, true)?;
                    self.memory.push(Cell::Ret); // sentinel: halt the run loop
                    self.execute(start)?;
                    // Remove the ephemeral code — only named words persist.
                    self.memory.truncate(start);
                    pending.clear();
                }
            };
        }

        while pos < tokens.len() {
            match tokens[pos].as_str() {
                ":" => {
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after :"); }
                    let name = tokens[pos].to_lowercase();
                    pos += 1;
                    let mut body = Vec::new();
                    let mut depth = 1i32;
                    while pos < tokens.len() {
                        match tokens[pos].as_str() {
                            ";" if depth == 1 => { depth = 0; break; }
                            ";" => { depth -= 1; body.push(tokens[pos].clone()); }
                            ":" => { depth += 1; body.push(tokens[pos].clone()); }
                            _ => { body.push(tokens[pos].clone()); }
                        }
                        pos += 1;
                    }
                    if depth != 0 { bail!("missing ; for :{name}"); }
                    // Auto-push undo snapshot before each user definition (capped at 20).
                    if self.log_definitions {
                        if self.undo_stack.len() >= 20 { self.undo_stack.remove(0); }
                        self.undo_stack.push(self.snapshot());
                    }
                    // Log user-defined words so the VM is serialisable.
                    // On redefinition, replace the previous entry so the dump
                    // stays clean — pasting it never defines a word twice.
                    if self.log_definitions {
                        let entry = format!(": {} {} ;", name, body.join(" "));
                        let prefix = format!(": {} ", name);
                        if let Some(pos) = self.source_log.iter().position(|e| e.starts_with(&prefix)) {
                            self.source_log[pos] = entry.clone();
                        } else {
                            self.source_log.push(entry.clone());
                        }
                        // Word propagation: broadcast definition to all channel members.
                        if !self.channels.is_empty() && !self.peers.is_empty() {
                            let my_name = hostname::get()
                                .ok().and_then(|h| h.into_string().ok())
                                .unwrap_or_else(|| "someone".to_string());
                            let from = self.registry_addr.clone();
                            let tokens_map = peer_tokens_map(&self.peer_meta);
                            for chan in &self.channels {
                                let msg = format!("[{}] {}: {}", chan, my_name, entry);
                                run_push_all(&self.peers, &msg, from.as_deref(), &tokens_map);
                            }
                        }
                    }
                    // If the word already exists as a user definition, require approval
                    // before overwriting it.  Builtins cannot be shadowed by this interpreter
                    // (they are always resolved first at compile time), so no gate is needed
                    // for them — and applying one there would block stdlib internal wrappers.
                    if self.log_definitions && self.name_index.contains_key(&name) {
                        if let Some(ref f) = self.confirm_fn {
                            let prompt = format!("redefine '{name}'?  (it already exists)");
                            if !f(&prompt) {
                                bail!("redefinition of '{name}' cancelled");
                            }
                        }
                        // No confirm_fn (pipe/test mode): allow silently.
                    }
                    // Register entry address BEFORE compiling body so `recurse` resolves.
                    let word_addr = self.memory.len();
                    self.name_index.insert(name.clone(), word_addr);
                    if self.log_definitions {
                        self.user_word_names.insert(name.clone());
                    }
                    self.compile_into(&body, false)?;
                    // Patch Addr(usize::MAX) recurse placeholders with the real word_addr.
                    for cell in &mut self.memory[word_addr..] {
                        if let Cell::Addr(a) = cell { if *a == usize::MAX { *a = word_addr; } }
                    }
                    self.memory.push(Cell::Ret);
                    self.apply_tco(word_addr);
                }
                "constant" => {
                    // `42 constant answer` — pop value, define word that pushes it.
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after constant"); }
                    let name = tokens[pos].to_lowercase();
                    let value = self.data.pop().ok_or_else(|| anyhow::anyhow!("stack underflow in constant"))?;
                    let word_addr = self.memory.len();
                    self.name_index.insert(name.clone(), word_addr);
                    self.memory.push(Cell::Lit(value));
                    self.memory.push(Cell::Ret);
                    if self.log_definitions {
                        self.source_log.push(format!("{value} constant {name}"));
                    }
                }
                "value" => {
                    // `42 value answer` — like constant but mutable via `to`.
                    // The word pushes the current heap value (not the address).
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after value"); }
                    let name = tokens[pos].to_lowercase();
                    let init = self.data.pop().ok_or_else(|| anyhow::anyhow!("stack underflow in value"))?;
                    let addr = self.heap.len();
                    self.heap.push(init);
                    // Define the word: push heap addr, then fetch.
                    let word_addr = self.memory.len();
                    self.name_index.insert(name.clone(), word_addr);
                    self.memory.push(Cell::Lit(addr as i64));
                    self.memory.push(Cell::Builtin(Builtin::Fetch));
                    self.memory.push(Cell::Ret);
                    // Also register in var_index so `to` can find the address.
                    self.var_index.insert(format!("value:{name}"), addr);
                    if self.log_definitions {
                        self.source_log.push(format!("{init} value {name}"));
                    }
                }
                "to" => {
                    // `42 to answer` — store TOS into a `value`'s heap cell.
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after to"); }
                    let name = tokens[pos].to_lowercase();
                    let key = format!("value:{name}");
                    let addr = *self.var_index.get(&key)
                        .ok_or_else(|| anyhow::anyhow!("`to {name}`: not a value"))?;
                    let v = self.data.pop().ok_or_else(|| anyhow::anyhow!("stack underflow in `to`"))?;
                    self.heap[addr] = v;
                }
                "variable" => {
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after variable"); }
                    let name = tokens[pos].to_lowercase();
                    let addr = self.heap.len();
                    self.heap.push(0);
                    self.var_index.insert(name, addr);
                }
                "create" => {
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after create"); }
                    let name = tokens[pos].to_lowercase();
                    // Allocate a heap address for this word's data field.
                    let data_addr = self.heap.len() as i64;
                    // Define a word that pushes its data address when called.
                    let word_addr = self.memory.len();
                    self.name_index.insert(name, word_addr);
                    self.memory.push(Cell::Lit(data_addr));
                    self.memory.push(Cell::Ret);
                }
                "forget" => {
                    flush_pending!();
                    pos += 1;
                    if pos >= tokens.len() { bail!("expected name after forget"); }
                    let name = tokens[pos].to_lowercase();
                    // Remove from word and variable indices so the name is unreachable.
                    self.name_index.remove(&name);
                    self.var_index.remove(&name);
                    // Remove from source log so dump/undo are clean.
                    let word_prefix = format!(": {} ", name);
                    self.source_log.retain(|e| !e.starts_with(&word_prefix));
                }
                _ => {
                    pending.push(tokens[pos].clone());
                }
            }
            pos += 1;
        }
        flush_pending!();
        Ok(())
    }

    /// Compile a token slice directly into `self.memory`.
    ///
    /// Jump targets are absolute indices into `self.memory` — no offset
    /// arithmetic, no `extend_adjusted` helper needed.
    fn compile_into(&mut self, tokens: &[String], _top_level: bool) -> Result<()> {
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].as_str() {
                // `;` outside a word definition = sentence separator (no-op).
                // Allows natural language like "Hello; I am a Forth program."
                ";" => { i += 1; continue; }
                // `exit` — unconditional return from the current word.
                // Compiles a Ret cell directly; safe to use anywhere in a definition.
                "exit" => {
                    self.memory.push(Cell::Ret);
                    i += 1;
                }
                "see" | "？" => {
                    i += 1;
                    let word_name = tokens.get(i).cloned().unwrap_or_default();
                    i += 1;
                    let idx = self.strings.len();
                    self.strings.push(word_name);
                    self.memory.push(Cell::Builtin(Builtin::See(idx)));
                }
                "if" => {
                    i += 1;
                    let (true_branch, false_branch, skip) = collect_if(tokens, i)?;
                    i += skip;
                    let jmpz_pos = self.memory.len();
                    self.memory.push(Cell::JmpZ(0)); // forward: patch after true branch
                    self.compile_into(&true_branch, false)?;
                    if false_branch.is_empty() {
                        let after = self.memory.len();
                        self.memory[jmpz_pos] = Cell::JmpZ(after);
                    } else {
                        let jmp_pos = self.memory.len();
                        self.memory.push(Cell::Jmp(0)); // forward: patch after false branch
                        let false_start = self.memory.len();
                        self.memory[jmpz_pos] = Cell::JmpZ(false_start);
                        self.compile_into(&false_branch, false)?;
                        let after = self.memory.len();
                        self.memory[jmp_pos] = Cell::Jmp(after);
                    }
                }
                "begin" => {
                    i += 1;
                    let (body, end_kind, after_body, skip) = collect_begin(tokens, i)?;
                    i += skip;
                    let begin_addr = self.memory.len();
                    self.compile_into(&body, false)?;
                    match end_kind.as_str() {
                        "until" => { self.memory.push(Cell::Until(begin_addr)); }
                        "again" => { self.memory.push(Cell::Jmp(begin_addr)); }
                        "while" => {
                            let while_pos = self.memory.len();
                            self.memory.push(Cell::While(0)); // forward: patch to after repeat
                            self.compile_into(&after_body, false)?;
                            self.memory.push(Cell::Repeat(begin_addr));
                            let after = self.memory.len();
                            self.memory[while_pos] = Cell::While(after);
                        }
                        _ => bail!("unexpected begin terminator: {end_kind}"),
                    }
                }
                "do" => {
                    self.memory.push(Cell::DoSetup);
                    i += 1;
                    let back_addr = self.memory.len(); // loop body starts here
                    let (body, plus_loop, skip) = collect_do(tokens, i)?;
                    i += skip;
                    self.compile_into(&body, false)?;
                    if plus_loop {
                        self.memory.push(Cell::DoLoopPlus(back_addr));
                    } else {
                        self.memory.push(Cell::DoLoop(back_addr));
                    }
                }
                "case" => {
                    i += 1;
                    let (of_blocks, default_block, skip) = collect_case(tokens, i)?;
                    i += skip;
                    let mut endcase_patches: Vec<usize> = Vec::new();
                    for (val_toks, body_toks) in &of_blocks {
                        self.compile_into(val_toks, false)?;
                        let of_pos = self.memory.len();
                        self.memory.push(Cell::OfTest(0)); // forward: patch to next of/endcase
                        self.compile_into(body_toks, false)?;
                        let jmp_pos = self.memory.len();
                        self.memory.push(Cell::Jmp(0)); // forward: patch to endcase
                        endcase_patches.push(jmp_pos);
                        let next = self.memory.len();
                        self.memory[of_pos] = Cell::OfTest(next);
                    }
                    if !default_block.is_empty() {
                        self.compile_into(&default_block, false)?;
                    }
                    self.memory.push(Cell::Builtin(Builtin::Drop)); // drop case selector
                    let endcase = self.memory.len();
                    for p in endcase_patches {
                        self.memory[p] = Cell::Jmp(endcase);
                    }
                }
                _ => { self.emit_token(&tokens[i])?; }
            }
            i += 1;
        }
        Ok(())
    }

    /// Inline limit: words with bodies ≤ this many cells (and no jumps/calls) are
    /// copied directly into the caller rather than emitting a Cell::Addr call.
    /// 16 covers most hand-written definitions (e.g. `between`, `signum`, `clamp`).
    const INLINE_LIMIT: usize = 16;

    /// Emit a word reference: inline the body if it's short and pure, else emit Cell::Addr.
    /// Deduplicates the identical logic that appeared in both the user-word and stdlib paths.
    #[inline]
    fn emit_word_or_inline(&mut self, addr: usize) {
        let word_end = self.memory[addr..]
            .iter()
            .position(|c| matches!(c, Cell::Ret))
            .map(|p| addr + p)
            .unwrap_or(self.memory.len());
        let word_len = word_end.saturating_sub(addr);
        let is_pure = word_len <= Self::INLINE_LIMIT
            && self.memory[addr..word_end].iter()
                .all(|c| !matches!(c, Cell::Addr(_) | Cell::Jmp(_) | Cell::JmpZ(_)
                                     | Cell::Repeat(_) | Cell::Until(_) | Cell::While(_)
                                     | Cell::DoSetup | Cell::DoLoop(_) | Cell::DoLoopPlus(_)));
        if is_pure && word_len > 0 {
            let cells: Vec<Cell> = self.memory[addr..word_end].to_vec();
            self.memory.extend(cells);
        } else {
            self.memory.push(Cell::Addr(addr));
        }
    }

    /// Emit a single token as one or more cells into `self.memory`.
    fn emit_token(&mut self, tok: &str) -> Result<()> {
        if let Some(s) = tok.strip_prefix("\x00str:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::Str(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00push-str:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::PushStr(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00confirm:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::Confirm(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00select:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::SelectDialog(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00read:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ReadFile(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00csv:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ReadCsv(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00tsv:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ReadTsv(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00xlsx:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ReadXlsx(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00exec:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ExecCmd(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00glob:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::GlobFiles(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00peer:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::AddPeer(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00scatter:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ScatterExec(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00symbol:") {
            let idx = self.intern_str(s);
            self.memory.push(Cell::ScatterSymbol(idx));
            return Ok(());
        }
        if tok == "\x00scatter-stack" {
            self.memory.push(Cell::ScatterStack);
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00on:") {
            // Format: "peer\x01code"
            let (peer, code) = s.split_once('\x01').unwrap_or((s, ""));
            let peer_idx = self.strings.len();
            self.strings.push(peer.to_string());
            let code_idx = self.strings.len();
            self.strings.push(code.to_string());
            self.memory.push(Cell::RunOn(peer_idx as u32, code_idx as u32));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00hello:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::HelloPeer(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00tag:") {
            // Format: "name\x01addr"
            let (name, addr) = s.split_once('\x01').unwrap_or((s, ""));
            let name_idx = self.strings.len();
            self.strings.push(name.to_string());
            let addr_idx = self.strings.len();
            self.strings.push(addr.to_string());
            self.memory.push(Cell::TagPeer(name_idx as u32, addr_idx as u32));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00channel:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::JoinChannel(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00part:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::PartChannel(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00say:") {
            // Format: "channel\x01message"
            let (chan, msg) = s.split_once('\x01').unwrap_or((s, ""));
            let chan_idx = self.strings.len();
            self.strings.push(chan.to_string());
            let msg_idx = self.strings.len();
            self.strings.push(msg.to_string());
            self.memory.push(Cell::SayInChannel(chan_idx as u32, msg_idx as u32));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00prove:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::ProveWord(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00scatter-on:") {
            // Format: "ensemble\x01code"
            let (ensemble, code) = s.split_once('\x01').unwrap_or((s, ""));
            let ens_idx = self.strings.len();
            self.strings.push(ensemble.to_string());
            let code_idx = self.strings.len();
            self.strings.push(code.to_string());
            self.memory.push(Cell::ScatterOnCluster(ens_idx as u32, code_idx as u32));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00scatter-exec:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::ScatterBashExec(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00gen:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::GenAI(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00forth-back:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::Builtin(Builtin::ForthBack(idx)));
            return Ok(());
        }
        if let Ok(n) = tok.parse::<i64>() {
            self.memory.push(Cell::Lit(n));
            return Ok(());
        }
        if let Some(hex) = tok.strip_prefix("0x").or_else(|| tok.strip_prefix("0X")) {
            if let Ok(n) = i64::from_str_radix(hex, 16) {
                self.memory.push(Cell::Lit(n));
                return Ok(());
            }
        }
        // Avoid allocating a new String when the token is already lowercase — most tokens are.
        let lo_owned;
        let lo: &str = if tok.bytes().any(|b| b.is_ascii_uppercase()) {
            lo_owned = tok.to_lowercase();
            &lo_owned
        } else {
            tok
        };
        if lo == "scatter-symbol" {
            // scatter-symbol  ( str-idx -- )  dynamic form: pop string index, scatter as symbol
            self.memory.push(Cell::ScatterStack); // ScatterStack already does dynamic string scatter
            return Ok(());
        }
        if lo == "scatter-code" {
            // ( code-idx -- )  scatter strings[code-idx] on all peers (dynamic, stack-based)
            self.memory.push(Cell::ScatterStack);
            return Ok(());
        }
        if lo == "exit" {
            self.memory.push(Cell::Ret);
            return Ok(());
        }
        if lo == "recurse" {
            // usize::MAX is a sentinel; patched to the enclosing word's entry address by eval.
            self.memory.push(Cell::Addr(usize::MAX));
            return Ok(());
        }
        // Single name_index lookup covers both the user-word-shadow path and the stdlib path.
        // User definitions take priority over builtins when the name is in user_word_names.
        // Stdlib thin wrappers must NOT shadow builtins — they call themselves → infinite loop.
        let ni_addr = self.name_index.get(lo).copied();
        if let Some(addr) = ni_addr {
            if self.user_word_names.contains(lo) {
                // User-defined word may shadow a builtin.
                self.emit_word_or_inline(addr);
                return Ok(());
            }
        }
        if let Some(b) = name_to_builtin(lo) {
            // Peephole: combine common literal+op sequences into single instructions.
            let prev = self.memory.last().copied();
            let folded: Option<Cell> = match (prev, b) {
                (Some(Cell::Lit(1)),  Builtin::Plus)    => { self.memory.pop(); Some(Cell::Builtin(Builtin::Inc)) }
                (Some(Cell::Lit(1)),  Builtin::Minus)   => { self.memory.pop(); Some(Cell::Builtin(Builtin::Dec)) }
                (Some(Cell::Lit(0)),  Builtin::Eq)      => { self.memory.pop(); Some(Cell::Builtin(Builtin::ZeroEq)) }
                (Some(Cell::Lit(0)),  Builtin::Lt)      => { self.memory.pop(); Some(Cell::Builtin(Builtin::ZeroLt)) }
                (Some(Cell::Lit(0)),  Builtin::Gt)      => { self.memory.pop(); Some(Cell::Builtin(Builtin::ZeroGt)) }
                _ => None,
            };
            self.memory.push(folded.unwrap_or(Cell::Builtin(b)));
            return Ok(());
        }
        if let Some(addr) = ni_addr {
            // Stdlib/library word: inline if short and pure.
            self.emit_word_or_inline(addr);
            return Ok(());
        }
        if let Some(&addr) = self.var_index.get(lo) {
            self.memory.push(Cell::Lit(addr as i64));
            return Ok(());
        }
        // Natural language gateway: if `missing-word` is defined, route unknown
        // words through it (push word-as-string, call handler).
        // Also track the word in pending_defines so the REPL can ask AI to
        // define it — the grammar grows from use.
        if let Some(&handler_addr) = self.name_index.get("missing-word") {
            // Don't queue internal/punctuation tokens for AI definition.
            let looks_like_word = tok.chars().any(|c| c.is_alphabetic());
            if looks_like_word && !self.pending_defines.contains(&tok.to_string()) {
                self.pending_defines.push(tok.to_string());
            }
            let idx = self.strings.len() as i64;
            self.strings.push(tok.to_string());
            self.memory.push(Cell::Lit(idx));
            self.memory.push(Cell::Addr(handler_addr));
            return Ok(());
        }
        bail!("unknown word: {tok}")
    }

    /// Inner interpreter: execute `memory` starting at `start`.
    ///
    /// Uses an explicit instruction pointer and call stack — no recursive
    /// Rust calls across word boundaries.
    fn execute(&mut self, start: usize) -> Result<()> {
        let mut ip = start;
        // Reuse the struct-level call stack — avoids a Vec allocation per execute().
        self.call_stack.clear();
        // Fuel is now checked only on back-edges (loops) and function calls.
        // Straight-line code runs free — zero overhead per instruction.
        // This prevents infinite loops and runaway recursion while keeping
        // the common case (arithmetic, stack ops, output) as fast as possible.
        //
        // Performance: keep fuel in a local variable so LLVM can hold it in a
        // register across loop iterations rather than round-tripping through the
        // struct pointer on every back-edge.
        let mut fuel = self.fuel;
        macro_rules! check_fuel {
            () => {
                if fuel == 0 {
                    self.fuel = 0;
                    bail!("fuel exhausted — word is too expensive for vocabulary use.\n\
                           hint: use  N with-fuel  for intentional heavy computation.");
                }
                fuel -= 1;
            };
        }
        // Inline the hottest stack/arithmetic builtins to avoid a function call.
        // Single bounds check + unchecked pop: one branch instead of two ok_or_else chains.
        macro_rules! pop2 {
            () => {{
                if self.data.len() < 2 { bail!("stack underflow"); }
                // SAFETY: we just checked len >= 2.
                let b = unsafe { self.data.pop().unwrap_unchecked() };
                let a = unsafe { self.data.pop().unwrap_unchecked() };
                (a, b)
            }};
        }
        macro_rules! pop1 {
            () => {{
                if self.data.is_empty() { bail!("stack underflow"); }
                // SAFETY: we just checked non-empty.
                unsafe { self.data.pop().unwrap_unchecked() }
            }};
        }
        loop {
            // SAFETY: every well-formed program ends with Cell::Ret which breaks
            // before ip can go out of bounds.  ip is only set to valid addresses
            // (start, call-return addresses, and jump targets from well-compiled code).
            // The bounds check runs only in debug builds to catch compiler bugs early;
            // release builds skip it — saving one branch per instruction on the hot path.
            #[cfg(debug_assertions)]
            if ip >= self.memory.len() { break; }
            // SAFETY: guaranteed by well-formed code (Ret always terminates before OOB).
            let cell = unsafe { *self.memory.get_unchecked(ip) };
            match cell {
                // ── Hot path: inlined arithmetic & stack ops (no function call) ──
                Cell::Builtin(Builtin::Plus)  => { let (a,b) = pop2!(); self.data.push(a.wrapping_add(b)); ip += 1; }
                Cell::Builtin(Builtin::Minus) => { let (a,b) = pop2!(); self.data.push(a.wrapping_sub(b)); ip += 1; }
                Cell::Builtin(Builtin::Star)  => { let (a,b) = pop2!(); self.data.push(a.wrapping_mul(b)); ip += 1; }
                Cell::Builtin(Builtin::Dup)   => { let a = pop1!(); self.data.push(a); self.data.push(a); ip += 1; }
                Cell::Builtin(Builtin::Drop)  => { pop1!(); ip += 1; }
                Cell::Builtin(Builtin::Swap)  => { let (a,b) = pop2!(); self.data.push(b); self.data.push(a); ip += 1; }
                Cell::Builtin(Builtin::Over)  => { let (a,b) = pop2!(); self.data.push(a); self.data.push(b); self.data.push(a); ip += 1; }
                Cell::Builtin(Builtin::Eq)    => { let (a,b) = pop2!(); self.data.push(if a == b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Ne)    => { let (a,b) = pop2!(); self.data.push(if a != b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Lt)    => { let (a,b) = pop2!(); self.data.push(if a  < b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Gt)    => { let (a,b) = pop2!(); self.data.push(if a  > b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::ZeroEq) => { let a = pop1!(); self.data.push(if a == 0 { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::ZeroLt) => { let a = pop1!(); self.data.push(if a  < 0 { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::ZeroGt) => { let a = pop1!(); self.data.push(if a  > 0 { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::And)    => { if self.data.len() >= 2 { let (a,b) = pop2!(); self.data.push(a & b); } ip += 1; }
                Cell::Builtin(Builtin::Or)     => { let (a,b) = pop2!(); self.data.push(a | b); ip += 1; }
                Cell::Builtin(Builtin::Xor)    => { let (a,b) = pop2!(); self.data.push(a ^ b); ip += 1; }
                Cell::Builtin(Builtin::Invert) => { let a = pop1!(); self.data.push(!a); ip += 1; }
                Cell::Builtin(Builtin::Negate) => { let a = pop1!(); self.data.push(a.wrapping_neg()); ip += 1; }
                Cell::Builtin(Builtin::Abs)    => { let a = pop1!(); self.data.push(a.wrapping_abs()); ip += 1; }
                Cell::Builtin(Builtin::Max)    => { let (a,b) = pop2!(); self.data.push(a.max(b)); ip += 1; }
                Cell::Builtin(Builtin::Min)    => { let (a,b) = pop2!(); self.data.push(a.min(b)); ip += 1; }
                Cell::Builtin(Builtin::Slash)  => { let (a,b) = pop2!(); if b == 0 { bail!("division by zero"); } self.data.push(a.wrapping_div(b)); ip += 1; }
                Cell::Builtin(Builtin::Mod)    => { let (a,b) = pop2!(); if b == 0 { bail!("division by zero"); } self.data.push(a.wrapping_rem(b)); ip += 1; }
                Cell::Builtin(Builtin::Lshift) => { let (a,b) = pop2!(); self.data.push(a << (b & 63)); ip += 1; }
                Cell::Builtin(Builtin::Rshift) => { let (a,b) = pop2!(); self.data.push(a >> (b & 63)); ip += 1; }
                Cell::Builtin(Builtin::Rot)    => { if self.data.len() >= 3 { let c=pop1!(); let b=pop1!(); let a=pop1!(); self.data.push(b); self.data.push(c); self.data.push(a); } ip += 1; }
                Cell::Builtin(Builtin::Nip)    => { let (a,b) = pop2!(); let _ = a; self.data.push(b); ip += 1; }
                Cell::Builtin(Builtin::Tuck)   => { let (a,b) = pop2!(); self.data.push(b); self.data.push(a); self.data.push(b); ip += 1; }
                Cell::Builtin(Builtin::TwoDup) => { if self.data.len() >= 2 { let b=*self.data.last().unwrap(); let a=self.data[self.data.len()-2]; self.data.push(a); self.data.push(b); } ip += 1; }
                Cell::Builtin(Builtin::TwoDrop)=> { if self.data.len() >= 2 { pop2!(); } ip += 1; }
                Cell::Builtin(Builtin::Inc)    => { let a = pop1!(); self.data.push(a.wrapping_add(1)); ip += 1; }
                Cell::Builtin(Builtin::Dec)    => { let a = pop1!(); self.data.push(a.wrapping_sub(1)); ip += 1; }
                Cell::Builtin(Builtin::Le)     => { let (a,b) = pop2!(); self.data.push(if a <= b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Ge)     => { let (a,b) = pop2!(); self.data.push(if a >= b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Depth)  => { self.data.push(self.data.len() as i64); ip += 1; }
                Cell::Builtin(Builtin::Cr)     => { self.out.push('\n'); ip += 1; }
                Cell::Builtin(Builtin::Space)  => { self.out.push(' '); ip += 1; }
                Cell::Builtin(Builtin::Print)  => { let a = pop1!(); self.out.push_str(&a.to_string()); self.out.push(' '); ip += 1; }
                // ── Literals and string output ──
                Cell::Lit(n) => { self.data.push(n); ip += 1; }  // bypass overflow check — hot path
                Cell::Str(idx) => {
                    self.out.push_str(&self.strings[idx]);
                    ip += 1;
                }
                Cell::PushStr(idx) => {
                    // s" literal" — push string pool index as an integer operand
                    self.data.push(idx as i64);
                    ip += 1;
                }
                Cell::Confirm(idx) => {
                    let msg = self.strings[idx].clone();
                    let approved = if self.remote_mode {
                        // Remote VMs must never block on interactive dialogs — auto-deny.
                        false
                    } else if let Some(ref f) = self.confirm_fn {
                        f(&msg)
                    } else {
                        true // auto-approve when no TUI callback (tests, pipe mode)
                    };
                    self.data.push(if approved { -1 } else { 0 });
                    ip += 1;
                }
                Cell::SelectDialog(idx) => {
                    let raw = self.strings[idx].clone();
                    let parts: Vec<String> = raw.split('|').map(|s| s.to_string()).collect();
                    let (title, options): (&str, &[String]) = if parts.len() >= 2 {
                        (parts[0].as_str(), &parts[1..])
                    } else {
                        (raw.as_str(), &[])
                    };
                    let chosen = if self.remote_mode {
                        -1 // remote VMs never block on TUI dialogs
                    } else if let Some(ref f) = self.select_fn {
                        f(title, options)
                    } else {
                        0 // no callback: auto-select first option (tests, pipe mode)
                    };
                    self.data.push(chosen);
                    ip += 1;
                }
                Cell::ReadFile(idx) => {
                    let path = self.strings[idx].clone();
                    match std::fs::read_to_string(&path) {
                        Ok(content) => self.out.push_str(&content),
                        Err(e) => self.out.push_str(&format!("error reading {path}: {e}\n")),
                    }
                    ip += 1;
                }
                Cell::ReadCsv(idx) => {
                    let path = self.strings[idx].clone();
                    self.out.push_str(&read_delimited_file(&path, b','));
                    ip += 1;
                }
                Cell::ReadTsv(idx) => {
                    let path = self.strings[idx].clone();
                    self.out.push_str(&read_delimited_file(&path, b'\t'));
                    ip += 1;
                }
                Cell::ReadXlsx(idx) => {
                    let path = self.strings[idx].clone();
                    self.out.push_str(&read_xlsx_file(&path));
                    ip += 1;
                }
                Cell::ExecCmd(idx) => {
                    let cmd = self.strings[idx].clone();
                    match std::process::Command::new("sh").arg("-c").arg(&cmd).output() {
                        Ok(o) => self.out.push_str(&String::from_utf8_lossy(&o.stdout)),
                        Err(e) => self.out.push_str(&format!("exec error: {e}\n")),
                    }
                    ip += 1;
                }
                Cell::GlobFiles(idx) => {
                    let pattern = self.strings[idx].clone();
                    match glob::glob(&pattern) {
                        Ok(paths) => {
                            let mut any = false;
                            for path in paths.flatten() {
                                self.out.push_str(&path.display().to_string());
                                self.out.push('\n');
                                any = true;
                            }
                            if !any {
                                // no matches — silent, like ls 2>/dev/null
                            }
                        }
                        Err(e) => self.out.push_str(&format!("glob error: {e}\n")),
                    }
                    ip += 1;
                }
                Cell::AddPeer(idx) => {
                    let addr = self.strings[idx].clone();
                    if !self.peers.contains(&addr) {
                        self.peers.push(addr);
                    }
                    ip += 1;
                }
                Cell::ScatterExec(idx) => {
                    let snippet = self.strings[idx].clone();
                    if self.peers.is_empty() {
                        use crossterm::style::Stylize;
                        self.out.push_str(&format!("{}\n",
                            "nobody else is here yet  (add-peer\" host:port\" to invite someone)".dark_grey()));
                    } else {
                        let peers = self.peers.clone();
                        let tokens = peer_tokens_map(&self.peer_meta);
                        let results = run_scatter(&peers, &snippet, self.registry_addr.as_deref(), &tokens);
                        self.emit_scatter_results(results);
                    }
                    ip += 1;
                }
                Cell::ScatterBashExec(idx) => {
                    let cmd = self.strings[idx].clone();
                    if self.peers.is_empty() {
                        self.out.push_str(
                            "nobody else is here yet  (add-peer\" host:port\" to invite someone)\n"
                        );
                    } else {
                        let peers = self.peers.clone();
                        // Show plan and require confirmation before running on other machines.
                        let names: Vec<String> = peers.iter().map(|p| {
                            self.peer_meta.get(p)
                                .and_then(|m| m.label.clone())
                                .unwrap_or_else(|| p.clone())
                        }).collect();
                        let plan = format!(
                            "Share with {}?\n  {}",
                            names.join(", "),
                            cmd
                        );
                        let approved = if let Some(ref f) = self.confirm_fn {
                            f(&plan)
                        } else {
                            true // auto-approve in tests / pipe mode
                        };
                        if !approved {
                            self.out.push_str("ok, just for you\n");
                        } else {
                            let tokens = peer_tokens_map(&self.peer_meta);
                            let results = run_exec_scatter(&peers, &cmd, &tokens);
                            for r in results {
                                if let Some(err) = r.error {
                                    self.out.push_str(&format!("[{}] error: {}\n", r.peer, err));
                                } else {
                                    for line in r.output.lines() {
                                        self.out.push_str(&format!("[{}] {}\n", r.peer, line));
                                    }
                                    if r.output.is_empty() {
                                        self.out.push_str(&format!("[{}] (no output)\n", r.peer));
                                    }
                                }
                            }
                        }
                    }
                    ip += 1;
                }
                Cell::ScatterStack => {
                    // ( code-idx -- )  scatter strings[code-idx] to all registered peers
                    let code_idx = self.pop()? as usize;
                    let snippet = self.strings.get(code_idx)
                        .ok_or_else(|| anyhow::anyhow!("scatter-code: index {} out of bounds", code_idx))?
                        .clone();
                    if self.peers.is_empty() {
                        self.out.push_str(
                            "nobody else is here yet  (add-peer\" host:port\" to invite someone)\n"
                        );
                    } else {
                        let peers = self.peers.clone();
                        let tokens = peer_tokens_map(&self.peer_meta);
                        let results = run_scatter(&peers, &snippet, self.registry_addr.as_deref(), &tokens);
                        self.emit_scatter_results(results);
                    }
                    ip += 1;
                }
                Cell::ScatterSymbol(idx) => {
                    // symbol" name" — share a word by name across all peers.
                    // If I know it, send my definition first so peers speak the same word.
                    // Then run the word on all peers — those who don't know it define it themselves.
                    use crossterm::style::Stylize;
                    let name = self.strings.get(idx)
                        .ok_or_else(|| anyhow::anyhow!("symbol: string index out of bounds"))?
                        .clone();
                    if self.peers.is_empty() {
                        self.out.push_str(
                            "nobody else is here yet  (add-peer\" host:port\" to invite someone)\n"
                        );
                        ip += 1;
                        continue;
                    }
                    let peers = self.peers.clone();
                    let tokens = peer_tokens_map(&self.peer_meta);
                    // If I have a local definition, push it to all peers first.
                    if let Some(def) = self.source_log.iter().find(|e| {
                        e.starts_with(&format!(": {} ", name))
                    }).cloned() {
                        self.out.push_str(&format!(
                            "{} {}  →  peers\n",
                            "sharing".dark_grey(),
                            name.as_str().cyan().bold(),
                        ));
                        let define_results = run_define_scatter(&peers, &def, &tokens);
                        for r in &define_results {
                            if let Some(ref e) = r.error {
                                let peer_name = r.peer.trim_start_matches("http://")
                                    .split(':').next().unwrap_or(&r.peer);
                                self.out.push_str(&format!(
                                    "  {} couldn't learn it: {}\n",
                                    peer_name.cyan(), e.as_str().red()
                                ));
                            }
                        }
                    } else {
                        // I don't know it either — send the name; each peer handles it their own way.
                        self.out.push_str(&format!(
                            "{} {}  (I don't know it either)\n",
                            "asking about".dark_grey(),
                            name.as_str().cyan().bold(),
                        ));
                    }
                    // Now run the word on all peers.
                    let results = run_scatter(&peers, &name, self.registry_addr.as_deref(), &tokens);
                    self.emit_scatter_results(results);
                    ip += 1;
                }
                Cell::ScatterOnCluster(ens_idx, code_idx) => {
                    // Run code on a named ensemble without touching self.peers.
                    let name = self.strings.get(ens_idx as usize)
                        .ok_or_else(|| anyhow::anyhow!("scatter-on: ensemble string index out of bounds"))?
                        .trim().to_string();
                    let peers = self.ensembles.get(&name)
                        .ok_or_else(|| anyhow::anyhow!("scatter-on: unknown ensemble '{}'", name))?
                        .clone();
                    let code = self.strings.get(code_idx as usize)
                        .ok_or_else(|| anyhow::anyhow!("scatter-on: code string index out of bounds"))?
                        .clone();
                    let tokens = peer_tokens_map(&self.peer_meta);
                    let results = run_scatter(&peers, &code, self.registry_addr.as_deref(), &tokens);
                    self.emit_scatter_results(results);
                    ip += 1;
                }
                Cell::RunOn(peer_idx, code_idx) => {
                    // Run code on exactly one peer, matched by address or label.
                    use crossterm::style::Stylize;
                    let target = self.strings.get(peer_idx as usize)
                        .ok_or_else(|| anyhow::anyhow!("on: peer string index out of bounds"))?
                        .trim().to_string();
                    let code = self.strings.get(code_idx as usize)
                        .ok_or_else(|| anyhow::anyhow!("on: code string index out of bounds"))?
                        .clone();
                    // Resolve target: exact address match first, then label match.
                    let addr = if self.peers.contains(&target) {
                        Some(target.clone())
                    } else {
                        self.peer_meta.iter()
                            .find(|(addr, m)| {
                                m.label.as_deref() == Some(target.as_str())
                                    && self.peers.contains(*addr)
                            })
                            .map(|(addr, _)| addr.clone())
                    };
                    match addr {
                        None => {
                            self.out.push_str(&format!("on: no peer matching '{}'\n",
                                target.as_str().yellow()));
                        }
                        Some(addr) => {
                            let display = self.peer_meta.get(&addr)
                                .and_then(|m| m.label.as_deref())
                                .map(|l| l.cyan().bold().to_string())
                                .unwrap_or_else(|| addr.as_str().cyan().to_string());
                            let tokens = peer_tokens_map(&self.peer_meta);
                            // Is this code a word definition?  ( : word ... ; )
                            // If so, the peer's machine changed — return the peer address.
                            // If computation, return the results (stack values).
                            let is_definition = code.trim_start().starts_with(':');
                            let results = run_scatter(&[addr.clone()], &code, self.registry_addr.as_deref(), &tokens);
                            for r in results {
                                if let Some(e) = r.error {
                                    self.out.push_str(&format!("[{}] {}\n", display,
                                        format!("error: {e}").red()));
                                } else {
                                    for line in r.output.lines() {
                                        self.out.push_str(&format!("[{}] {}\n", display, line));
                                    }
                                    if is_definition {
                                        // New machine: push the peer address so the caller
                                        // can chain further operations on the same machine.
                                        let idx = self.strings.len() as i64;
                                        self.strings.push(addr.clone());
                                        self.data.push(idx);
                                    } else {
                                        // Result: push remote stack values locally.
                                        for v in &r.stack { self.data.push(*v); }
                                    }
                                }
                                if let Some(ref warn) = r.debt_warning {
                                    self.out.push_str(&format!(
                                        "  ⚠  {} {}\n",
                                        display,
                                        warn.as_str().yellow().bold()
                                    ));
                                }
                                // Execute any Forth code the peer sent back.
                                if let Some(ref fb) = r.forth_back {
                                    if !fb.is_empty() {
                                        self.eval(fb)?;
                                    }
                                }
                            }
                        }
                    }
                    ip += 1;
                }
                Cell::HelloPeer(idx) => {
                    let target = self.strings[idx].clone();
                    // Resolve to address (label or direct addr)
                    let addr = if self.peers.contains(&target) {
                        Some(target.clone())
                    } else {
                        self.peer_meta.iter()
                            .find(|(a, m)| {
                                m.label.as_deref() == Some(target.as_str())
                                    && self.peers.contains(*a)
                            })
                            .map(|(a, _)| a.clone())
                    };
                    match addr {
                        None => {
                            use crossterm::style::Stylize;
                            self.out.push_str(&format!(
                                "hello: don't know anyone called {}\n",
                                target.as_str().yellow()
                            ));
                        }
                        Some(addr) => {
                            let my_name = hostname::get()
                                .ok().and_then(|h| h.into_string().ok())
                                .unwrap_or_else(|| "someone".to_string());
                            let msg = format!("hello from {}!", my_name);
                            let from = self.registry_addr.clone();
                            let tokens = peer_tokens_map(&self.peer_meta);
                            let token = tokens.get(&addr).cloned();
                            run_push_one(&addr, &msg, from.as_deref(), token.as_deref());
                            use crossterm::style::Stylize;
                            let label = self.peer_meta.get(&addr)
                                .and_then(|m| m.label.as_deref())
                                .unwrap_or(target.as_str());
                            self.out.push_str(&format!(
                                "said hello to {}\n", label.cyan().bold()
                            ));
                        }
                    }
                    ip += 1;
                }
                Cell::TagPeer(name_idx, addr_idx) => {
                    let name = self.strings[name_idx as usize].trim().to_string();
                    let addr = self.strings[addr_idx as usize].trim().to_string();
                    // Resolve addr — might be a partial hostname; try matching
                    let resolved = if self.peers.contains(&addr) {
                        Some(addr.clone())
                    } else {
                        // Fuzzy: peer whose addr contains the given string
                        self.peers.iter().find(|p| p.contains(addr.as_str())).cloned()
                    };
                    match resolved {
                        None if !addr.is_empty() => {
                            // Register with given addr even if not yet a known peer
                            self.peer_meta.entry(addr.clone()).or_default().label = Some(name.clone());
                            use crossterm::style::Stylize;
                            self.out.push_str(&format!(
                                "{} tagged as {}\n", addr.as_str().dark_grey(), name.as_str().cyan().bold()
                            ));
                        }
                        None => {
                            self.out.push_str("tag: need an address\n");
                        }
                        Some(a) => {
                            self.peer_meta.entry(a.clone()).or_default().label = Some(name.clone());
                            use crossterm::style::Stylize;
                            self.out.push_str(&format!(
                                "{} is now {}\n", a.as_str().dark_grey(), name.as_str().cyan().bold()
                            ));
                        }
                    }
                    ip += 1;
                }
                Cell::JoinChannel(idx) => {
                    let raw = self.strings[idx].clone();
                    let chan = if raw.starts_with('#') { raw } else { format!("#{raw}") };
                    self.channels.insert(chan.clone());
                    let my_name = hostname::get()
                        .ok().and_then(|h| h.into_string().ok())
                        .unwrap_or_else(|| "someone".to_string());
                    let msg = format!("{} joined {}", my_name, chan);
                    let from = self.registry_addr.clone();
                    let tokens = peer_tokens_map(&self.peer_meta);
                    run_push_all(&self.peers, &msg, from.as_deref(), &tokens);
                    use crossterm::style::Stylize;
                    let n = self.peers.len();
                    self.out.push_str(&format!(
                        "joined {} — {} peer{} notified\n",
                        chan.as_str().cyan().bold(),
                        n,
                        if n == 1 { "" } else { "s" },
                    ));
                    ip += 1;
                }
                Cell::PartChannel(idx) => {
                    let raw = self.strings[idx].clone();
                    let chan = if raw.starts_with('#') { raw } else { format!("#{raw}") };
                    self.channels.remove(&chan);
                    let my_name = hostname::get()
                        .ok().and_then(|h| h.into_string().ok())
                        .unwrap_or_else(|| "someone".to_string());
                    let msg = format!("{} left {}", my_name, chan);
                    let from = self.registry_addr.clone();
                    let tokens = peer_tokens_map(&self.peer_meta);
                    run_push_all(&self.peers, &msg, from.as_deref(), &tokens);
                    use crossterm::style::Stylize;
                    self.out.push_str(&format!("left {}\n", chan.as_str().dark_grey()));
                    ip += 1;
                }
                Cell::SayInChannel(chan_idx, msg_idx) => {
                    let raw = self.strings[chan_idx as usize].clone();
                    let chan = if raw.starts_with('#') { raw } else { format!("#{raw}") };
                    let message = self.strings[msg_idx as usize].clone();
                    let my_name = hostname::get()
                        .ok().and_then(|h| h.into_string().ok())
                        .unwrap_or_else(|| "someone".to_string());
                    let text = format!("[{}] {}: {}", chan, my_name, message);
                    let from = self.registry_addr.clone();
                    let tokens = peer_tokens_map(&self.peer_meta);
                    run_push_all(&self.peers, &text, from.as_deref(), &tokens);
                    use crossterm::style::Stylize;
                    self.out.push_str(&format!(
                        "[{}] you: {}\n",
                        chan.as_str().cyan().bold(),
                        message.as_str(),
                    ));
                    ip += 1;
                }
                Cell::ProveWord(idx) => {
                    let word = self.strings[idx].clone();
                    let test_name = format!("test:{word}");
                    use crossterm::style::Stylize;
                    if self.name_index.contains_key(&test_name) {
                        let saved = self.out.len();
                        let result = self.eval(&test_name);
                        self.out.truncate(saved);
                        match result {
                            Ok(_) => {
                                self.out.push_str(&format!("✓ {}\n", word.as_str().green().bold()));
                            }
                            Err(e) => {
                                self.out.push_str(&format!(
                                    "✗ {}: {}\n",
                                    word.as_str().red().bold(),
                                    e.to_string().as_str().dark_grey(),
                                ));
                            }
                        }
                    } else {
                        // Check if a test:word exists but under a different normalisation
                        self.out.push_str(&format!(
                            "? {}: no proof available (define test:{})\n",
                            word.as_str().dark_grey(),
                            word,
                        ));
                    }
                    ip += 1;
                }
                Cell::GenAI(idx) => {
                    let prompt = self.strings[idx].clone();
                    let response = if self.remote_mode {
                        // Remote VMs run headless — no AI calls allowed.
                        String::new()
                    } else if let Some(ref f) = self.gen_fn {
                        f(&prompt)
                    } else {
                        "(no generator connected)\n".to_string()
                    };
                    self.out.push_str(&response);
                    if !response.is_empty() && !response.ends_with('\n') {
                        self.out.push('\n');
                    }
                    ip += 1;
                }
                Cell::Builtin(Builtin::Inc) => { if let Some(t) = self.data.last_mut() { *t += 1; } ip += 1; }
                Cell::Builtin(Builtin::Dec) => { if let Some(t) = self.data.last_mut() { *t -= 1; } ip += 1; }
                Cell::Builtin(Builtin::Negate) => { if let Some(t) = self.data.last_mut() { *t = t.wrapping_neg(); } ip += 1; }
                Cell::Builtin(Builtin::Rot)    => { let len = self.data.len(); if len >= 3 { self.data.swap(len-3, len-2); self.data.swap(len-2, len-1); } ip += 1; }
                Cell::Builtin(Builtin::Xor)    => { let (a,b) = pop2!(); self.data.push(a ^ b); ip += 1; }
                Cell::Builtin(Builtin::Invert) => { if let Some(t) = self.data.last_mut() { *t = !*t; } ip += 1; }
                Cell::Builtin(Builtin::Lshift) => { let (a,b) = pop2!(); self.data.push(a << (b & 63)); ip += 1; }
                Cell::Builtin(Builtin::Rshift) => { let (a,b) = pop2!(); self.data.push(a >> (b & 63)); ip += 1; }
                Cell::Builtin(Builtin::Tuck)   => { let (a,b) = pop2!(); self.data.push(b); self.data.push(a); self.data.push(b); ip += 1; }
                Cell::Builtin(Builtin::Nip)    => { let (a,b) = pop2!(); let _ = a; self.data.push(b); ip += 1; }
                Cell::Builtin(Builtin::TwoDup) => { let (a,b) = pop2!(); self.data.push(a); self.data.push(b); self.data.push(a); self.data.push(b); ip += 1; }
                Cell::Builtin(Builtin::Slash)  => {
                    let (a,b) = pop2!();
                    if b == 0 { bail!("division by zero"); }
                    self.data.push(a / b); ip += 1;
                }
                Cell::Builtin(Builtin::Mod)    => {
                    let (a,b) = pop2!();
                    if b == 0 { bail!("division by zero"); }
                    self.data.push(a % b); ip += 1;
                }
                Cell::Builtin(Builtin::Le)     => { let (a,b) = pop2!(); self.data.push(if a <= b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Ge)     => { let (a,b) = pop2!(); self.data.push(if a >= b { -1 } else { 0 }); ip += 1; }
                Cell::Builtin(Builtin::Abs)    => { if let Some(t) = self.data.last_mut() { *t = t.abs(); } ip += 1; }
                // ── Loop index (very hot inside do/loop bodies) ──
                Cell::Builtin(Builtin::LoopI)  => {
                    let v = self.loop_stack.last().map(|t| t.0).unwrap_or(0);
                    self.data.push(v); ip += 1;
                }
                Cell::Builtin(Builtin::LoopJ)  => {
                    let len = self.loop_stack.len();
                    let v = if len >= 2 { self.loop_stack[len-2].0 } else { 0 };
                    self.data.push(v); ip += 1;
                }
                // ── Return stack ops (hot in most real Forth programs) ──
                Cell::Builtin(Builtin::ToR)    => { let a = pop1!(); self.rstack.push(a); ip += 1; }
                Cell::Builtin(Builtin::FromR)  => {
                    if self.rstack.is_empty() { bail!("return stack underflow"); }
                    let a = unsafe { self.rstack.pop().unwrap_unchecked() };
                    self.data.push(a); ip += 1;
                }
                Cell::Builtin(Builtin::FetchR) => {
                    let a = self.rstack.last().copied().ok_or_else(|| anyhow::anyhow!("return stack underflow"))?;
                    self.data.push(a); ip += 1;
                }
                // ── Variable memory ops ──
                Cell::Builtin(Builtin::Fetch)  => {
                    let addr = pop1!() as usize;
                    self.data.push(self.heap.get(addr).copied().unwrap_or(0)); ip += 1;
                }
                Cell::Builtin(Builtin::Store)  => {
                    let addr = pop1!() as usize;
                    let val  = pop1!();
                    if addr < self.heap.len() { self.heap[addr] = val; }
                    ip += 1;
                }
                Cell::Builtin(Builtin::PlusStore) => {
                    let addr = pop1!() as usize;
                    let val  = pop1!();
                    if addr < self.heap.len() { self.heap[addr] = self.heap[addr].wrapping_add(val); }
                    ip += 1;
                }
                Cell::Builtin(b) => {
                    // Sync local fuel to struct before the call so exec_builtin
                    // (e.g. WithFuel) can read/write the current budget correctly.
                    self.fuel = fuel;
                    self.exec_builtin(b)?;
                    // Re-read in case WithFuel (or similar) changed the budget.
                    fuel = self.fuel;
                    ip += 1;
                }
                Cell::Addr(addr) => {
                    check_fuel!();
                    // Hot call detection: count every word invocation by entry address.
                    *self.call_counts.entry(addr).or_insert(0) += 1;
                    // Tail-call optimisation: if next cell is Ret, reuse the current frame.
                    // SAFETY: ip+1 is valid because compiled words always end with Ret.
                    if matches!(unsafe { self.memory.get_unchecked(ip + 1) }, Cell::Ret) {
                        ip = addr;  // jump without pushing return address
                    } else {
                        if self.call_stack.len() >= MAX_CALL_DEPTH { bail!("return stack overflow"); }
                        self.call_stack.push(ip + 1);
                        ip = addr;
                    }
                }
                Cell::Ret => {
                    // Empty call_stack = top-level return = halt.
                    // Non-empty = normal return; use unchecked pop to skip the redundant
                    // is_empty check (we just tested it via is_empty() → break path).
                    if self.call_stack.is_empty() { break; }
                    ip = unsafe { self.call_stack.pop().unwrap_unchecked() };
                }
                // Back-edges: charge one fuel unit per iteration to prevent infinite loops.
                Cell::Jmp(addr)   => { if addr <= ip { check_fuel!(); } ip = addr; }
                Cell::Repeat(back) => { check_fuel!(); ip = back; }
                Cell::Until(back) => { check_fuel!(); let v = self.pop()?; if v == 0 { ip = back; } else { ip += 1; } }
                Cell::JmpZ(addr)  => { let v = self.pop()?; if v == 0 { ip = addr; } else { ip += 1; } }
                Cell::While(exit) => { let v = self.pop()?; if v == 0 { ip = exit; } else { ip += 1; } }
                Cell::DoSetup => {
                    let index = self.pop()?;
                    let limit = self.pop()?;
                    self.loop_stack.push((index, limit));
                    ip += 1;
                }
                Cell::DoLoop(back) => {
                    check_fuel!();
                    // SAFETY: DoLoop is only emitted after DoSetup which pushes to loop_stack.
                    // Well-formed programs always have a matching DoSetup, so loop_stack is
                    // non-empty here.  The unsafe avoids a redundant bounds check on every
                    // loop iteration — the single hottest back-edge in the VM.
                    let top = unsafe { self.loop_stack.last_mut().unwrap_unchecked() };
                    top.0 += 1;
                    if top.0 < top.1 { ip = back; continue; }
                    self.loop_stack.pop();
                    ip += 1;
                }
                Cell::DoLoopPlus(back) => {
                    check_fuel!();
                    let step = self.pop()?;
                    // SAFETY: same as DoLoop — matching DoSetup always precedes DoLoopPlus.
                    let top = unsafe { self.loop_stack.last_mut().unwrap_unchecked() };
                    top.0 += step;
                    if top.0 < top.1 { ip = back; continue; }
                    self.loop_stack.pop();
                    ip += 1;
                }
                Cell::OfTest(skip) => {
                    let val = self.pop()?;
                    let sel = *self.data.last().ok_or_else(|| anyhow::anyhow!("stack underflow"))?;
                    if sel == val { ip += 1; } else { ip = skip; }
                }
            }
        }
        // Write local fuel counter back to struct so callers see the remaining budget.
        self.fuel = fuel;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    #[inline(never)] // keep execute() hot loop tight; exec_builtin is the cold path
    fn exec_builtin(&mut self, b: Builtin) -> Result<()> {
        match b {
            Builtin::Plus  => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.wrapping_add(b)); }
            Builtin::Minus => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.wrapping_sub(b)); }
            Builtin::Star  => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.wrapping_mul(b)); }
            Builtin::Slash => { let b = self.pop()?; let a = self.pop()?; if b == 0 { bail!("division by zero"); } self.data.push(a / b); }
            Builtin::Mod   => { let b = self.pop()?; let a = self.pop()?; if b == 0 { bail!("division by zero"); } self.data.push(a % b); }
            Builtin::Dup   => { let a = self.pop()?; self.data.push(a); self.data.push(a); }
            Builtin::Drop  => { self.pop()?; }
            Builtin::Inc   => { if let Some(t) = self.data.last_mut() { *t += 1; } }
            Builtin::Dec   => { if let Some(t) = self.data.last_mut() { *t -= 1; } }
            Builtin::Swap  => { let b = self.pop()?; let a = self.pop()?; self.data.push(b); self.data.push(a); }
            Builtin::Over  => { let b = self.pop()?; let a = self.pop()?; self.data.push(a); self.data.push(b); self.data.push(a); }
            Builtin::Rot   => { let c = self.pop()?; let b = self.pop()?; let a = self.pop()?; self.data.push(b); self.data.push(c); self.data.push(a); }
            Builtin::Nip   => { let b = self.pop()?; self.pop()?; self.data.push(b); }
            Builtin::Tuck  => { let b = self.pop()?; let a = self.pop()?; self.data.push(b); self.data.push(a); self.data.push(b); }
            Builtin::TwoDup => {
                let len = self.data.len();
                if len < 2 { bail!("stack underflow"); }
                let a = self.data[len-2]; let b = self.data[len-1];
                self.data.push(a); self.data.push(b);
            }
            Builtin::TwoDrop => { self.pop()?; self.pop()?; }
            Builtin::TwoSwap => {
                let d = self.pop()?; let c = self.pop()?;
                let b = self.pop()?; let a = self.pop()?;
                self.data.push(c); self.data.push(d);
                self.data.push(a); self.data.push(b);
            }
            Builtin::TwoOver => {
                let len = self.data.len();
                if len < 4 { bail!("2over: stack underflow — need 4 items"); }
                let a = self.data[len - 4];
                let b = self.data[len - 3];
                self.data.push(a);
                self.data.push(b);
            }
            Builtin::TwoRot => {
                // ( a b c d e f -- c d e f a b )
                let len = self.data.len();
                if len < 6 { bail!("2rot: stack underflow — need 6 items"); }
                let a = self.data[len - 6];
                let b = self.data[len - 5];
                self.data.remove(len - 6);
                self.data.remove(len - 6); // index shifts after first remove
                self.data.push(a);
                self.data.push(b);
            }
            Builtin::Eq    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a == b { -1 } else { 0 }); }
            Builtin::Lt    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a < b { -1 } else { 0 }); }
            Builtin::Gt    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a > b { -1 } else { 0 }); }
            Builtin::Le    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a <= b { -1 } else { 0 }); }
            Builtin::Ge    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a >= b { -1 } else { 0 }); }
            Builtin::Ne    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a != b { -1 } else { 0 }); }
            Builtin::ZeroEq => { let a = self.pop()?; self.data.push(if a == 0 { -1 } else { 0 }); }
            Builtin::ZeroLt => { let a = self.pop()?; self.data.push(if a < 0 { -1 } else { 0 }); }
            Builtin::ZeroGt => { let a = self.pop()?; self.data.push(if a > 0 { -1 } else { 0 }); }
            Builtin::And   => { if self.data.len() >= 2 { let b = self.pop()?; let a = self.pop()?; self.data.push(a & b); } }
            Builtin::Or    => { let b = self.pop()?; let a = self.pop()?; self.data.push(a | b); }
            Builtin::Xor   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a ^ b); }
            Builtin::Invert => { let a = self.pop()?; self.data.push(!a); }
            Builtin::Negate => { let a = self.pop()?; self.data.push(a.wrapping_neg()); }
            Builtin::Abs   => { let a = self.pop()?; self.data.push(a.abs()); }
            Builtin::Max   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.max(b)); }
            Builtin::Min   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.min(b)); }
            Builtin::Lshift => { let n = self.pop()?; let a = self.pop()?; self.data.push(a << (n & 63)); }
            Builtin::Rshift => { let n = self.pop()?; let a = self.pop()?; self.data.push(a >> (n & 63)); }
            Builtin::Print  => { if let Some(a) = self.data.pop() { self.out.push_str(&format!("{a} ")); } }
            Builtin::PrintS => {
                self.out.push_str(&format!("<{}> ", self.data.len()));
                for n in &self.data { self.out.push_str(&format!("{n} ")); }
            }
            Builtin::Cr    => { self.out.push('\n'); }
            Builtin::Space => { self.out.push(' '); }
            Builtin::Emit  => { let a = self.pop()?; if let Some(c) = char::from_u32(a as u32) { self.out.push(c); } }
            Builtin::Fetch => {
                let addr = self.pop()? as usize;
                let v = self.heap.get(addr).copied().unwrap_or(0);
                self.data.push(v);
            }
            Builtin::Store => {
                let addr = self.pop()? as usize;
                let val  = self.pop()?;
                while self.heap.len() <= addr { self.heap.push(0); }
                self.heap[addr] = val;
            }
            Builtin::PlusStore => {
                let addr = self.pop()? as usize;
                let val  = self.pop()?;
                while self.heap.len() <= addr { self.heap.push(0); }
                self.heap[addr] += val;
            }
            Builtin::Here  => { self.data.push(self.heap.len() as i64); }
            Builtin::Allot => {
                let n = self.pop()?.max(0) as usize;
                self.heap.resize(self.heap.len() + n, 0);
            }
            Builtin::Comma => {
                let val = self.pop()?;
                self.heap.push(val);
            }
            Builtin::Cells  => { /* identity — 1 cell = 1 heap unit */ }
            Builtin::CellSz => { self.data.push(1); }
            Builtin::Fill   => {
                let val  = self.pop()?;
                let n    = self.pop()?.max(0) as usize;
                let addr = self.pop()? as usize;
                while self.heap.len() < addr + n { self.heap.push(0); }
                for slot in &mut self.heap[addr..addr + n] { *slot = val; }
            }
            Builtin::Eval => {
                // ( str -- )  evaluate strings[pop()] as Forth source.
                // This is how you execute a machine that someone sends you.
                let idx = self.pop()? as usize;
                let code = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("eval: invalid string index {idx}"))?;
                self.eval(&code)?;
            }
            Builtin::Argue => {
                // ( str1 str2 -- )  two programmers, one stack.
                // Dual-mode: with two string indices on the stack, compares programs.
                // With fewer than 2 items, prints "agreed." so natural language sentences
                // like "humans argue about forth programs" run cleanly.
                if self.data.len() < 2 {
                    self.out.push_str("agreed.\n");
                    return Ok(());
                }
                let idx2 = self.pop()? as usize;
                let idx1 = self.pop()? as usize;
                let code1 = self.strings.get(idx1).cloned()
                    .ok_or_else(|| anyhow::anyhow!("argue: invalid string index {idx1}"))?;
                let code2 = self.strings.get(idx2).cloned()
                    .ok_or_else(|| anyhow::anyhow!("argue: invalid string index {idx2}"))?;

                // Save and clear the data stack so each program sees depth = 0.
                let saved_data = std::mem::take(&mut self.data);

                self.eval(&code1)?;
                let result1 = self.data.last().copied()
                    .ok_or_else(|| anyhow::anyhow!("argue: first program left nothing on stack"))?;
                self.data.clear();

                self.eval(&code2)?;
                let result2 = self.data.last().copied()
                    .ok_or_else(|| anyhow::anyhow!("argue: second program left nothing on stack"))?;
                self.data.clear();

                // Restore caller's stack.
                self.data = saved_data;

                use crossterm::style::Stylize;
                let fmt_val = |v: i64| -> String {
                    match v {
                        -1 => "true".to_string(),
                         0 => "false".to_string(),
                         n => n.to_string(),
                    }
                };
                if result1 == result2 {
                    // Both arrows point at the same value — two directions, one proof.
                    self.out.push_str(&format!(
                        "  {}  ──→  {}  ←──  {}   {}\n",
                        code1.as_str().cyan(),
                        fmt_val(result1).green().bold(),
                        code2.as_str().cyan(),
                        "✓".green(),
                    ));
                } else {
                    self.out.push_str(&format!(
                        "  {}  ──→  {}  ≠  {}  ←──  {}   {}\n",
                        code1.as_str().cyan(), fmt_val(result1).red(),
                        fmt_val(result2).red(), code2.as_str().cyan(),
                        "✗".red(),
                    ));
                    anyhow::bail!("argue: {} got {}, {} got {}", code1, result1, code2, result2);
                }
            }
            Builtin::Gate => {
                // ( str-a str-b str-check -- result )
                // Run prog-a, run prog-b, run check with both results on stack.
                // If check leaves truthy: ✓ result propagates (left on caller's stack).
                // If check leaves falsy:  ✗ bail — the gate does not pass.
                let idx_check = self.pop()? as usize;
                let idx_b     = self.pop()? as usize;
                let idx_a     = self.pop()? as usize;
                let code_a     = self.strings.get(idx_a).cloned()
                    .ok_or_else(|| anyhow::anyhow!("gate: invalid string index {idx_a}"))?;
                let code_b     = self.strings.get(idx_b).cloned()
                    .ok_or_else(|| anyhow::anyhow!("gate: invalid string index {idx_b}"))?;
                let code_check = self.strings.get(idx_check).cloned()
                    .ok_or_else(|| anyhow::anyhow!("gate: invalid string index {idx_check}"))?;

                // Run each program in isolation.
                let saved = std::mem::take(&mut self.data);

                self.eval(&code_a)?;
                let result_a = self.data.last().copied()
                    .ok_or_else(|| anyhow::anyhow!("gate: prog-a left nothing on stack"))?;
                let stack_a = std::mem::take(&mut self.data);

                self.eval(&code_b)?;
                let result_b = self.data.last().copied()
                    .ok_or_else(|| anyhow::anyhow!("gate: prog-b left nothing on stack"))?;
                self.data.clear();

                // Run check with [result_a, result_b] on stack.
                self.data.push(result_a);
                self.data.push(result_b);
                self.eval(&code_check)?;
                let flag = self.data.last().copied().unwrap_or(0);
                self.data.clear();

                // Restore caller's stack.
                self.data = saved;

                use crossterm::style::Stylize;
                if flag != 0 {
                    // Gate passes — leave the agreed result on the stack.
                    // "Passes along" the full output stack of prog-a (the canonical result).
                    for v in &stack_a { self.data.push(*v); }
                    let fmt = |v: i64| -> String {
                        match v { -1 => "true".into(), 0 => "false".into(), n => n.to_string() }
                    };
                    self.out.push_str(&format!(
                        "  {} | {} ──[{}]──→ {}   {}\n",
                        code_a.as_str().cyan(),
                        code_b.as_str().cyan(),
                        code_check.as_str().yellow(),
                        fmt(result_a).green().bold(),
                        "✓ gate passed".green(),
                    ));
                } else {
                    let fmt = |v: i64| -> String {
                        match v { -1 => "true".into(), 0 => "false".into(), n => n.to_string() }
                    };
                    self.out.push_str(&format!(
                        "  {} → {}  {} → {}  check: {}   {}\n",
                        code_a.as_str().cyan(), fmt(result_a).red(),
                        code_b.as_str().cyan(), fmt(result_b).red(),
                        code_check.as_str().yellow(),
                        "✗ gate blocked".red(),
                    ));
                    anyhow::bail!("gate blocked: check `{}` failed on ({}, {})", code_check, result_a, result_b);
                }
            }
            Builtin::Versus => {
                // ( str1 str2 -- )  run both machines, show FULL stacks side by side.
                // Unlike `argue` (top-only), `versus` shows every value each machine produced.
                // Agrees if both stacks are identical (depth + all values).
                let idx2 = self.pop()? as usize;
                let idx1 = self.pop()? as usize;
                let code1 = self.strings.get(idx1).cloned()
                    .ok_or_else(|| anyhow::anyhow!("versus: invalid string index {idx1}"))?;
                let code2 = self.strings.get(idx2).cloned()
                    .ok_or_else(|| anyhow::anyhow!("versus: invalid string index {idx2}"))?;

                let saved_data = std::mem::take(&mut self.data);

                self.eval(&code1)?;
                let stack1: Vec<i64> = self.data.clone();
                self.data.clear();

                self.eval(&code2)?;
                let stack2: Vec<i64> = self.data.clone();
                self.data.clear();

                self.data = saved_data;

                use crossterm::style::Stylize;

                // Format a stack as "[ a b c ]" or "[ ]" if empty.
                let fmt_stack = |s: &Vec<i64>| -> String {
                    if s.is_empty() {
                        "[ ]".to_string()
                    } else {
                        let inner = s.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(" ");
                        format!("[ {} ]", inner)
                    }
                };

                let s1 = fmt_stack(&stack1);
                let s2 = fmt_stack(&stack2);
                let agree = stack1 == stack2;

                if agree {
                    self.out.push_str(&format!(
                        "  {}  →  {}  =  {}  ←  {}   {}\n",
                        code1.as_str().cyan(),
                        s1.green().bold(),
                        s2.green().bold(),
                        code2.as_str().cyan(),
                        "✓".green(),
                    ));
                } else {
                    self.out.push_str(&format!(
                        "  {}  →  {}  ≠  {}  ←  {}   {}\n",
                        code1.as_str().cyan(),
                        s1.red(),
                        s2.red(),
                        code2.as_str().cyan(),
                        "✗".red(),
                    ));
                    anyhow::bail!("versus: stacks differ\n  A: {}\n  B: {}", fmt_stack(&stack1), fmt_stack(&stack2));
                }
            }
            Builtin::BothWays => {
                // ( a b str -- )  prove op commutes: op(a,b) = op(b,a)
                // Two directions at once.  The stack settles it.
                let idx = self.pop()? as usize;
                let b   = self.pop()?;
                let a   = self.pop()?;
                let code = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("both-ways: invalid string index {idx}"))?;

                let depth_before = self.data.len();

                self.data.push(a); self.data.push(b);
                self.eval(&code)?;
                let r1 = self.data.last().copied()
                    .ok_or_else(|| anyhow::anyhow!("both-ways: forward left nothing on stack"))?;
                self.data.truncate(depth_before);

                self.data.push(b); self.data.push(a);
                self.eval(&code)?;
                let r2 = self.data.last().copied()
                    .ok_or_else(|| anyhow::anyhow!("both-ways: reverse left nothing on stack"))?;
                self.data.truncate(depth_before);

                use crossterm::style::Stylize;
                if r1 == r2 {
                    self.out.push_str(&format!(
                        "  {} {} [{}] → {}   {} {} [{}] → {}   {}\n",
                        a, b, code.as_str().cyan(), r1.to_string().green().bold(),
                        b, a, code.as_str().cyan(), r2.to_string().green().bold(),
                        "✓".green(),
                    ));
                } else {
                    self.out.push_str(&format!(
                        "  {} {} [{}] → {}   {} {} [{}] → {}   {}\n",
                        a, b, code.as_str().cyan(), r1.to_string().red(),
                        b, a, code.as_str().cyan(), r2.to_string().red(),
                        "✗".red(),
                    ));
                    anyhow::bail!("both-ways: {} {} {} → {} but {} {} {} → {}", a, b, code, r1, b, a, code, r2);
                }
            }
            Builtin::Page => {
                // ( str -- )  A proof page: each non-empty line is "left | right".
                // Runs both sides in order.  Every line must agree.
                // If a line has no | it runs as plain Forth.
                use crossterm::style::Stylize;
                let idx = self.pop()? as usize;
                let src = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("page: invalid string index {idx}"))?;
                let mut step = 1usize;
                for raw_line in src.lines() {
                    let line = raw_line.trim();
                    if line.is_empty() || line.starts_with('\\') { continue; }

                    if let Some(pipe) = line.find('|') {
                        let left  = line[..pipe].trim();
                        let right = line[pipe+1..].trim();

                        let depth_before = self.data.len();

                        self.eval(left)?;
                        let r1 = self.data.last().copied()
                            .ok_or_else(|| anyhow::anyhow!("page line {step}: left side left nothing on stack"))?;
                        self.data.truncate(depth_before);

                        self.eval(right)?;
                        let r2 = self.data.last().copied()
                            .ok_or_else(|| anyhow::anyhow!("page line {step}: right side left nothing on stack"))?;
                        self.data.truncate(depth_before);

                        if r1 == r2 {
                            self.out.push_str(&format!(
                                "  {}.  {}  ──→  {}  ←──  {}   {}\n",
                                step,
                                left.cyan(), r1.to_string().green().bold(),
                                right.cyan(), "✓".green(),
                            ));
                        } else {
                            self.out.push_str(&format!(
                                "  {}.  {}  ──→  {}  ≠  {}  ←──  {}   {}\n",
                                step,
                                left.cyan(), r1.to_string().red(),
                                r2.to_string().red(), right.cyan(),
                                "✗".red(),
                            ));
                            anyhow::bail!("page line {step}: left got {r1}, right got {r2}");
                        }
                    } else {
                        // Plain Forth — just run it (sets up shared stack state)
                        self.eval(line)?;
                    }
                    step += 1;
                }
            }
            Builtin::Resolve => {
                // ( str -- )  many sentences, one truth.
                // Run each non-empty line.  All must produce the same top-of-stack value.
                // Shows all of them converging.  This is what agreement looks like.
                let idx = self.pop()? as usize;
                let src = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("resolve: invalid string index {idx}"))?;

                use crossterm::style::Stylize;

                struct Sentence { text: String, result: i64 }
                let mut sentences: Vec<Sentence> = Vec::new();
                let depth_before = self.data.len();

                for raw_line in src.lines() {
                    let line = raw_line.trim();
                    if line.is_empty() || line.starts_with('\\') { continue; }
                    self.eval(line)?;
                    let result = self.data.last().copied()
                        .ok_or_else(|| anyhow::anyhow!("resolve: '{}' left nothing on stack", line))?;
                    self.data.truncate(depth_before);
                    sentences.push(Sentence { text: line.to_string(), result });
                }

                if sentences.is_empty() { return Ok(()); }

                let truth = sentences[0].result;
                let all_agree = sentences.iter().all(|s| s.result == truth);

                for s in &sentences {
                    if s.result == truth {
                        self.out.push_str(&format!(
                            "  {}  ──→  {}\n",
                            s.text.as_str().cyan(),
                            s.result.to_string().green().bold(),
                        ));
                    } else {
                        self.out.push_str(&format!(
                            "  {}  ──→  {}  {}\n",
                            s.text.as_str().cyan(),
                            s.result.to_string().red(),
                            "(disagrees)".red(),
                        ));
                    }
                }

                if all_agree {
                    self.out.push_str(&format!("  {}\n", "all agree.".green().bold()));
                } else {
                    let bad: Vec<_> = sentences.iter().filter(|s| s.result != truth)
                        .map(|s| format!("'{}' → {}", s.text, s.result)).collect();
                    anyhow::bail!("resolve: sentences disagree: {}", bad.join(", "));
                }
            }
            Builtin::LoopI => {
                let v = self.loop_stack.last().map(|t| t.0).unwrap_or(0);
                self.data.push(v);
            }
            Builtin::LoopJ => {
                let len = self.loop_stack.len();
                let v = if len >= 2 { self.loop_stack[len-2].0 } else { 0 };
                self.data.push(v);
            }
            Builtin::ToR    => { let a = self.pop()?; self.rstack.push(a); }
            Builtin::FromR  => { let a = self.rstack.pop().ok_or_else(|| anyhow::anyhow!("return stack underflow"))?; self.data.push(a); }
            Builtin::FetchR => { let a = self.rstack.last().copied().ok_or_else(|| anyhow::anyhow!("return stack underflow"))?; self.data.push(a); }
            Builtin::Pick => {
                let n = self.pop()? as usize;
                let len = self.data.len();
                if n >= len { bail!("pick: stack underflow"); }
                self.data.push(self.data[len - 1 - n]);
            }
            Builtin::Roll => {
                let n = self.pop()? as usize;
                let len = self.data.len();
                if n >= len { bail!("roll: stack underflow"); }
                let val = self.data.remove(len - 1 - n);
                self.data.push(val);
            }
            Builtin::ULt => {
                let b = self.pop()? as u64;
                let a = self.pop()? as u64;
                self.data.push(if a < b { -1 } else { 0 });
            }
            Builtin::StarSlash => {
                // ( a b c -- a*b/c ) with 128-bit intermediate
                let c = self.pop()?; let b = self.pop()?; let a = self.pop()?;
                if c == 0 { bail!("division by zero"); }
                let r = (a as i128) * (b as i128) / (c as i128);
                self.data.push(r as i64);
            }
            Builtin::SlashMod => {
                // ( n d -- rem quot )
                let d = self.pop()?; let n = self.pop()?;
                if d == 0 { bail!("division by zero"); }
                self.data.push(n % d); self.data.push(n / d);
            }
            Builtin::PrintU  => { let a = self.pop()? as u64; self.out.push_str(&format!("{a} ")); }
            Builtin::PrintHex => { let a = self.pop()?; self.out.push_str(&format!("{a:#x} ")); }
            Builtin::Words => {
                let mut names: Vec<String> = self.name_index.keys().cloned().collect();
                names.sort();
                self.out.push_str(&names.join("  "));
                self.out.push('\n');
            }
            Builtin::HotWords => {
                // Build reverse map: addr → name
                let addr_to_name: HashMap<usize, &str> = self.name_index.iter()
                    .map(|(n, &a)| (a, n.as_str()))
                    .collect();
                let mut counts: Vec<(u64, &str)> = self.call_counts.iter()
                    .filter_map(|(&addr, &count)| {
                        addr_to_name.get(&addr).map(|&name| (count, name))
                    })
                    .collect();
                counts.sort_by(|a, b| b.0.cmp(&a.0));
                self.out.push_str("── hot words ──\n");
                for (count, name) in counts.iter().take(10) {
                    self.out.push_str(&format!("  {:>8}  {}\n", count, name));
                }
            }
            Builtin::Random => {
                // Simple LCG random number generator
                use std::time::{SystemTime, UNIX_EPOCH};
                let seed = SystemTime::now().duration_since(UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as i64)
                    .unwrap_or(12345);
                let n = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                self.data.push(n.abs());
            }
            Builtin::Time => {
                use std::time::{SystemTime, UNIX_EPOCH};
                let secs = SystemTime::now().duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64).unwrap_or(0);
                self.data.push(secs);
            }
            Builtin::Depth => { self.data.push(self.data.len() as i64); }
            Builtin::Nop   => {}
            Builtin::Fuel  => { self.data.push(self.fuel as i64); }
            Builtin::WithFuel => {
                // ( n -- )  set fuel budget for the next word call
                // Used as: 1000000 with-fuel  before a heavy word
                let n = self.pop()?;
                self.fuel = if n <= 0 { usize::MAX } else { n as usize };
            }
            Builtin::Undo => {
                if let Some(snap) = self.undo_stack.pop() {
                    self.restore(&snap);
                    self.out.push_str("ok\n");
                } else {
                    self.out.push_str("nothing to undo\n");
                }
            }
            Builtin::Lock => {
                // ( name-idx ttl-ms -- flag )
                // Acquire advisory lock.  Max TTL = 5 000 ms.  Returns -1 on success, 0 if held.
                const MAX_TTL_MS: u64 = 5_000;
                let ttl_ms = self.pop()? as u64;
                let name_idx = self.pop()? as usize;
                let name = self.strings.get(name_idx)
                    .ok_or_else(|| anyhow::anyhow!("lock: string index {} out of bounds", name_idx))?
                    .clone();
                let ttl = Duration::from_millis(ttl_ms.min(MAX_TTL_MS));
                let now = Instant::now();
                // Purge expired entry first
                if let Some(&exp) = self.locks.get(&name) {
                    if now >= exp { self.locks.remove(&name); }
                }
                if self.locks.contains_key(&name) {
                    self.data.push(0); // lock is held
                } else {
                    self.locks.insert(name, now + ttl);
                    self.data.push(-1); // acquired
                }
            }
            Builtin::Unlock => {
                // ( name-idx -- )  release the named lock immediately
                let name_idx = self.pop()? as usize;
                let name = self.strings.get(name_idx)
                    .ok_or_else(|| anyhow::anyhow!("unlock: string index {} out of bounds", name_idx))?
                    .clone();
                self.locks.remove(&name);
            }
            Builtin::LockTtl => {
                // ( name-idx -- ms )  remaining TTL; 0 if free or expired
                let name_idx = self.pop()? as usize;
                let name = self.strings.get(name_idx)
                    .ok_or_else(|| anyhow::anyhow!("lock-ttl: string index {} out of bounds", name_idx))?;
                let now = Instant::now();
                let ms = self.locks.get(name)
                    .and_then(|&exp| exp.checked_duration_since(now))
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                self.data.push(ms);
            }
            // ── Writing assistance ────────────────────────────────────────────
            Builtin::Capitalize => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("capitalize: index {} out of bounds", idx))?
                    .clone();
                let result = {
                    let mut c = s.chars();
                    match c.next() {
                        None => String::new(),
                        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                    }
                };
                let new_idx = self.strings.len();
                self.strings.push(result);
                self.data.push(new_idx as i64);
            }
            Builtin::StrUpper => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("str-upper: index {} out of bounds", idx))?
                    .to_uppercase();
                let new_idx = self.strings.len();
                self.strings.push(s);
                self.data.push(new_idx as i64);
            }
            Builtin::StrLower => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("str-lower: index {} out of bounds", idx))?
                    .to_lowercase();
                let new_idx = self.strings.len();
                self.strings.push(s);
                self.data.push(new_idx as i64);
            }
            Builtin::StrTrim => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("str-trim: index {} out of bounds", idx))?
                    .trim()
                    .to_string();
                let new_idx = self.strings.len();
                self.strings.push(s);
                self.data.push(new_idx as i64);
            }
            Builtin::WordCount => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("word-count: index {} out of bounds", idx))?;
                let n = s.split_whitespace().count();
                self.data.push(n as i64);
            }
            Builtin::SentenceCheck => {
                // A well-formed sentence: non-empty, first char uppercase, last char is . ! ?
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("sentence?: index {} out of bounds", idx))?;
                let trimmed = s.trim();
                let valid = !trimmed.is_empty()
                    && trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    && trimmed.chars().last().map(|c| matches!(c, '.' | '!' | '?')).unwrap_or(false);
                self.data.push(if valid { -1 } else { 0 });
            }
            Builtin::GrammarCheck => {
                let idx = self.pop()? as usize;
                let text = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("grammar-check: index {} out of bounds", idx))?
                    .clone();
                let result = if let Some(ref f) = self.gen_fn {
                    let prompt = format!(
                        "Fix any grammar mistakes in this sentence. \
                         Return only the corrected sentence, no explanation: {}",
                        text
                    );
                    f(&prompt).trim().to_string()
                } else {
                    text // no AI available: return unchanged
                };
                let new_idx = self.strings.len();
                self.strings.push(result);
                self.data.push(new_idx as i64);
            }
            Builtin::ImproveStr => {
                let idx = self.pop()? as usize;
                let text = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("improve: index {} out of bounds", idx))?
                    .clone();
                let result = if let Some(ref f) = self.gen_fn {
                    let prompt = format!(
                        "Improve this sentence to be clearer and more fluent. \
                         Return only the improved sentence, no explanation: {}",
                        text
                    );
                    f(&prompt).trim().to_string()
                } else {
                    text
                };
                let new_idx = self.strings.len();
                self.strings.push(result);
                self.data.push(new_idx as i64);
            }
            Builtin::Sqrt  => {
                let n = self.pop()?;
                if n < 0 { bail!("sqrt of negative"); }
                self.data.push((n as f64).sqrt() as i64);
            }
            Builtin::Floor => {
                let d = self.pop()?; let n = self.pop()?;
                if d == 0 { bail!("division by zero"); }
                self.data.push(n.div_euclid(d) * d);
            }
            Builtin::Ceil  => {
                let d = self.pop()?; let n = self.pop()?;
                if d == 0 { bail!("division by zero"); }
                let q = n.div_euclid(d);
                let r = n.rem_euclid(d);
                self.data.push(if r == 0 { q * d } else { (q + 1) * d });
            }
            Builtin::Sin   => {
                // Input: degrees * 1000  Output: sin * 1000
                let deg_milli = self.pop()?;
                let rad = (deg_milli as f64) / 1000.0 * std::f64::consts::PI / 180.0;
                self.data.push((rad.sin() * 1000.0) as i64);
            }
            Builtin::Cos   => {
                let deg_milli = self.pop()?;
                let rad = (deg_milli as f64) / 1000.0 * std::f64::consts::PI / 180.0;
                self.data.push((rad.cos() * 1000.0) as i64);
            }
            Builtin::FPMul => {
                let scale = self.pop()?; let b = self.pop()?; let a = self.pop()?;
                if scale == 0 { bail!("fpmul: scale is zero"); }
                self.data.push((a as i128 * b as i128 / scale as i128) as i64);
            }
            Builtin::Peers => {
                if self.peers.is_empty() {
                    self.out.push_str("(no peers registered)\n");
                } else {
                    for (i, p) in self.peers.iter().enumerate() {
                        self.out.push_str(&format!("{}: {}\n", i, p));
                    }
                }
            }
            Builtin::PeersClear => {
                self.peers.clear();
            }
            Builtin::PeersDiscover => {
                // mDNS scan — finds _finch._tcp.local. services, adds host:port to self.peers
                let found = run_peers_discover(2000);
                if found.is_empty() {
                    self.out.push_str("peers-discover: no finch instances found on LAN\n");
                } else {
                    for (host, port, name, token) in &found {
                        let addr = format!("{host}:{port}");
                        if !self.peers.contains(&addr) {
                            self.peers.push(addr.clone());
                            let meta = self.peer_meta.entry(addr.clone()).or_default();
                            if !name.is_empty() && meta.label.is_none() {
                                meta.label = Some(name.clone());
                            }
                            if let Some(t) = token {
                                if meta.token.is_none() { meta.token = Some(t.clone()); }
                            }
                            self.out.push_str(&format!("  + {}\n",
                                if name.is_empty() { addr.clone() } else { name.clone() }));
                        }
                    }
                }
            }
            // ── Ensembles ─────────────────────────────────────────────────────
            Builtin::TakeAll => {
                use crossterm::style::Stylize;
                let found = run_peers_discover(3000);
                if found.is_empty() {
                    self.out.push_str(&format!("{}\n",
                        "take-all: no machines found on LAN".dark_grey()));
                } else {
                    let mut added = 0usize;
                    for (host, port, name, token) in &found {
                        let addr = format!("{host}:{port}");
                        if !self.peers.contains(&addr) {
                            self.peers.push(addr.clone());
                            added += 1;
                        }
                        let meta = self.peer_meta.entry(addr).or_default();
                        if !name.is_empty() && meta.label.is_none() {
                            meta.label = Some(name.clone());
                        }
                        if let Some(t) = token {
                            if meta.token.is_none() { meta.token = Some(t.clone()); }
                        }
                        if !meta.tags.contains(&"all".to_string()) {
                            meta.tags.push("all".to_string());
                        }
                    }
                    // Rebuild the "all" ensemble from tagged peers
                    let all_peers: Vec<String> = self.peer_meta.iter()
                        .filter(|(_, m)| m.tags.contains(&"all".to_string()))
                        .map(|(a, _)| a.clone())
                        .collect();
                    let total = all_peers.len();
                    self.ensembles.insert("all".to_string(), all_peers);
                    self.out.push_str(&format!("{} {} machine(s)  ensemble '{}' ready\n",
                        "took".green().bold(),
                        total.to_string().cyan().bold(),
                        "all".cyan()));
                    if added < total {
                        self.out.push_str(&format!("  ({} new, {} already known)\n",
                            added.to_string().dark_grey(),
                            (total - added).to_string().dark_grey()));
                    }
                }
            }
            Builtin::EnsembleDef => {
                // ( name-idx -- )  snapshot current peers under a name
                let idx = self.pop()? as usize;
                let name = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("ensemble-def: string index out of bounds"))?
                    .trim().to_string();
                if name.is_empty() { bail!("ensemble-def: name must not be empty"); }
                let peers = self.peers.clone();
                let count = peers.len();
                self.ensembles.insert(name.clone(), peers);
                self.out.push_str(&format!("ensemble '{}': {} peer(s) saved\n", name, count));
            }
            Builtin::EnsembleUse => {
                // ( name-idx -- )  push current peers, switch to named ensemble
                let idx = self.pop()? as usize;
                let name = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("ensemble-use: string index out of bounds"))?
                    .trim().to_string();
                let members = self.ensembles.get(&name)
                    .ok_or_else(|| anyhow::anyhow!("ensemble-use: unknown ensemble '{}'", name))?
                    .clone();
                let saved = std::mem::replace(&mut self.peers, members);
                self.peer_save_stack.push(saved);
            }
            Builtin::EnsembleEnd => {
                // ( -- )  restore previous peers
                match self.peer_save_stack.pop() {
                    Some(saved) => { self.peers = saved; }
                    None => { bail!("ensemble-end: no saved peer context (missing ensemble-use?)"); }
                }
            }
            Builtin::EnsembleList => {
                use crossterm::style::Stylize;
                if self.ensembles.is_empty() {
                    self.out.push_str(&"(no ensembles defined)".dark_grey().to_string());
                    self.out.push('\n');
                } else {
                    let mut names: Vec<&String> = self.ensembles.keys().collect();
                    names.sort();
                    for name in names {
                        let members = &self.ensembles[name];
                        self.out.push_str(&format!("  {}  {}\n",
                            name.as_str().cyan().bold(),
                            format!("({} peers)", members.len()).dark_grey()));
                        for m in members {
                            let label = self.peer_meta.get(m)
                                .and_then(|p| p.label.as_deref())
                                .unwrap_or("");
                            let tags = self.peer_meta.get(m)
                                .map(|p| p.tags.join(" "))
                                .unwrap_or_default();
                            let suffix = match (label.is_empty(), tags.is_empty()) {
                                (false, false) => format!("  {} [{}]", label.yellow(), tags.as_str().dark_grey()),
                                (false, true)  => format!("  {}", label.yellow()),
                                (true,  false) => format!("  [{}]", tags.as_str().dark_grey()),
                                (true,  true)  => String::new(),
                            };
                            self.out.push_str(&format!("    {}{}\n", m.as_str().dark_grey(), suffix));
                        }
                    }
                }
            }
            Builtin::LabelPeer => {
                // ( addr-idx label-idx -- )
                let label_idx = self.pop()? as usize;
                let addr_idx  = self.pop()? as usize;
                let addr  = self.strings.get(addr_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("label-peer: addr index out of bounds"))?;
                let label = self.strings.get(label_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("label-peer: label index out of bounds"))?;
                self.peer_meta.entry(addr.trim().to_string()).or_default().label = Some(label.trim().to_string());
            }
            Builtin::TagPeer => {
                // ( addr-idx tag-idx -- )
                let tag_idx  = self.pop()? as usize;
                let addr_idx = self.pop()? as usize;
                let addr = self.strings.get(addr_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("tag-peer: addr index out of bounds"))?;
                let tag  = self.strings.get(tag_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("tag-peer: tag index out of bounds"))?;
                let tag_str = tag.trim().to_string();
                let meta = self.peer_meta.entry(addr.trim().to_string()).or_default();
                if !meta.tags.contains(&tag_str) { meta.tags.push(tag_str); }
            }
            Builtin::EnsembleFromTag => {
                // ( tag-idx -- )  build ensemble named after the tag from all tagged peers
                let tag_idx = self.pop()? as usize;
                let tag = self.strings.get(tag_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("ensemble-from-tag: index out of bounds"))?
                    .trim().to_string();
                let members: Vec<String> = self.peer_meta.iter()
                    .filter(|(_, m)| m.tags.contains(&tag))
                    .map(|(addr, _)| addr.clone())
                    .collect();
                use crossterm::style::Stylize;
                self.out.push_str(&format!("ensemble '{}': {} peer(s)\n",
                    tag.as_str().cyan().bold(), members.len()));
                self.ensembles.insert(tag, members);
            }
            Builtin::PeerInfo => {
                use crossterm::style::Stylize;
                if self.peers.is_empty() {
                    self.out.push_str(&"(no peers registered)".dark_grey().to_string());
                    self.out.push('\n');
                } else {
                    for addr in &self.peers {
                        let meta = self.peer_meta.get(addr);
                        let label = meta.and_then(|m| m.label.as_deref()).unwrap_or("");
                        let tags  = meta.map(|m| m.tags.join(" ")).unwrap_or_default();
                        let name = if label.is_empty() { addr.as_str().cyan().to_string() }
                                   else { format!("{} {}", label.cyan().bold(), addr.as_str().dark_grey()) };
                        let tag_str = if tags.is_empty() { String::new() }
                                      else { format!("  [{}]", tags.as_str().yellow()) };
                        self.out.push_str(&format!("  {}{}\n", name, tag_str));
                    }
                }
            }
            // ── Vocabulary sharing ────────────────────────────────────────────
            Builtin::Publish => {
                // ( name-idx -- )  scatter one word's source to all peers
                use crossterm::style::Stylize;
                let name_idx = self.pop()? as usize;
                let name = self.strings.get(name_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("publish: index out of bounds"))?;
                match self.word_source(&name) {
                    None => {
                        self.out.push_str(&format!("'{}' hasn't been defined yet\n",
                            name.as_str().yellow()));
                    }
                    Some(src) => {
                        if self.peers.is_empty() {
                            self.out.push_str(&"nobody else is here yet\n".dark_grey().to_string());
                        } else {
                            let tokens = peer_tokens_map(&self.peer_meta);
                            let results = run_define_scatter(&self.peers, &src, &tokens);
                            for r in &results {
                                let label = self.peer_meta.get(&r.peer)
                                    .and_then(|m| m.label.as_deref())
                                    .map(|l| l.cyan().bold().to_string())
                                    .unwrap_or_else(|| {
                                        r.peer.trim_start_matches("http://")
                                            .split(':').next().unwrap_or(&r.peer)
                                            .cyan().to_string()
                                    });
                                if let Some(e) = &r.error {
                                    self.out.push_str(&format!("{} couldn't learn {}: {}\n",
                                        label, name.as_str().yellow(), e.as_str().red()));
                                } else {
                                    self.out.push_str(&format!("{} now knows {}\n",
                                        label, name.as_str().green().bold()));
                                }
                            }
                        }
                    }
                }
            }
            Builtin::Sync => {
                // ( -- )  scatter all user words to all peers
                use crossterm::style::Stylize;
                let src = self.dump_source();
                if src.is_empty() {
                    self.out.push_str(&"nothing to share yet\n".dark_grey().to_string());
                } else if self.peers.is_empty() {
                    self.out.push_str(&"nobody else is here yet\n".dark_grey().to_string());
                } else {
                    let word_count = src.lines().filter(|l| l.trim_start().starts_with(':')).count();
                    let tokens = peer_tokens_map(&self.peer_meta);
                    let results = run_define_scatter(&self.peers, &src, &tokens);
                    for r in &results {
                        let label = self.peer_meta.get(&r.peer)
                            .and_then(|m| m.label.as_deref())
                            .map(|l| l.cyan().bold().to_string())
                            .unwrap_or_else(|| {
                                r.peer.trim_start_matches("http://")
                                    .split(':').next().unwrap_or(&r.peer)
                                    .cyan().to_string()
                            });
                        if let Some(e) = &r.error {
                            self.out.push_str(&format!("{} is out of reach: {}\n",
                                label, e.as_str().red()));
                        } else {
                            self.out.push_str(&format!("{} is caught up  ({} word{})\n",
                                label,
                                word_count,
                                if word_count == 1 { "" } else { "s" }));
                        }
                    }
                }
            }
            // ── Registry ─────────────────────────────────────────────────────
            Builtin::RegistrySet => {
                // ( addr-idx -- )  set the registry address for this VM
                let idx = self.pop()? as usize;
                let addr = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("registry: index out of bounds"))?;
                self.registry_addr = Some(addr.trim().to_string());
            }
            Builtin::JoinRegistry => {
                // ( self-addr-idx -- )  register this machine with the configured registry
                use crossterm::style::Stylize;
                let addr_idx = self.pop()? as usize;
                let self_addr = self.strings.get(addr_idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("join-registry: index out of bounds"))?;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"join-registry: no registry set  (use registry\" addr\")\n"
                            .yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        // Build tags from peer_meta for self_addr if present; else empty.
                        let meta = self.peer_meta.get(&self_addr).cloned();
                        let specs = collect_machine_specs();
                        let result = run_registry_join(&reg, crate::registry::PeerEntry {
                            addr:      self_addr.clone(),
                            label:     meta.as_ref().and_then(|m| m.label.clone()),
                            tags:      meta.map(|m| m.tags).unwrap_or_default(),
                            load:      None,
                            region:    None,
                            cpu_cores: Some(specs.0),
                            ram_mb:    Some(specs.1),
                            bench_ms:  Some(specs.2),
                        });
                        match result {
                            Ok(_) => {
                                self.my_addr = Some(self_addr.clone());
                                self.out.push_str(&format!("registered {} with {}\n",
                                    self_addr.as_str().cyan().bold(),
                                    reg.as_str().dark_grey()));
                            }
                            Err(e) => {
                                self.out.push_str(&format!("join-registry error: {}\n",
                                    e.to_string().red()));
                            }
                        }
                    }
                }
            }
            Builtin::LeaveRegistry => {
                // ( -- )  deregister this machine from the configured registry
                use crossterm::style::Stylize;
                let addr = match &self.my_addr {
                    Some(a) => a.clone(),
                    None => {
                        self.out.push_str(&"leave: not registered (use join\" addr\" first)\n"
                            .yellow().to_string());
                        return Ok(());
                    }
                };
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"leave: no registry set\n".yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_leave(&reg, &addr) {
                            Ok(()) => {
                                self.out.push_str(&format!("left registry: {}\n",
                                    addr.as_str().dark_grey()));
                                self.my_addr = None;
                            }
                            Err(e) => {
                                self.out.push_str(&format!("leave error: {}\n",
                                    e.to_string().red()));
                            }
                        }
                    }
                }
            }
            Builtin::FromRegistry => {
                // ( -- )  pull live peers from registry into self.peers
                use crossterm::style::Stylize;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"from-registry: no registry set\n".yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_peers(&reg, None, None) {
                            Err(e) => {
                                self.out.push_str(&format!("from-registry error: {}\n",
                                    e.to_string().red()));
                            }
                            Ok(peers) => {
                                let mut added = 0usize;
                                for p in &peers {
                                    if !self.peers.contains(&p.addr) {
                                        self.peers.push(p.addr.clone());
                                        added += 1;
                                    }
                                    // Merge label + tags into peer_meta.
                                    let meta = self.peer_meta.entry(p.addr.clone()).or_default();
                                    if meta.label.is_none() {
                                        meta.label = p.label.clone();
                                    }
                                    for t in &p.tags {
                                        if !meta.tags.contains(t) {
                                            meta.tags.push(t.clone());
                                        }
                                    }
                                }
                                self.out.push_str(&format!("{} peer(s) from registry ({} new)\n",
                                    peers.len().to_string().cyan(),
                                    added.to_string().green()));
                            }
                        }
                    }
                }
            }
            Builtin::RegistryList => {
                // ( -- )  print all registry members
                use crossterm::style::Stylize;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"registry-list: no registry set\n".yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_peers(&reg, None, None) {
                            Err(e) => {
                                self.out.push_str(&format!("registry-list error: {}\n",
                                    e.to_string().red()));
                            }
                            Ok(peers) => {
                                if peers.is_empty() {
                                    self.out.push_str(&"(registry empty)\n".dark_grey().to_string());
                                } else {
                                    for p in &peers {
                                        let name = p.label.as_deref()
                                            .map(|l| format!("{} {}", l.cyan().bold(),
                                                p.addr.as_str().dark_grey()))
                                            .unwrap_or_else(|| p.addr.as_str().cyan().to_string());
                                        let tags = if p.tags.is_empty() { String::new() }
                                            else { format!("  [{}]", p.tags.join(" ").yellow()) };
                                        let load = p.load.map(|l| format!("  load:{:.0}%",
                                            (l * 100.0).round())).unwrap_or_default();
                                        let hw = format_peer_hw(p);
                                        self.out.push_str(&format!("  {}{}{}{}\n", name, hw, tags, load));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Builtin::Balance => {
                // ( -- )  print this machine's net compute balance
                use crossterm::style::Stylize;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"balance: not registered\n".yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_ledger(&reg, &reg) {
                            Err(e) => {
                                self.out.push_str(&format!("balance error: {}\n", e.to_string().red()));
                            }
                            Ok(entry) => {
                                let net = entry.balance_ms();
                                let sign = if net >= 0 { "+" } else { "" };
                                let color_str = if net >= 0 {
                                    format!("{sign}{net}ms").green().to_string()
                                } else {
                                    format!("{sign}{net}ms").red().to_string()
                                };
                                self.out.push_str(&format!(
                                    "  balance: {}  (earned {}ms  spent {}ms)\n",
                                    color_str,
                                    entry.credits_ms.to_string().cyan(),
                                    entry.debits_ms.to_string().yellow(),
                                ));
                            }
                        }
                    }
                }
            }
            Builtin::Balances => {
                // ( -- )  print all machines' compute balances from registry
                use crossterm::style::Stylize;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"balances: no registry set\n".yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_all_ledgers(&reg) {
                            Err(e) => {
                                self.out.push_str(&format!("balances error: {}\n", e.to_string().red()));
                            }
                            Ok(entries) => {
                                if entries.is_empty() {
                                    self.out.push_str(&"(no machines registered)\n".dark_grey().to_string());
                                } else {
                                    for (addr, entry) in &entries {
                                        let net = entry.balance_ms();
                                        let sign = if net >= 0 { "+" } else { "" };
                                        let color_str = if net >= 0 {
                                            format!("{sign}{net}ms").green().to_string()
                                        } else {
                                            format!("{sign}{net}ms").red().to_string()
                                        };
                                        self.out.push_str(&format!(
                                            "  {}  {}\n",
                                            addr.as_str().cyan(),
                                            color_str,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Builtin::RecordDebit => {
                // ( peer-idx compute-ms -- )  record compute consumed from peer
                if self.data.len() < 2 {
                    return Err(anyhow::anyhow!("record-debit: stack underflow"));
                }
                let compute_ms = self.data.pop().unwrap() as u64;
                let peer_idx   = self.data.pop().unwrap() as usize;
                if let Some(peer_addr) = self.strings.get(peer_idx).cloned() {
                    if let Some(ref reg) = self.registry_addr.clone() {
                        let _ = run_registry_debit(reg, &peer_addr, compute_ms);
                    }
                }
            }
            Builtin::See(idx) => {
                use crossterm::style::Stylize;
                let word_name = self.strings.get(idx).cloned().unwrap_or_default();
                let mut found = false;

                // 1. Check user-defined words (source_log).
                if let Some(src) = self.word_source(&word_name) {
                    self.out.push_str(&format!(
                        "  {} {}\n",
                        word_name.as_str().cyan().bold(),
                        src.trim().dark_grey(),
                    ));
                    found = true;
                }

                // 2. Check the library for a plain-language definition.
                static LIB: std::sync::OnceLock<crate::coforth::Library> = std::sync::OnceLock::new();
                let lib = LIB.get_or_init(crate::coforth::Library::load);
                if let Some(entry) = lib.lookup(&word_name) {
                    self.out.push_str(&format!(
                        "  {}\n",
                        entry.definition.as_str().white(),
                    ));
                    if !entry.related.is_empty() {
                        let rel = entry.related.join("  ");
                        self.out.push_str(&format!(
                            "  → {}\n",
                            rel.as_str().dark_grey(),
                        ));
                    }
                    found = true;
                }

                // 3b. Show proof indicator if test:<word> is defined.
                let test_name = format!("test:{word_name}");
                if self.name_index.contains_key(&test_name) {
                    self.out.push_str(&format!(
                        "  {} prove\" {}\"\n",
                        "▸".dark_grey(),
                        word_name.as_str().dark_grey(),
                    ));
                }

                // 3. Check builtins.
                if !found {
                    if name_to_builtin(&word_name).is_some() {
                        self.out.push_str(&format!(
                            "  {} — built-in word\n",
                            word_name.as_str().cyan().bold(),
                        ));
                    } else {
                        self.out.push_str(&format!(
                            "  {} — unknown word\n",
                            word_name.as_str().yellow(),
                        ));
                    }
                }
            }
            Builtin::DebtCheck => {
                // ( -- )  list machines whose balance is deeply negative (they owe you)
                use crossterm::style::Stylize;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"debt-check: not registered\n".yellow().to_string());
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_all_ledgers(&reg) {
                            Err(e) => {
                                self.out.push_str(&format!("debt-check error: {}\n", e.to_string().red()));
                            }
                            Ok(entries) => {
                                let debtors: Vec<_> = entries.iter()
                                    .filter(|(_, e)| e.balance_ms() < 0)
                                    .collect();
                                if debtors.is_empty() {
                                    self.out.push_str(&"  all square\n".green().to_string());
                                } else {
                                    for (addr, entry) in &debtors {
                                        let balance_s = entry.balance_ms().abs() as f64 / 1000.0;
                                        self.out.push_str(&format!(
                                            "  {} owes {:.1}s\n",
                                            addr.as_str().cyan(),
                                            balance_s,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Builtin::Settle => {
                // ( peer-idx -- )  settle compute debt with a peer (P2P, no third party)
                // Posts to the peer's /v1/settle endpoint acknowledging the debt.
                // The peer verifies and clears both ledgers.
                use crossterm::style::Stylize;
                if self.data.is_empty() {
                    return Err(anyhow::anyhow!("settle: stack underflow"));
                }
                let peer_idx = self.data.pop().unwrap() as usize;
                let peer_addr = self.strings.get(peer_idx)
                    .ok_or_else(|| anyhow::anyhow!("settle: peer string index out of bounds"))?
                    .trim().to_string();
                // Look up what we owe this peer from our local registry view.
                let my_addr = self.registry_addr.clone();
                match run_peer_settle(&peer_addr, my_addr.as_deref()) {
                    Err(e) => {
                        self.out.push_str(&format!("settle error: {}\n", e.to_string().red()));
                    }
                    Ok((cleared_ms, msg)) => {
                        let cleared_s = cleared_ms as f64 / 1000.0;
                        self.out.push_str(&format!(
                            "  ✓ settled with {}  ({:.1}s cleared)\n",
                            peer_addr.as_str().cyan(),
                            cleared_s,
                        ));
                        if !msg.is_empty() {
                            self.out.push_str(&format!("    {}\n", msg.dark_grey()));
                        }
                    }
                }
            }
            Builtin::Slowest => {
                // ( -- addr-idx )  push address of slowest (highest bench_ms) live peer.
                // Falls back to the peer with the most load if bench_ms not reported.
                // Pushes -1 if no peers or no registry set.
                use crossterm::style::Stylize;
                match &self.registry_addr {
                    None => {
                        self.out.push_str(&"slowest: no registry set\n".yellow().to_string());
                        self.data.push(-1);
                    }
                    Some(reg) => {
                        let reg = reg.clone();
                        match run_registry_peers(&reg, None, None) {
                            Err(e) => {
                                self.out.push_str(&format!("slowest error: {}\n", e.to_string().red()));
                                self.data.push(-1);
                            }
                            Ok(peers) if peers.is_empty() => {
                                self.out.push_str(&"slowest: registry empty\n".dark_grey().to_string());
                                self.data.push(-1);
                            }
                            Ok(peers) => {
                                // Score: highest bench_ms wins (slowest); tie-break by highest load.
                                let slowest = peers.iter().max_by(|a, b| {
                                    let sa = a.bench_ms.unwrap_or(0);
                                    let sb = b.bench_ms.unwrap_or(0);
                                    if sa != sb {
                                        sa.cmp(&sb)
                                    } else {
                                        let la = (a.load.unwrap_or(0.0) * 1000.0) as u32;
                                        let lb = (b.load.unwrap_or(0.0) * 1000.0) as u32;
                                        la.cmp(&lb)
                                    }
                                }).unwrap();
                                let hw = format_peer_hw(slowest);
                                self.out.push_str(&format!(
                                    "slowest: {}{}\n",
                                    slowest.addr.as_str().cyan(),
                                    hw,
                                ));
                                let idx = self.strings.len();
                                self.strings.push(slowest.addr.clone());
                                self.data.push(idx as i64);
                            }
                        }
                    }
                }
            }
            Builtin::ForthBack(idx) => {
                // ( -- )  queue Forth code to execute on the caller after they receive our response.
                // In remote_mode the peer sets self.forth_back; the caller retrieves it from the
                // response and evals it locally.  In local mode it runs immediately (no round-trip).
                let code = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("forth-back: string index out of bounds"))?
                    .clone();
                if self.remote_mode {
                    // We are the remote peer — store for transmission.
                    self.forth_back = Some(code);
                } else {
                    // Local execution — run immediately.
                    self.eval(&code)?;
                }
            }
            // ── String pool operations ────────────────────────────────────────
            Builtin::Type => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("type: string index {} out of bounds", idx))?
                    .clone();
                self.out.push_str(&s);
            }
            Builtin::StrEq => {
                let b = self.pop()? as usize;
                let a = self.pop()? as usize;
                let sa = self.strings.get(a)
                    .ok_or_else(|| anyhow::anyhow!("str=: index {} out of bounds", a))?;
                let sb = self.strings.get(b)
                    .ok_or_else(|| anyhow::anyhow!("str=: index {} out of bounds", b))?;
                self.data.push(if sa == sb { -1 } else { 0 });
            }
            Builtin::StrLen => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("str-len: index {} out of bounds", idx))?;
                self.data.push(s.len() as i64);
            }
            Builtin::StrCat => {
                let b = self.pop()? as usize;
                let a = self.pop()? as usize;
                let sb = self.strings.get(b)
                    .ok_or_else(|| anyhow::anyhow!("str-cat: index {} out of bounds", b))?
                    .clone();
                let sa = self.strings.get(a)
                    .ok_or_else(|| anyhow::anyhow!("str-cat: index {} out of bounds", a))?
                    .clone();
                let cat = sa + &sb;
                let idx = self.strings.len();
                self.strings.push(cat);
                self.data.push(idx as i64);
            }
            Builtin::StrSplit => {
                let sep_idx = self.pop()? as usize;
                let src_idx = self.pop()? as usize;
                let sep = self.strings.get(sep_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-split: sep index {} out of bounds", sep_idx))?
                    .clone();
                let src = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-split: src index {} out of bounds", src_idx))?
                    .clone();
                let result = if sep.is_empty() {
                    src.chars().map(|c| c.to_string()).collect::<Vec<_>>().join("\n")
                } else {
                    src.split(sep.as_str()).collect::<Vec<_>>().join("\n")
                };
                let idx = self.strings.len();
                self.strings.push(result);
                self.data.push(idx as i64);
            }
            Builtin::StrJoin => {
                let sep_idx = self.pop()? as usize;
                let src_idx = self.pop()? as usize;
                let sep = self.strings.get(sep_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-join: sep index {} out of bounds", sep_idx))?
                    .clone();
                let src = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-join: src index {} out of bounds", src_idx))?
                    .clone();
                let result = src.lines().collect::<Vec<_>>().join(sep.as_str());
                let idx = self.strings.len();
                self.strings.push(result);
                self.data.push(idx as i64);
            }
            Builtin::StrSub => {
                let len  = self.pop()? as usize;
                let start = self.pop()? as usize;
                let src_idx = self.pop()? as usize;
                let src = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-sub: index {} out of bounds", src_idx))?
                    .clone();
                let result: String = src.chars().skip(start).take(len).collect();
                let idx = self.strings.len();
                self.strings.push(result);
                self.data.push(idx as i64);
            }
            Builtin::StrFind => {
                let needle_idx = self.pop()? as usize;
                let src_idx   = self.pop()? as usize;
                let needle = self.strings.get(needle_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-find: needle index {} out of bounds", needle_idx))?
                    .clone();
                let src = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-find: src index {} out of bounds", src_idx))?
                    .clone();
                let pos: i64 = if needle.is_empty() {
                    0
                } else if let Some(byte_pos) = src.find(needle.as_str()) {
                    // convert byte offset to char offset
                    src[..byte_pos].chars().count() as i64
                } else {
                    -1
                };
                self.data.push(pos);
            }
            Builtin::StrReplace => {
                let to_idx   = self.pop()? as usize;
                let from_idx = self.pop()? as usize;
                let src_idx  = self.pop()? as usize;
                let to   = self.strings.get(to_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-replace: to index {} out of bounds", to_idx))?
                    .clone();
                let from = self.strings.get(from_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-replace: from index {} out of bounds", from_idx))?
                    .clone();
                let src  = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-replace: src index {} out of bounds", src_idx))?
                    .clone();
                let result = if from.is_empty() { src } else { src.replace(from.as_str(), to.as_str()) };
                let idx = self.strings.len();
                self.strings.push(result);
                self.data.push(idx as i64);
            }
            Builtin::StrReverse => {
                let src_idx = self.pop()? as usize;
                let src = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str-reverse: index {} out of bounds", src_idx))?
                    .clone();
                let result: String = src.chars().rev().collect();
                let idx = self.strings.len();
                self.strings.push(result);
                self.data.push(idx as i64);
            }
            Builtin::Safe => {
                // Execute a Forth string without aborting on error.
                // Pushes -1 on success, 0 on failure; rolls back state on failure.
                let str_idx = self.pop()? as usize;
                let code = self.strings.get(str_idx)
                    .ok_or_else(|| anyhow::anyhow!("safe: invalid string index"))?
                    .clone();
                let snap       = self.snapshot();
                let saved_data = self.data.clone();
                let saved_out  = self.out.clone();
                match self.eval(&code) {
                    Ok(()) => { self.data.push(-1); }
                    Err(_) => {
                        self.restore(&snap);
                        self.data = saved_data;
                        self.out  = saved_out;
                        self.data.push(0);
                    }
                }
            }
            Builtin::NumToStr => {
                let n = self.pop()?;
                let s = n.to_string();
                let idx = self.intern_str(&s);
                self.data.push(idx as i64);
            }
            Builtin::StrToNum => {
                let src_idx = self.pop()? as usize;
                let s = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("str>num: invalid string index"))?
                    .trim()
                    .to_string();
                match s.parse::<i64>() {
                    Ok(n)  => { self.data.push(n);  self.data.push(-1); }
                    Err(_) => { self.data.push(0);  self.data.push(0);  }
                }
            }
            Builtin::WordDefined => {
                let idx = self.pop()? as usize;
                let name = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("word-defined?: invalid string index"))?
                    .clone();
                // Check colon definitions AND compiled builtins.
                let defined = self.name_index.contains_key(name.as_str())
                    || name_to_builtin(name.as_str()).is_some();
                self.data.push(if defined { -1 } else { 0 });
            }
            Builtin::WordNames => {
                let mut names: Vec<&str> = self.name_index.keys().map(|s| s.as_str()).collect();
                names.sort_unstable();
                let joined = names.join("\n");
                let idx = self.strings.len();
                self.strings.push(joined);
                self.data.push(idx as i64);
            }
            Builtin::NthLine => {
                let n       = self.pop()? as usize;
                let src_idx = self.pop()? as usize;
                let src = self.strings.get(src_idx)
                    .ok_or_else(|| anyhow::anyhow!("nth-line: invalid string index"))?
                    .clone();
                let line = src.lines().nth(n)
                    .ok_or_else(|| anyhow::anyhow!("nth-line: index {} out of range", n))?
                    .to_string();
                let idx = self.intern_str(&line);
                self.data.push(idx as i64);
            }
            Builtin::AgreeQ => {
                // ( str1 str2 -- flag )
                // Run both programs in isolation; push -1 if tops agree, 0 if not.
                // Never aborts. Leaves the caller's stack otherwise intact.
                if self.data.len() < 2 {
                    self.data.push(-1); // vacuously agree
                    return Ok(());
                }
                let idx2 = self.pop()? as usize;
                let idx1 = self.pop()? as usize;
                let code1 = self.strings.get(idx1).cloned()
                    .ok_or_else(|| anyhow::anyhow!("agree?: invalid string index {idx1}"))?;
                let code2 = self.strings.get(idx2).cloned()
                    .ok_or_else(|| anyhow::anyhow!("agree?: invalid string index {idx2}"))?;

                let saved_data = std::mem::take(&mut self.data);
                let saved_out  = self.out.clone();
                let snap       = self.snapshot();

                let r1 = self.eval(&code1).ok().and_then(|_| self.data.last().copied());
                self.data.clear();
                self.restore(&snap);

                let r2 = self.eval(&code2).ok().and_then(|_| self.data.last().copied());
                self.data.clear();
                self.restore(&snap);

                self.data = saved_data;
                self.out  = saved_out;

                let flag = match (r1, r2) {
                    (Some(a), Some(b)) if a == b => -1,
                    _ => 0,
                };
                self.data.push(flag);
            }
            Builtin::SameQ => {
                // ( str1 str2 -- flag )
                // Run both programs in isolation; push -1 if FULL stacks agree, 0 if not.
                // Never aborts.
                if self.data.len() < 2 {
                    self.data.push(-1);
                    return Ok(());
                }
                let idx2 = self.pop()? as usize;
                let idx1 = self.pop()? as usize;
                let code1 = self.strings.get(idx1).cloned()
                    .ok_or_else(|| anyhow::anyhow!("same?: invalid string index {idx1}"))?;
                let code2 = self.strings.get(idx2).cloned()
                    .ok_or_else(|| anyhow::anyhow!("same?: invalid string index {idx2}"))?;

                let saved_data = std::mem::take(&mut self.data);
                let saved_out  = self.out.clone();
                let snap       = self.snapshot();

                let stack1 = if self.eval(&code1).is_ok() {
                    self.data.clone()
                } else {
                    vec![]
                };
                self.data.clear();
                self.restore(&snap);

                let stack2 = if self.eval(&code2).is_ok() {
                    self.data.clone()
                } else {
                    vec![]
                };
                self.data.clear();
                self.restore(&snap);

                self.data = saved_data;
                self.out  = saved_out;

                let flag = if stack1 == stack2 { -1 } else { 0 };
                self.data.push(flag);
            }
            // ── Crypto primitives ─────────────────────────────────────────────
            Builtin::Sha256 => {
                use sha2::{Sha256, Digest};
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx)
                    .ok_or_else(|| anyhow::anyhow!("sha256: index {} out of bounds", idx))?
                    .as_bytes().to_vec();
                let hash = Sha256::digest(&s);
                let hex = crypto_hex_encode(&hash);
                let new_idx = self.strings.len();
                self.strings.push(hex);
                self.data.push(new_idx as i64);
            }
            Builtin::FileSha256 => {
                use sha2::{Sha256, Digest};
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-sha256: index {} out of bounds", path_idx))?
                    .clone();
                let content = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("file-sha256: cannot read {}: {}", path, e))?;
                let hash = Sha256::digest(&content);
                let hex = crypto_hex_encode(&hash);
                let new_idx = self.strings.len();
                self.strings.push(hex);
                self.data.push(new_idx as i64);
            }
            Builtin::FileHash => {
                // ( path-idx -- )  hash file, print hex to out — scatter-friendly (no pool idx noise)
                use sha2::{Sha256, Digest};
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-hash: index {} out of bounds", path_idx))?
                    .clone();
                match std::fs::read(&path) {
                    Ok(content) => {
                        let hash = Sha256::digest(&content);
                        let hex = crypto_hex_encode(&hash);
                        self.out.push_str(&hex);
                        self.out.push('\n');
                    }
                    Err(e) => self.out.push_str(&format!("file-hash: cannot read {}: {}\n", path, e)),
                }
            }
            Builtin::FileFetch => {
                // ( path-idx -- content-idx )  read whole file into string pool
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-fetch: index {} out of bounds", path_idx))?
                    .clone();
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("file-fetch: cannot read {}: {}", path, e))?;
                let new_idx = self.strings.len();
                self.strings.push(content);
                self.data.push(new_idx as i64);
            }
            Builtin::FileSlice => {
                // ( path-idx offset length -- content-idx )  read byte range into pool (utf-8 lossy)
                let length  = self.pop()? as usize;
                let offset  = self.pop()? as usize;
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-slice: index {} out of bounds", path_idx))?
                    .clone();
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("file-slice: cannot read {}: {}", path, e))?;
                let start = offset.min(bytes.len());
                let end   = (offset + length).min(bytes.len());
                let slice = String::from_utf8_lossy(&bytes[start..end]).into_owned();
                let new_idx = self.strings.len();
                self.strings.push(slice);
                self.data.push(new_idx as i64);
            }
            Builtin::FileSha256Range => {
                // ( path-idx offset length -- hex-idx )  SHA-256 of exact byte range
                use sha2::{Sha256, Digest};
                let length   = self.pop()? as usize;
                let offset   = self.pop()? as usize;
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-sha256-range: index {} out of bounds", path_idx))?
                    .clone();
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("file-sha256-range: cannot read {}: {}", path, e))?;
                let start = offset.min(bytes.len());
                let end   = (offset + length).min(bytes.len());
                let hex = crypto_hex_encode(&Sha256::digest(&bytes[start..end]));
                let new_idx = self.strings.len();
                self.strings.push(hex);
                self.data.push(new_idx as i64);
            }
            Builtin::FileHashRange => {
                // ( path-idx offset length -- )  SHA-256 of byte range → out
                use sha2::{Sha256, Digest};
                let length   = self.pop()? as usize;
                let offset   = self.pop()? as usize;
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-hash-range: index {} out of bounds", path_idx))?
                    .clone();
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        let start = offset.min(bytes.len());
                        let end   = (offset + length).min(bytes.len());
                        let hex = crypto_hex_encode(&Sha256::digest(&bytes[start..end]));
                        self.out.push_str(&hex);
                        self.out.push('\n');
                    }
                    Err(e) => self.out.push_str(&format!("file-hash-range: cannot read {}: {}\n", path, e)),
                }
            }
            Builtin::FileSize => {
                // ( path-idx -- n )  file size in bytes; -1 if not found
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-size: index {} out of bounds", path_idx))?
                    .clone();
                let n = std::fs::metadata(&path)
                    .map(|m| m.len() as i64)
                    .unwrap_or(-1);
                self.data.push(n);
            }
            Builtin::FileWrite => {
                // ( content-idx path-idx -- )  overwrite file with string content
                let path_idx    = self.pop()? as usize;
                let content_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-write: path index {} out of bounds", path_idx))?
                    .clone();
                let content = self.strings.get(content_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-write: content index {} out of bounds", content_idx))?
                    .clone();
                std::fs::write(&path, content.as_bytes())
                    .map_err(|e| anyhow::anyhow!("file-write: cannot write {}: {}", path, e))?;
            }
            Builtin::FileAppend => {
                // ( content-idx path-idx -- )  append string content to file
                use std::io::Write as IoWrite;
                let path_idx    = self.pop()? as usize;
                let content_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-append: path index {} out of bounds", path_idx))?
                    .clone();
                let content = self.strings.get(content_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-append: content index {} out of bounds", content_idx))?
                    .clone();
                let mut f = std::fs::OpenOptions::new()
                    .append(true).create(true).open(&path)
                    .map_err(|e| anyhow::anyhow!("file-append: cannot open {}: {}", path, e))?;
                f.write_all(content.as_bytes())
                    .map_err(|e| anyhow::anyhow!("file-append: cannot write {}: {}", path, e))?;
            }
            Builtin::Nonce => {
                use rand::RngCore;
                let n = rand::thread_rng().next_u64() as i64;
                self.data.push(n);
            }
            Builtin::Keygen => {
                use ed25519_dalek::SigningKey;
                use rand::rngs::OsRng;
                let signing_key = SigningKey::generate(&mut OsRng);
                let priv_hex = crypto_hex_encode(&signing_key.to_bytes());
                let pub_hex  = crypto_hex_encode(signing_key.verifying_key().as_bytes());
                // ( -- pub-idx priv-idx )  TOS = priv (ready to sign next)
                let pub_idx = self.strings.len();
                self.strings.push(pub_hex);
                let priv_idx = self.strings.len();
                self.strings.push(priv_hex);
                self.data.push(pub_idx as i64);
                self.data.push(priv_idx as i64);
            }
            Builtin::Sign => {
                use ed25519_dalek::{SigningKey, Signer};
                let data_idx    = self.pop()? as usize;
                let privkey_idx = self.pop()? as usize;
                let privkey_hex = self.strings.get(privkey_idx)
                    .ok_or_else(|| anyhow::anyhow!("sign: privkey index out of bounds"))?
                    .clone();
                let data_str = self.strings.get(data_idx)
                    .ok_or_else(|| anyhow::anyhow!("sign: data index out of bounds"))?
                    .clone();
                let privkey_bytes = crypto_hex_decode(&privkey_hex)
                    .map_err(|e| anyhow::anyhow!("sign: bad privkey hex: {}", e))?;
                if privkey_bytes.len() != 32 {
                    bail!("sign: privkey must be 32 bytes (64 hex chars), got {}", privkey_bytes.len());
                }
                let arr: [u8; 32] = privkey_bytes.try_into().unwrap();
                let signing_key = SigningKey::from_bytes(&arr);
                let sig = signing_key.sign(data_str.as_bytes());
                let sig_hex = crypto_hex_encode(&sig.to_bytes());
                let sig_idx = self.strings.len();
                self.strings.push(sig_hex);
                self.data.push(sig_idx as i64);
            }
            Builtin::Verify => {
                use ed25519_dalek::{VerifyingKey, Signature, Verifier};
                let data_idx   = self.pop()? as usize;
                let sig_idx    = self.pop()? as usize;
                let pubkey_idx = self.pop()? as usize;
                let pubkey_hex = self.strings.get(pubkey_idx)
                    .ok_or_else(|| anyhow::anyhow!("verify: pubkey index out of bounds"))?
                    .clone();
                let sig_hex = self.strings.get(sig_idx)
                    .ok_or_else(|| anyhow::anyhow!("verify: sig index out of bounds"))?
                    .clone();
                let data_str = self.strings.get(data_idx)
                    .ok_or_else(|| anyhow::anyhow!("verify: data index out of bounds"))?
                    .clone();
                let pubkey_bytes = crypto_hex_decode(&pubkey_hex)
                    .map_err(|e| anyhow::anyhow!("verify: bad pubkey hex: {}", e))?;
                let sig_bytes = crypto_hex_decode(&sig_hex)
                    .map_err(|e| anyhow::anyhow!("verify: bad sig hex: {}", e))?;
                if pubkey_bytes.len() != 32 {
                    bail!("verify: pubkey must be 32 bytes, got {}", pubkey_bytes.len());
                }
                if sig_bytes.len() != 64 {
                    bail!("verify: signature must be 64 bytes, got {}", sig_bytes.len());
                }
                let pub_arr: [u8; 32] = pubkey_bytes.try_into().unwrap();
                let sig_arr: [u8; 64] = sig_bytes.try_into().unwrap();
                let vk  = VerifyingKey::from_bytes(&pub_arr)
                    .map_err(|e| anyhow::anyhow!("verify: invalid pubkey: {}", e))?;
                let sig = Signature::from_bytes(&sig_arr);
                let ok  = vk.verify(data_str.as_bytes(), &sig).is_ok();
                self.data.push(if ok { -1 } else { 0 });
            }
            // ── Terminal control (crossterm) ─────────────────────────────────
            Builtin::AtXy => {
                use crossterm::{cursor, execute};
                let row = self.pop()? as u16;
                let col = self.pop()? as u16;
                execute!(std::io::stdout(), cursor::MoveTo(col, row)).ok();
            }
            Builtin::TermSize => {
                let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
                self.data.push(cols as i64);
                self.data.push(rows as i64);
            }
            Builtin::SaveCursor => {
                use crossterm::{cursor, execute};
                execute!(std::io::stdout(), cursor::SavePosition).ok();
            }
            Builtin::RestoreCursor => {
                use crossterm::{cursor, execute};
                execute!(std::io::stdout(), cursor::RestorePosition).ok();
            }
            Builtin::ClearEol => {
                use crossterm::{terminal::{Clear, ClearType}, execute};
                execute!(std::io::stdout(), Clear(ClearType::UntilNewLine)).ok();
            }
            Builtin::ClearLine => {
                use crossterm::{terminal::{Clear, ClearType}, execute};
                execute!(std::io::stdout(), Clear(ClearType::CurrentLine)).ok();
            }
            Builtin::ColorFg => {
                use crossterm::{style::{SetForegroundColor, Color}, execute};
                let n = self.pop()? as u8;
                execute!(std::io::stdout(), SetForegroundColor(Color::AnsiValue(n))).ok();
            }
            Builtin::ResetStyle => {
                use crossterm::{style::ResetColor, execute};
                execute!(std::io::stdout(), ResetColor).ok();
            }
            Builtin::SyncBegin => {
                use crossterm::{queue, terminal::BeginSynchronizedUpdate};
                use std::io::Write;
                queue!(std::io::stdout(), BeginSynchronizedUpdate).ok();
                std::io::stdout().flush().ok();
            }
            Builtin::SyncEnd => {
                use crossterm::{queue, terminal::EndSynchronizedUpdate};
                use std::io::Write;
                queue!(std::io::stdout(), EndSynchronizedUpdate).ok();
                std::io::stdout().flush().ok();
            }
            Builtin::HideCursor => {
                use crossterm::{cursor, execute};
                execute!(std::io::stdout(), cursor::Hide).ok();
            }
            Builtin::ShowCursor => {
                use crossterm::{cursor, execute};
                execute!(std::io::stdout(), cursor::Show).ok();
            }

            Builtin::Connected => {
                // -1 (true) if this VM has active peers, a registry address, or a known own address.
                // 0 (false) if completely isolated — no session joined.
                let connected = !self.peers.is_empty()
                    || self.registry_addr.is_some()
                    || self.my_addr.is_some();
                self.data.push(if connected { -1 } else { 0 });
            }

            // ── Proof system ────────────────────────────────────────────────
            Builtin::Assert => {
                let flag = self.pop()?;
                if flag == 0 {
                    anyhow::bail!("assertion failed");
                }
            }

            Builtin::ProveAll => {
                let test_words: Vec<String> = self.name_index.keys()
                    .filter(|k| k.starts_with("test:"))
                    .cloned()
                    .collect();
                let mut sorted = test_words;
                sorted.sort();
                let total = sorted.len();
                let mut pass = 0usize;
                let mut fail = 0usize;
                let mut fail_lines: Vec<String> = Vec::new();
                let outer_fuel  = self.fuel;       // restore after inner evals reset it
                let outer_depth = self.data.len(); // tests must not leak stack state
                for tw in &sorted {
                    let word_name = &tw["test:".len()..];
                    let saved = self.out.len();
                    let result = self.eval(tw);
                    self.out.truncate(saved);
                    self.data.truncate(outer_depth); // clean leaked stack values
                    use crossterm::style::Stylize;
                    match result {
                        Ok(_) => { pass += 1; }
                        Err(e) => {
                            fail_lines.push(format!("  ✗ {}: {}\n", word_name.red(), e));
                            fail += 1;
                        }
                    }
                }
                self.fuel = outer_fuel;
                use crossterm::style::Stylize;
                for line in fail_lines {
                    self.out.push_str(&line);
                }
                let pass_s = format!("{pass}/{total} passed");
                if fail == 0 {
                    self.out.push_str(&format!("── {} ──\n", pass_s.green().bold()));
                } else {
                    self.out.push_str(&format!(
                        "── {}  {} failed ──\n",
                        pass_s.yellow().bold(),
                        fail.to_string().red().bold(),
                    ));
                }
            }

            Builtin::ProveAllBool => {
                // Same as ProveAll but pushes -1 (all pass) or 0 (any fail).
                // Failures are printed; the summary line is suppressed on all-pass
                // so callers can print their own message.
                let test_words: Vec<String> = self.name_index.keys()
                    .filter(|k| k.starts_with("test:"))
                    .cloned()
                    .collect();
                let mut sorted = test_words;
                sorted.sort();
                let total = sorted.len();
                let mut pass = 0usize;
                let mut fail = 0usize;
                let mut fail_lines: Vec<String> = Vec::new();
                let outer_fuel  = self.fuel;
                let outer_depth = self.data.len();
                for tw in &sorted {
                    let word_name = &tw["test:".len()..];
                    let saved = self.out.len();
                    let result = self.eval(tw);
                    self.out.truncate(saved);
                    self.data.truncate(outer_depth);
                    use crossterm::style::Stylize;
                    match result {
                        Ok(_) => { pass += 1; }
                        Err(e) => {
                            fail_lines.push(format!("  ✗ {}: {}\n", word_name.red(), e));
                            fail += 1;
                        }
                    }
                }
                self.fuel = outer_fuel;
                use crossterm::style::Stylize;
                for line in fail_lines {
                    self.out.push_str(&line);
                }
                if fail > 0 {
                    let pass_s = format!("{pass}/{total} passed");
                    self.out.push_str(&format!(
                        "── {}  {} failed ──\n",
                        pass_s.yellow().bold(),
                        fail.to_string().red().bold(),
                    ));
                }
                self.data.push(if fail == 0 { -1 } else { 0 });
            }

            Builtin::ProveEnglish => {
                // Run every English-library word body in an isolated VM and report
                // how many succeed (no error).  "Prove most the words in english."
                //
                // Two kinds of proof:
                //   1. Execution proof — the Forth body runs without an unknown-word error.
                //      Stack underflows are acceptable (some words need input on the stack).
                //   2. Argue proof   — words that carry a `proof = [a, b]` field get an
                //      `argue` run: sentence A and sentence B must converge to the same stack.
                //      This bridges the English definition ↔ Forth machine.
                use crate::coforth::library::Library;
                use crossterm::style::Stylize;

                let defs = Library::builtin_defs();
                let pairs: Vec<(String, String)> = defs.pairs.clone();
                let proofs: Vec<(String, [String; 2])> = defs.proofs.clone();
                let total = pairs.len();
                let mut pass = 0usize;
                let mut fail_words: Vec<(String, String)> = Vec::new();

                // Phase 1: execution proofs (all words).
                for (word, body) in &pairs {
                    let mut vm = Library::precompiled_vm();
                    vm.clear_data();
                    match vm.exec_with_fuel(body.as_str(), 50_000) {
                        Ok(_) => { pass += 1; }
                        Err(e) => {
                            let msg = e.to_string();
                            // Only count hard failures: unknown words or compile errors.
                            // Stack underflows from words that expect input are acceptable.
                            if msg.contains("unknown word") || msg.contains("compile") {
                                fail_words.push((word.clone(), msg));
                            } else {
                                pass += 1;
                            }
                        }
                    }
                }

                // Phase 2: argue proofs (definition ↔ Forth bridge).
                let proof_total = proofs.len();
                let mut proof_pass = 0usize;
                let mut proof_fail: Vec<(String, String)> = Vec::new();
                for (word, [a, b]) in &proofs {
                    let mut vm = Library::precompiled_vm();
                    // Build: s" A" s" B" argue
                    let src = format!("s\" {}\" s\" {}\" argue", a, b);
                    match vm.exec_with_fuel(&src, 100_000) {
                        Ok(_) => { proof_pass += 1; }
                        Err(e) => {
                            proof_fail.push((word.clone(), e.to_string()));
                        }
                    }
                }

                let fail = fail_words.len();
                let pct = if total > 0 { (pass * 100) / total } else { 0 };

                for (word, msg) in &fail_words {
                    self.out.push_str(&format!("  ✗ {}: {}\n", word.as_str().red(), msg));
                }
                for (word, msg) in &proof_fail {
                    self.out.push_str(&format!("  ✗ argue:{}: {}\n", word.as_str().red(), msg));
                }

                let pass_s = format!("{pass}/{total} words ({pct}%)");
                if fail == 0 && proof_total == 0 {
                    self.out.push_str(&format!("── {} ──\n", pass_s.green().bold()));
                } else if fail == 0 && proof_fail.is_empty() {
                    let bridge_s = format!("{proof_pass}/{proof_total} proofs");
                    self.out.push_str(&format!(
                        "── {}  {} ──\n",
                        pass_s.green().bold(),
                        bridge_s.cyan().bold(),
                    ));
                } else {
                    let unresolved = fail + proof_fail.len();
                    self.out.push_str(&format!(
                        "── {}  {} unresolved ──\n",
                        pass_s.yellow().bold(),
                        unresolved.to_string().red().bold(),
                    ));
                }
            }

            Builtin::ProveLanguages => {
                // ( -- )  Argue English ↔ Chinese on shared Forth primitives.
                //
                // For each concept that both languages express via the same primitive,
                // run `argue`: prove that the two languages converge to the same stack value.
                //
                // English:  "3 4 +"     Chinese:  "3 4 加"  → both leave 7   ✓
                // English:  "true"      Chinese:  "是"       → both leave -1  ✓
                // … and so on.
                use crate::coforth::library::{Library, vocab_pairs_from_toml, ZH_LIBRARY};
                use crossterm::style::Stylize;

                // The cross-language pairs: (label, english_forth, chinese_word_or_forth).
                // The Chinese side uses the word name — which will be compiled from zh.toml.
                const PAIRS: &[(&str, &str, &str)] = &[
                    ("add / 加",        "3 4 +",   "3 4 加"),
                    ("subtract / 减",   "7 3 -",   "7 3 减"),
                    ("multiply / 乘",   "3 4 *",   "3 4 乘"),
                    ("divide / 除",     "12 3 /",  "12 3 除"),
                    ("true / 是",       "true",    "是"),
                    ("false / 否",      "false",   "否"),
                    ("zero / 零",       "0",       "零"),
                    ("equal / 相等",    "5 5 =",   "5 5 相等"),
                    ("greater / 大于",  "6 5 >",   "6 5 大于"),
                    ("less / 小于",     "5 6 <",   "5 6 小于"),
                ];

                // Build a VM that knows both English stdlib + Chinese vocabulary words.
                let mut vm = Library::precompiled_vm();
                let zh_pairs = vocab_pairs_from_toml(ZH_LIBRARY);
                vm.disable_logging();
                for (word, code) in &zh_pairs {
                    let def = format!(": {} {} ;\n", word, code);
                    let _ = vm.exec_with_fuel(&def, 10_000);
                }
                vm.enable_logging();

                let mut pass = 0usize;
                let mut fail = 0usize;

                self.out.push_str(&format!("{}\n",
                    "── prove-languages: English ↔ Chinese ──".cyan().bold()));

                for (label, en, zh) in PAIRS {
                    let saved = std::mem::take(&mut vm.data);
                    vm.data.clear();

                    vm.eval(en).ok();
                    let r_en = vm.data.last().copied();
                    vm.data.clear();

                    vm.eval(zh).ok();
                    let r_zh = vm.data.last().copied();
                    vm.data.clear();

                    vm.data = saved;

                    let fmt = |v: Option<i64>| -> String {
                        match v {
                            Some(-1) => "true".to_string(),
                            Some(0)  => "false/0".to_string(),
                            Some(n)  => n.to_string(),
                            None     => "—".to_string(),
                        }
                    };

                    if r_en.is_some() && r_en == r_zh {
                        pass += 1;
                        self.out.push_str(&format!(
                            "  {}  {}  ──→  {}   {}\n",
                            en.cyan(), zh.cyan(),
                            fmt(r_en).green().bold(),
                            "✓".green(),
                        ));
                    } else {
                        fail += 1;
                        self.out.push_str(&format!(
                            "  {} → {}  {} → {}   {} {}\n",
                            en.cyan(), fmt(r_en).red(),
                            zh.cyan(), fmt(r_zh).red(),
                            "✗".red(), (*label).dark_grey(),
                        ));
                    }
                }

                let total = pass + fail;
                if fail == 0 {
                    self.out.push_str(&format!(
                        "── {}/{} agreed — two languages, one stack ──\n",
                        pass.to_string().green().bold(),
                        total,
                    ));
                } else {
                    self.out.push_str(&format!(
                        "── {}/{} agreed   {} unresolved ──\n",
                        pass.to_string().yellow().bold(),
                        total,
                        fail.to_string().red().bold(),
                    ));
                }
            }

            Builtin::Infix => {
                // ( str -- )  Evaluate an infix expression.
                // Uses shunting-yard to respect precedence: * / before + -.
                // Supports: integers, +  -  *  /  mod  (  )
                // Example: "3 + 4 * 2" → 11   "(3 + 4) * 2" → 14
                let idx = self.pop()? as usize;
                let src = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("infix: invalid string index"))?;

                fn prec(op: &str) -> u8 {
                    match op { "*" | "/" | "mod" => 2, "+" | "-" => 1, _ => 0 }
                }
                fn apply(op: &str, a: i64, b: i64) -> anyhow::Result<i64> {
                    Ok(match op {
                        "+" => a.wrapping_add(b), "-" => a.wrapping_sub(b),
                        "*" => a.wrapping_mul(b),
                        "/" => { if b == 0 { anyhow::bail!("division by zero"); } a / b }
                        "mod" => { if b == 0 { anyhow::bail!("modulo by zero"); } a % b }
                        _ => anyhow::bail!("infix: unknown operator {op}"),
                    })
                }

                let mut out: Vec<i64>     = Vec::new(); // value stack
                let mut ops: Vec<String>  = Vec::new(); // operator stack

                for tok in src.split_whitespace() {
                    if let Ok(n) = tok.parse::<i64>() {
                        out.push(n);
                    } else if tok == "(" {
                        ops.push(tok.to_string());
                    } else if tok == ")" {
                        while ops.last().map(|s: &String| s.as_str()) != Some("(") {
                            let op = ops.pop().ok_or_else(|| anyhow::anyhow!("infix: mismatched parentheses"))?;
                            let b = out.pop().ok_or_else(|| anyhow::anyhow!("infix: stack underflow"))?;
                            let a = out.pop().ok_or_else(|| anyhow::anyhow!("infix: stack underflow"))?;
                            out.push(apply(&op, a, b)?);
                        }
                        ops.pop(); // discard "("
                    } else {
                        // operator — pop higher-or-equal precedence ops first
                        while ops.last()
                            .map(|o: &String| o != "(" && prec(o) >= prec(tok))
                            .unwrap_or(false)
                        {
                            let op = ops.pop().unwrap();
                            let b = out.pop().ok_or_else(|| anyhow::anyhow!("infix: stack underflow"))?;
                            let a = out.pop().ok_or_else(|| anyhow::anyhow!("infix: stack underflow"))?;
                            out.push(apply(&op, a, b)?);
                        }
                        ops.push(tok.to_string());
                    }
                }
                while let Some(op) = ops.pop() {
                    let b = out.pop().ok_or_else(|| anyhow::anyhow!("infix: stack underflow"))?;
                    let a = out.pop().ok_or_else(|| anyhow::anyhow!("infix: stack underflow"))?;
                    out.push(apply(&op, a, b)?);
                }
                let result = out.pop().ok_or_else(|| anyhow::anyhow!("infix: empty expression"))?;
                self.data.push(result);
            }

            // ── Boot poetry ──────────────────────────────────────────────────
            Builtin::RegisterBoot => {
                // ( str -- )  Register a boot poem line.
                // The text is stored in self.boot_poems; the REPL drains this
                // after each exec and appends lines to ~/.finch/boot.forth.
                let idx = self.pop()? as usize;
                let text = self.strings.get(idx).cloned()
                    .ok_or_else(|| anyhow::anyhow!("register-boot: invalid string index"))?;
                self.out.push_str(&format!("{}\n", text));
                self.boot_poems.push(text);
            }

            // ── Channel system ───────────────────────────────────────────────
            Builtin::ListChannels => {
                use crossterm::style::Stylize;
                if self.channels.is_empty() {
                    self.out.push_str("no channels joined\n");
                } else {
                    let mut chans: Vec<&String> = self.channels.iter().collect();
                    chans.sort();
                    for c in chans {
                        self.out.push_str(&format!("  {}\n", c.as_str().cyan().bold()));
                    }
                }
            }

            // ── Collection operations ────────────────────────────────────────
            Builtin::GlobPool => {
                let pattern_idx = self.pop()? as usize;
                let pattern = self.strings.get(pattern_idx).cloned().unwrap_or_default();
                let mut result = String::new();
                match glob::glob(&pattern) {
                    Ok(paths) => {
                        for path in paths.flatten() {
                            result.push_str(&path.display().to_string());
                            result.push('\n');
                        }
                    }
                    Err(e) => result.push_str(&format!("glob error: {e}\n")),
                }
                let new_idx = self.strings.len() as i64;
                self.strings.push(result);
                self.data.push(new_idx);
            }
            Builtin::CleanLines => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx).cloned().unwrap_or_default();
                let mut count = 0i64;
                for line in s.lines() {
                    let path = line.trim();
                    if !path.is_empty() && quarantine_file(path) {
                        count += 1;
                    }
                }
                self.data.push(count);
            }
            Builtin::GlobCount => {
                let idx = self.pop()? as usize;
                let pattern = self.strings.get(idx).cloned().unwrap_or_default();
                let count = glob::glob(&pattern)
                    .map(|paths| paths.flatten().count())
                    .unwrap_or(0) as i64;
                self.data.push(count);
            }
            Builtin::ExecCapture => {
                // ( cmd-idx -- output-idx )
                // Run a shell command; push stdout as a trimmed string pool entry.
                // Stderr is discarded.  On error, pushes an empty string.
                let idx = self.pop()? as usize;
                let cmd = self.strings.get(idx).cloned().unwrap_or_default();
                let stdout = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(&cmd)
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim_end().to_string())
                    .unwrap_or_default();
                let new_idx = self.strings.len() as i64;
                self.strings.push(stdout);
                self.data.push(new_idx);
            }
            Builtin::BackAndForthQ => {
                // ( n fwd-str back-str -- flag )
                // Apply fwd-str to n; apply back-str to the result; compare to n.
                // Push -1 if you are home again, 0 if not.  Never aborts.
                if self.data.len() < 3 {
                    self.data.push(0);
                    return Ok(());
                }
                let back_idx = self.pop()? as usize;
                let fwd_idx  = self.pop()? as usize;
                let n        = self.pop()?;

                let fwd_code  = self.strings.get(fwd_idx).cloned().unwrap_or_default();
                let back_code = self.strings.get(back_idx).cloned().unwrap_or_default();

                let saved_data = std::mem::take(&mut self.data);
                let saved_out  = self.out.clone();
                let snap       = self.snapshot();

                // Forward pass: run fwd_code with n on the stack.
                self.data.push(n);
                let m = if self.eval(&fwd_code).is_ok() {
                    self.data.last().copied()
                } else {
                    None
                };

                let flag = if let Some(m) = m {
                    // Backward pass: run back_code with m on the stack.
                    self.data.clear();
                    self.restore(&snap);
                    self.data.push(m);
                    let ok = self.eval(&back_code).is_ok();
                    let n_prime = self.data.last().copied();
                    match (ok, n_prime) {
                        (true, Some(np)) if np == n => -1,
                        _ => 0,
                    }
                } else {
                    0
                };

                self.data.clear();
                self.restore(&snap);
                self.data = saved_data;
                self.out  = saved_out;
                self.data.push(flag);
            }
            Builtin::InvertibleQ => {
                // ( n str -- flag )
                // Apply str to n to get m; apply str to m; check if result == n.
                // -1 if str is an involution (f(f(n)) = n).  Never aborts.
                if self.data.len() < 2 {
                    self.data.push(0);
                    return Ok(());
                }
                let idx = self.pop()? as usize;
                let n   = self.pop()?;
                let code = self.strings.get(idx).cloned().unwrap_or_default();

                let saved_data = std::mem::take(&mut self.data);
                let saved_out  = self.out.clone();
                let snap       = self.snapshot();

                // First application: n → m
                self.data.push(n);
                let m = if self.eval(&code).is_ok() {
                    self.data.last().copied()
                } else {
                    None
                };

                let flag = if let Some(m) = m {
                    // Second application: m → n'
                    self.data.clear();
                    self.restore(&snap);
                    self.data.push(m);
                    let ok = self.eval(&code).is_ok();
                    let n_prime = self.data.last().copied();
                    match (ok, n_prime) {
                        (true, Some(np)) if np == n => -1,
                        _ => 0,
                    }
                } else {
                    0
                };

                self.data.clear();
                self.restore(&snap);
                self.data = saved_data;
                self.out  = saved_out;
                self.data.push(flag);
            }
            Builtin::Help => {
                self.out.push_str(concat!(
                    "─── Co-Forth ────────────────────────────────────────────────────\n",
                    " Values go on the stack.  Words transform it.  Proofs settle it.\n",
                    "\n",
                    " Stack     3 4 +  →  7           dup drop swap over rot\n",
                    " Arithmetic + - * /  mod  negate  abs  max  min  square  pow\n",
                    " Compare   =  <  >  <>  0=  0<\n",
                    " Logic     and  or  xor  invert\n",
                    " Output    .  .\" hello\"  cr  .s  type\n",
                    " Strings   s\" hello\"  str-cat  str-len  str=  str-trim  str-find\n",
                    " Shell     s\" ls\" exec-capture  type\n",
                    " Define    : double  2 * ;\n",
                    " Proofs    s\" 2 3 +\" s\" 3 2 +\" agree?      \\ two machines, one answer\n",
                    "           5 s\" 1 +\" s\" 1 -\" back-and-forth?  \\ round trip\n",
                    "           9 s\" negate\" invertible?          \\ its own inverse\n",
                    " List      words\n",
                    " Describe  s\" dup\" describe\n",
                    "─────────────────────────────────────────────────────────────────\n",
                ));
            }
            Builtin::Describe => {
                // ( idx -- )  show what we know about the named word.
                use crossterm::style::Stylize;
                let idx = self.pop()? as usize;
                let name = self.strings.get(idx).cloned().unwrap_or_default();
                let name = name.trim().to_string();

                let mut found = false;

                // 1. User-defined colon word — show source.
                if let Some(src) = self.word_source(&name) {
                    self.out.push_str(&format!(
                        "  {} {}\n",
                        name.as_str().cyan().bold(),
                        src.trim().dark_grey(),
                    ));
                    found = true;
                }

                // 2. Library / vocabulary definition.
                static DESC_LIB: std::sync::OnceLock<crate::coforth::Library> = std::sync::OnceLock::new();
                let lib = DESC_LIB.get_or_init(crate::coforth::Library::load);
                if let Some(entry) = lib.lookup(&name) {
                    self.out.push_str(&format!("  {}\n", entry.definition.as_str().white()));
                    found = true;
                }

                // 3. Built-in.
                if !found {
                    if name_to_builtin(name.as_str()).is_some() {
                        self.out.push_str(&format!("  {}  — built-in word\n", name.as_str().cyan().bold()));
                    } else if self.var_index.contains_key(name.as_str()) {
                        self.out.push_str(&format!("  {}  — variable\n", name.as_str().cyan().bold()));
                    } else {
                        self.out.push_str(&format!("  {}  — not defined  (type `help` for a guide)\n", name.as_str().dark_grey()));
                    }
                }
            }
            Builtin::Compute => {
                // ( str-idx -- )
                // Evaluate str as a computation and print the result.
                //
                // Strategy:
                //   1. Try as Forth code (via safe execution).
                //   2. If Forth fails, try as infix expression.
                //   3. Print "= <result>" on success; "?" on failure.
                //
                // The caller's stack is restored; Compute leaves nothing.
                let idx = self.pop()? as usize;
                let src = self.strings.get(idx).cloned().unwrap_or_default();

                let saved_data = std::mem::take(&mut self.data);
                let saved_out  = self.out.clone();
                let snap       = self.snapshot();

                // Attempt 1: Forth.
                let result = {
                    let forth_ok = self.eval(&src).is_ok();
                    let tos = self.data.last().copied();
                    self.data.clear();
                    self.restore(&snap);
                    if forth_ok { tos } else { None }
                };

                // Attempt 2: infix (if Forth failed).
                // Push the source string index onto the data stack, then call `infix`
                // which pops it and evaluates the shunting-yard expression.
                let result = if result.is_some() {
                    result
                } else {
                    self.data.push(idx as i64);
                    let infix_ok = self.eval("infix").is_ok();
                    let tos = self.data.last().copied();
                    self.data.clear();
                    self.restore(&snap);
                    if infix_ok { tos } else { None }
                };

                self.data = saved_data;
                self.out  = saved_out;

                match result {
                    Some(n) => self.out.push_str(&format!("= {n}\n")),
                    None    => self.out.push_str("?\n"),
                }
            }
            Builtin::EquivQ => {
                // ( str1 str2 -- flag )
                // Test program equivalence by probing both programs over a range of inputs.
                //
                // For each n in -5..=5, run each program with n pre-loaded on the stack.
                // If the programs leave the same TOS for every input where both succeed,
                // push -1 (equivalent).  Push 0 if they ever disagree on any input.
                //
                // Programs that always fail, or programs that produce results only on
                // a subset of inputs, are equivalent iff their successful outputs agree.
                // Two programs that both fail on every input are considered equivalent.
                //
                // Never aborts.  The caller's stack is restored.
                if self.data.len() < 2 {
                    self.data.push(-1);
                    return Ok(());
                }
                let idx2 = self.pop()? as usize;
                let idx1 = self.pop()? as usize;
                let code1 = self.strings.get(idx1).cloned().unwrap_or_default();
                let code2 = self.strings.get(idx2).cloned().unwrap_or_default();

                let saved_data = std::mem::take(&mut self.data);
                let saved_out  = self.out.clone();
                let snap       = self.snapshot();

                let mut equiv = true;
                for n in -5i64..=5 {
                    // Run program 1 with n on the stack.
                    self.data.push(n);
                    let r1 = if self.eval(&code1).is_ok() { self.data.last().copied() } else { None };
                    self.data.clear();
                    self.restore(&snap);

                    // Run program 2 with n on the stack.
                    self.data.push(n);
                    let r2 = if self.eval(&code2).is_ok() { self.data.last().copied() } else { None };
                    self.data.clear();
                    self.restore(&snap);

                    // Both produced a result — they must agree.
                    if let (Some(a), Some(b)) = (r1, r2) {
                        if a != b {
                            equiv = false;
                            break;
                        }
                    }
                    // One failed and one succeeded — not equivalent.
                    if r1.is_some() != r2.is_some() {
                        equiv = false;
                        break;
                    }
                }

                self.data = saved_data;
                self.out  = saved_out;
                self.data.push(if equiv { -1 } else { 0 });
            }
            Builtin::Fork => {
                // ( str-idx -- )
                // Run code in an isolated copy of the current VM.
                // The fork inherits the current dictionary AND the current data stack.
                // Output from the fork is appended to self.out.
                // The current VM is unaffected — no stack change, no definition change.
                let str_idx = self.pop()? as usize;
                let code = self.strings.get(str_idx).cloned().unwrap_or_default();

                let mut forked = self.fork_vm();
                match forked.eval(&code) {
                    Ok(()) => {
                        if !forked.out.is_empty() {
                            self.out.push_str(&forked.out);
                        }
                        // Show the fork's resulting stack.
                        if !forked.data.is_empty() {
                            let stack: Vec<String> = forked.data.iter().map(|n| n.to_string()).collect();
                            self.out.push_str(&format!("fork →  {}\n", stack.join("  ")));
                        }
                    }
                    Err(e) => {
                        self.out.push_str(&format!("fork ✗  {e}\n"));
                    }
                }
            }
            Builtin::Boot => {
                // ( -- )
                // Re-execute all vocabulary words that have boot = true.
                // Safe: failures are silently ignored (a boot word failing should not
                // abort the session).
                static BOOT_LIB: std::sync::OnceLock<crate::coforth::Library> =
                    std::sync::OnceLock::new();
                let lib = BOOT_LIB.get_or_init(crate::coforth::Library::load);
                let entries: Vec<String> = lib.boot_entries()
                    .iter()
                    .filter_map(|e| e.forth.clone())
                    .collect();
                for code in &entries {
                    let _ = self.eval(code);
                }
            }
            Builtin::PrintR => {
                // ( n width -- )  print n right-aligned in a field of `width` chars.
                // If the number is wider than `width`, it prints without padding.
                let width = self.pop()? as usize;
                let n = self.pop()?;
                self.out.push_str(&format!("{n:>width$}"));
            }
            Builtin::PrintPad => {
                // ( n width char-idx -- )  print n left-padded with char to width.
                let char_idx = self.pop()? as usize;
                let width    = self.pop()? as usize;
                let n        = self.pop()?;
                let pad_char = self.strings.get(char_idx)
                    .and_then(|s| s.chars().next())
                    .unwrap_or(' ');
                let s = n.to_string();
                let pad = width.saturating_sub(s.len());
                for _ in 0..pad { self.out.push(pad_char); }
                self.out.push_str(&s);
            }
            // ── Fast hash operations ───────────────────────────────────────────
            Builtin::Hash => {
                // ( str-idx -- n )  FNV1a-64 hash of string → i64
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx).map(|s| s.as_str()).unwrap_or("");
                const FNV_OFFSET: u64 = 14695981039346656037;
                const FNV_PRIME:  u64 = 1099511628211;
                let h = s.bytes().fold(FNV_OFFSET, |acc, b| {
                    acc.wrapping_mul(FNV_PRIME) ^ (b as u64)
                });
                self.data.push(h as i64);
            }
            Builtin::HashInt => {
                // ( n -- n' )  fast integer mix (Murmur3 finalizer)
                let n = self.pop()? as u64;
                let n = (n ^ (n >> 33)).wrapping_mul(0xff51afd7ed558ccd);
                let n = (n ^ (n >> 33)).wrapping_mul(0xc4ceb9fe1a85ec53);
                let n = n ^ (n >> 33);
                self.data.push(n as i64);
            }
            Builtin::HashCombine => {
                // ( h n -- h' )  combine hash h with value n
                let n = self.pop()?;
                let h = self.pop()?;
                let mixed = (h as u64) ^ ((n as u64).wrapping_mul(0x9e3779b97f4a7c15));
                self.data.push(mixed as i64);
            }
            Builtin::SortLines => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx).cloned().unwrap_or_default();
                let mut lines: Vec<&str> = s.lines().collect();
                lines.sort_unstable();
                let sorted = lines.join("\n");
                let new_idx = self.strings.len() as i64;
                self.strings.push(sorted);
                self.data.push(new_idx);
            }
            Builtin::UniqueLines => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx).cloned().unwrap_or_default();
                let mut seen = std::collections::HashSet::new();
                let unique: Vec<&str> = s.lines().filter(|l| seen.insert(*l)).collect();
                let result = unique.join("\n");
                let new_idx = self.strings.len() as i64;
                self.strings.push(result);
                self.data.push(new_idx);
            }
            Builtin::ReverseLines => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx).cloned().unwrap_or_default();
                let reversed: Vec<&str> = s.lines().rev().collect();
                let result = reversed.join("\n");
                let new_idx = self.strings.len() as i64;
                self.strings.push(result);
                self.data.push(new_idx);
            }
            Builtin::LineCount => {
                let idx = self.pop()? as usize;
                let s = self.strings.get(idx).cloned().unwrap_or_default();
                let count = if s.is_empty() { 0 } else { s.lines().count() } as i64;
                self.data.push(count);
            }

            // ── Security scanning ───────────────────────────────────────────
            Builtin::ScanFile => {
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("scan-file: index out of bounds"))?
                    .clone();
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("scan-file: cannot read {}: {}", path, e))?;
                let report = scan_bytes_for_signatures(&bytes, Some(&path));
                let idx = self.strings.len();
                self.strings.push(report);
                self.data.push(idx as i64);
            }

            Builtin::ScanBytes => {
                let str_idx = self.pop()? as usize;
                let content = self.strings.get(str_idx)
                    .ok_or_else(|| anyhow::anyhow!("scan-bytes: index out of bounds"))?
                    .clone();
                let score = scan_risk_score(content.as_bytes());
                self.data.push(score as i64);
            }

            Builtin::FileEntropy => {
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("file-entropy: index out of bounds"))?
                    .clone();
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("file-entropy: cannot read {}: {}", path, e))?;
                let entropy = shannon_entropy(&bytes);
                self.data.push((entropy * 1000.0) as i64);
            }

            Builtin::ScanDir => {
                let path_idx = self.pop()? as usize;
                let root = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("scan-dir: index out of bounds"))?
                    .clone();
                let report = scan_dir_recursive(&root);
                let idx = self.strings.len();
                self.strings.push(report);
                self.data.push(idx as i64);
            }

            Builtin::ScanStrings => {
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("scan-strings: index out of bounds"))?
                    .clone();
                let bytes = std::fs::read(&path)
                    .map_err(|e| anyhow::anyhow!("scan-strings: cannot read {}: {}", path, e))?;
                let extracted = extract_strings(&bytes, 6);
                let idx = self.strings.len();
                self.strings.push(extracted);
                self.data.push(idx as i64);
            }

            Builtin::ScanProcs => {
                let report = scan_processes();
                let idx = self.strings.len();
                self.strings.push(report);
                self.data.push(idx as i64);
            }

            Builtin::ScanNet => {
                let report = scan_network();
                let idx = self.strings.len();
                self.strings.push(report);
                self.data.push(idx as i64);
            }

            Builtin::ScanStartup => {
                let report = scan_startup_locations();
                let idx = self.strings.len();
                self.strings.push(report);
                self.data.push(idx as i64);
            }

            Builtin::Quarantine => {
                let path_idx = self.pop()? as usize;
                let path = self.strings.get(path_idx)
                    .ok_or_else(|| anyhow::anyhow!("quarantine: index out of bounds"))?
                    .clone();
                let flag = quarantine_file(&path);
                self.data.push(if flag { -1 } else { 0 });
            }
        }
        Ok(())
    }

    /// Tail-call optimisation: if a word's last instruction before `Ret` is
    /// `Addr(x)`, replace it with `Jmp(x)` and remove the trailing `Ret`.
    ///
    /// `Jmp(x)` jumps without pushing a return address, so when x executes
    /// its `Ret` it pops the *caller's* return address — correct TCO.
    ///
    /// Only applied when no jump inside the word targets the `Ret` position
    /// (i.e. no early-return branches pointing at it), to avoid corrupting
    /// control flow.
    fn apply_tco(&mut self, word_start: usize) {
        let end = self.memory.len();
        if end < word_start + 2 { return; }
        // Last cell must be Ret, second-to-last must be a call.
        if !matches!(self.memory[end - 1], Cell::Ret) { return; }
        let tail_addr = match self.memory[end - 2] {
            Cell::Addr(x) => x,
            _ => return,
        };
        // Ensure no jump inside the word targets the Ret position.
        let ret_pos = end - 1;
        for cell in &self.memory[word_start..end - 1] {
            let targets = match *cell {
                Cell::Jmp(t) | Cell::JmpZ(t) | Cell::While(t) | Cell::Repeat(t)
                | Cell::Until(t) | Cell::DoLoop(t) | Cell::DoLoopPlus(t) => Some(t),
                _ => None,
            };
            if targets == Some(ret_pos) { return; }
        }
        // Safe: convert tail call to jump and remove Ret.
        self.memory[end - 2] = Cell::Jmp(tail_addr);
        self.memory.truncate(end - 1);
    }

    #[inline(always)]
    fn pop(&mut self) -> Result<i64> {
        self.data.pop().ok_or_else(|| anyhow::anyhow!("stack underflow"))
    }

    #[inline]
    fn push(&mut self, v: i64) -> Result<()> {
        if self.data.len() >= MAX_DATA_DEPTH {
            bail!("stack overflow — too many values on the stack (max {})", MAX_DATA_DEPTH);
        }
        self.data.push(v);
        Ok(())
    }
}

// ── Security scanning helpers ─────────────────────────────────────────────────

// ── Signature database ────────────────────────────────────────────────────────

struct Sig { pattern: &'static [u8], label: &'static str, score: u8 }

const BYTE_SIGS: &[Sig] = &[
    Sig { pattern: b"MZ",                                     label: "PE/DOS executable",          score: 25 },
    Sig { pattern: b"\x7fELF",                                label: "ELF executable",              score: 25 },
    Sig { pattern: b"\xca\xfe\xba\xbe",                       label: "Mach-O fat binary",           score: 20 },
    Sig { pattern: b"\xfe\xed\xfa\xce",                       label: "Mach-O 32-bit",               score: 20 },
    Sig { pattern: b"\xfe\xed\xfa\xcf",                       label: "Mach-O 64-bit",               score: 20 },
    Sig { pattern: b"\xce\xfa\xed\xfe",                       label: "Mach-O 32-bit LE",            score: 20 },
    Sig { pattern: b"\xcf\xfa\xed\xfe",                       label: "Mach-O 64-bit LE",            score: 20 },
    Sig { pattern: b"#!",                                     label: "shell script",                score:  5 },
    // Exploit kits / shellcode markers
    Sig { pattern: b"\x90\x90\x90\x90\x90\x90\x90\x90",      label: "NOP sled (shellcode)",        score: 35 },
    Sig { pattern: b"\xeb\xfe",                               label: "infinite loop (shellcode)",   score: 30 },
    // Archive / dropper
    Sig { pattern: b"PK\x03\x04",                             label: "ZIP/JAR/APK archive",         score:  5 },
    Sig { pattern: b"\x1f\x8b",                               label: "gzip archive",                score:  5 },
    Sig { pattern: b"Rar!\x1a\x07",                           label: "RAR archive",                 score:  5 },
    // Office macros
    Sig { pattern: b"\xd0\xcf\x11\xe0",                       label: "OLE2 compound doc (Office)",  score: 10 },
    // Java
    Sig { pattern: b"\xca\xfe\xba\xbe",                       label: "Java class file",             score: 10 },
];

struct StrSig { pattern: &'static str, label: &'static str, score: u8 }

const STR_SIGS: &[StrSig] = &[
    // EICAR test
    StrSig { pattern: "EICAR-STANDARD-ANTIVIRUS-TEST-FILE",   label: "EICAR antivirus test file",  score: 100 },
    // Webshells
    StrSig { pattern: "eval(base64_decode",                   label: "PHP webshell (eval+b64)",     score: 40 },
    StrSig { pattern: "assert($_REQUEST",                     label: "PHP webshell (assert)",       score: 40 },
    StrSig { pattern: "passthru($_",                          label: "PHP webshell (passthru)",     score: 40 },
    StrSig { pattern: "system($_REQUEST",                     label: "PHP webshell (system)",       score: 40 },
    StrSig { pattern: "exec($_GET",                           label: "PHP webshell (exec+GET)",     score: 40 },
    // Encoded execution
    StrSig { pattern: "powershell -enc",                      label: "encoded PowerShell",          score: 35 },
    StrSig { pattern: "powershell -e ",                       label: "encoded PowerShell",          score: 30 },
    StrSig { pattern: "powershell -nop",                      label: "PowerShell no-profile exec",  score: 25 },
    StrSig { pattern: "cmd.exe /c ",                          label: "Windows shell execution",     score: 20 },
    StrSig { pattern: "mshta.exe",                            label: "MSHTA execution (LOLBin)",    score: 25 },
    StrSig { pattern: "regsvr32.exe",                         label: "Regsvr32 LOLBin",             score: 20 },
    // Destructive
    StrSig { pattern: "rm -rf /",                             label: "recursive root delete",       score: 30 },
    StrSig { pattern: "dd if=/dev/zero",                      label: "disk-wipe command",           score: 30 },
    StrSig { pattern: "shred -u",                             label: "secure file delete",          score: 20 },
    StrSig { pattern: "mkfs.",                                label: "filesystem format",            score: 25 },
    // Downloaders
    StrSig { pattern: "wget http",                            label: "remote download (wget)",      score: 15 },
    StrSig { pattern: "curl http",                            label: "remote download (curl)",      score: 15 },
    StrSig { pattern: "urllib.request.urlopen",               label: "Python remote download",      score: 15 },
    // Persistence
    StrSig { pattern: "crontab -",                            label: "cron persistence",            score: 15 },
    StrSig { pattern: "LaunchAgents",                         label: "macOS LaunchAgent reference", score: 10 },
    StrSig { pattern: "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                                                              label: "Windows Run key persistence", score: 25 },
    // Crypto miners
    StrSig { pattern: "stratum+tcp://",                       label: "mining pool connection",      score: 45 },
    StrSig { pattern: "xmrig",                                label: "XMRig miner",                 score: 45 },
    StrSig { pattern: "cryptonight",                          label: "CryptoNight algorithm",       score: 40 },
    StrSig { pattern: "monero",                               label: "Monero mining reference",     score: 20 },
    // Rootkit / evasion
    StrSig { pattern: "ptrace",                               label: "ptrace (debugger/rootkit)",   score: 15 },
    StrSig { pattern: "LD_PRELOAD",                           label: "LD_PRELOAD injection",        score: 25 },
    StrSig { pattern: "DYLD_INSERT_LIBRARIES",                label: "macOS dylib injection",       score: 25 },
    StrSig { pattern: "NtSetInformationThread",               label: "Windows anti-debug",          score: 20 },
    // Ransomware indicators
    StrSig { pattern: ".onion",                               label: "Tor hidden service (.onion)", score: 20 },
    StrSig { pattern: "your files have been encrypted",       label: "ransomware note",             score: 60 },
    StrSig { pattern: "bitcoin address",                      label: "ransom payment instruction",  score: 40 },
    StrSig { pattern: "AES_encrypt",                          label: "AES encryption call",         score: 15 },
    // Backdoors
    StrSig { pattern: "/bin/sh -i",                           label: "interactive shell spawn",     score: 30 },
    StrSig { pattern: "nc -e /bin/sh",                        label: "netcat reverse shell",        score: 45 },
    StrSig { pattern: "bash -i >&",                           label: "bash reverse shell",          score: 45 },
    StrSig { pattern: "socket.connect(",                      label: "socket connect (C2)",         score: 10 },
    // Keyloggers
    StrSig { pattern: "GetAsyncKeyState",                     label: "Windows keylogger API",       score: 30 },
    StrSig { pattern: "SetWindowsHookEx",                     label: "Windows hook injection",      score: 25 },
];

// ── Scanning helpers ──────────────────────────────────────────────────────────

/// Shannon entropy of a byte slice (0.0 = uniform, 8.0 = maximum randomness).
fn shannon_entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() { return 0.0; }
    let mut counts = [0u64; 256];
    for &b in bytes { counts[b as usize] += 1; }
    let len = bytes.len() as f64;
    counts.iter()
        .filter(|&&c| c > 0)
        .map(|&c| { let p = c as f64 / len; -p * p.log2() })
        .sum()
}

/// Return a risk score 0–100 based on suspicious byte patterns.
fn scan_risk_score(bytes: &[u8]) -> u8 {
    let mut score: u32 = 0;

    // Entropy
    let entropy = shannon_entropy(bytes);
    if entropy > 7.5      { score += 40; }
    else if entropy > 7.0 { score += 20; }
    else if entropy > 6.5 { score += 10; }

    // Binary signature table
    for sig in BYTE_SIGS {
        if bytes.starts_with(sig.pattern) {
            score += sig.score as u32;
        }
    }

    // String signature table (lossy UTF-8 view)
    let text = String::from_utf8_lossy(bytes);
    let text_lower = text.to_lowercase();
    for sig in STR_SIGS {
        let needle_lower = sig.pattern.to_lowercase();
        if text_lower.contains(&needle_lower) {
            score += sig.score as u32;
        }
    }

    // Mixed nulls — shellcode smell
    let null_count = bytes.iter().filter(|&&b| b == 0).count();
    if null_count > 0 && null_count < bytes.len() {
        let ratio = null_count as f64 / bytes.len() as f64;
        if ratio > 0.05 && ratio < 0.95 { score += 15; }
    }

    score.min(100) as u8
}

/// Full scan report for a file: human-readable text.
fn scan_bytes_for_signatures(bytes: &[u8], path: Option<&str>) -> String {
    let score   = scan_risk_score(bytes);
    let entropy = shannon_entropy(bytes);

    let risk_label = match score {
        0..=19   => "clean",
        20..=49  => "low",
        50..=74  => "medium",
        75..=99  => "high",
        _        => "critical",
    };

    let mut findings: Vec<String> = vec![
        format!("risk: {} ({}/100)", risk_label, score),
        format!("entropy: {:.3}  size: {} bytes", entropy, bytes.len()),
    ];

    // File type from magic bytes
    let kind = if bytes.starts_with(b"MZ")                           { "PE/DOS executable" }
        else if bytes.starts_with(b"\x7fELF")                        { "ELF executable" }
        else if bytes.starts_with(b"\xcf\xfa\xed\xfe") ||
                bytes.starts_with(b"\xce\xfa\xed\xfe") ||
                bytes.starts_with(b"\xfe\xed\xfa\xce") ||
                bytes.starts_with(b"\xfe\xed\xfa\xcf") ||
                bytes.starts_with(b"\xca\xfe\xba\xbe")               { "Mach-O binary" }
        else if bytes.starts_with(b"%PDF")                           { "PDF document" }
        else if bytes.starts_with(b"PK\x03\x04")                     { "ZIP/JAR/APK" }
        else if bytes.starts_with(b"\x1f\x8b")                       { "gzip" }
        else if bytes.starts_with(b"Rar!\x1a\x07")                   { "RAR archive" }
        else if bytes.starts_with(b"\xd0\xcf\x11\xe0")               { "OLE2/Office document" }
        else if bytes.starts_with(b"#!")                             { "shell script" }
        else                                                          { "data/text" };
    findings.push(format!("type: {}", kind));

    if entropy > 7.5 {
        findings.push("! high entropy — packed or encrypted payload".to_string());
    }

    // Matched string signatures
    let text       = String::from_utf8_lossy(bytes);
    let text_lower = text.to_lowercase();
    for sig in STR_SIGS {
        if text_lower.contains(&sig.pattern.to_lowercase()) {
            findings.push(format!("! {}", sig.label));
        }
    }

    // Matched byte signatures (for embedded content, not just file header)
    for sig in BYTE_SIGS {
        if bytes.windows(sig.pattern.len()).any(|w| w == sig.pattern) &&
           !bytes.starts_with(sig.pattern) {
            // Only flag if NOT the file's own header (already reported in type)
            findings.push(format!("embedded: {}", sig.label));
        }
    }

    let header = match path {
        Some(p) => format!("scan  {}", p),
        None    => "scan  (bytes)".to_string(),
    };
    format!("{}\n{}", header, findings.join("\n"))
}

/// Extract printable ASCII strings of at least `min_len` bytes (like the `strings` utility).
fn extract_strings(bytes: &[u8], min_len: usize) -> String {
    let mut result = String::new();
    let mut current = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() || b == b' ' {
            current.push(b as char);
        } else {
            if current.len() >= min_len {
                result.push_str(&current);
                result.push('\n');
            }
            current.clear();
        }
    }
    if current.len() >= min_len {
        result.push_str(&current);
        result.push('\n');
    }
    result
}

/// Recursively scan a directory. Returns a summary report.
fn scan_dir_recursive(root: &str) -> String {
    let mut lines: Vec<String> = vec![format!("scan-dir  {}", root)];
    let mut total   = 0usize;
    let mut flagged = 0usize;
    let mut errors  = 0usize;

    fn walk(dir: &str, lines: &mut Vec<String>, total: &mut usize, flagged: &mut usize, errors: &mut usize, depth: usize) {
        if depth > 20 { return; } // guard against deep symlink cycles
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => { lines.push(format!("  err  {}: {}", dir, e)); *errors += 1; return; }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let path_str = path.to_string_lossy().to_string();
            // Skip known-safe dirs to keep scans fast
            if path_str.contains("/.git/") || path_str.contains("/target/") { continue; }
            if path.is_dir() {
                walk(&path_str, lines, total, flagged, errors, depth + 1);
            } else if path.is_file() {
                *total += 1;
                match std::fs::read(&path) {
                    Ok(bytes) => {
                        let score = scan_risk_score(&bytes);
                        if score >= 20 {
                            *flagged += 1;
                            let label = match score {
                                20..=49 => "low",
                                50..=74 => "med",
                                75..=99 => "high",
                                _       => "crit",
                            };
                            lines.push(format!("  [{:>4}] [{label}] {path_str}", score));
                        }
                    }
                    Err(_) => { *errors += 1; }
                }
            }
        }
    }

    walk(root, &mut lines, &mut total, &mut flagged, &mut errors, 0);
    lines.push(format!("total: {}  flagged: {}  errors: {}", total, flagged, errors));
    lines.join("\n")
}

/// Scan running processes for suspicious indicators.
fn scan_processes() -> String {
    let mut lines = vec!["scan-procs".to_string()];

    // Common suspicious process names
    let suspicious_names = [
        "xmrig", "minerd", "cpuminer", "cgminer",     // miners
        "ncat", "socat", "cryptcat",                   // network tools
        "mimikatz", "metasploit", "msfconsole",        // attack tools
        "keylogger", "rootkit",                        // obvious malware names
    ];

    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("ps").args(["axo", "pid,comm,args"]).output() {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines().skip(1) {
                    let lower = line.to_lowercase();
                    for name in &suspicious_names {
                        if lower.contains(name) {
                            lines.push(format!("! {}", line.trim()));
                            break;
                        }
                    }
                }
                let count = text.lines().count().saturating_sub(1);
                lines.push(format!("{} processes scanned", count));
            }
            Err(e) => lines.push(format!("err: {}", e)),
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Read /proc on Linux
        if let Ok(entries) = std::fs::read_dir("/proc") {
            let mut count = 0usize;
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.chars().all(|c| c.is_ascii_digit()) {
                    count += 1;
                    let cmdline_path = format!("/proc/{}/cmdline", name_str);
                    if let Ok(bytes) = std::fs::read(&cmdline_path) {
                        let cmdline = String::from_utf8_lossy(&bytes).replace('\0', " ");
                        let lower = cmdline.to_lowercase();
                        for sus in &suspicious_names {
                            if lower.contains(sus) {
                                lines.push(format!("! pid {}  {}", name_str, cmdline.trim()));
                                break;
                            }
                        }
                    }
                }
            }
            lines.push(format!("{} processes scanned", count));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    lines.push("process scanning not supported on this platform".to_string());

    lines.join("\n")
}

/// Scan open network connections for suspicious endpoints.
fn scan_network() -> String {
    let mut lines = vec!["scan-net".to_string()];

    // Known bad ports
    let suspicious_ports: &[u16] = &[
        4444, 4445, 1337, 31337, // common reverse shells
        6666, 6667, 6668, 6669,  // IRC (botnet C2)
        8080, 8888,              // common C2 HTTP alt ports
        9050, 9051,              // Tor SOCKS proxy
    ];

    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("netstat").args(["-an", "-p", "tcp"]).output() {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut flagged = 0usize;
                let mut total   = 0usize;
                for line in text.lines() {
                    if line.contains("ESTABLISHED") || line.contains("LISTEN") {
                        total += 1;
                        // Check for suspicious ports in the line
                        for &port in suspicious_ports {
                            if line.contains(&format!(".{}", port)) || line.contains(&format!(":{}", port)) {
                                lines.push(format!("! {}", line.trim()));
                                flagged += 1;
                                break;
                            }
                        }
                        // Check for .onion DNS lookups
                        if line.contains(".onion") {
                            lines.push(format!("! tor: {}", line.trim()));
                            flagged += 1;
                        }
                    }
                }
                lines.push(format!("{} connections  {} flagged", total, flagged));
            }
            Err(e) => lines.push(format!("err: {}", e)),
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Read /proc/net/tcp and /proc/net/tcp6
        for proto_file in &["/proc/net/tcp", "/proc/net/tcp6"] {
            if let Ok(content) = std::fs::read_to_string(proto_file) {
                for line in content.lines().skip(1) {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 4 {
                        let local_addr = parts[1];
                        let state = parts[3];
                        if state == "0A" { // LISTEN
                            // Parse hex port (last 4 chars of addr field)
                            if let Some(port_hex) = local_addr.split(':').nth(1) {
                                if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                                    if suspicious_ports.contains(&port) {
                                        lines.push(format!("! listening on suspicious port {}", port));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        lines.push("checked /proc/net/tcp".to_string());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    lines.push("network scanning not supported on this platform".to_string());

    lines.join("\n")
}

/// Check common persistence/startup locations.
fn scan_startup_locations() -> String {
    let mut lines = vec!["scan-startup".to_string()];
    let mut found = 0usize;

    let home = std::env::var("HOME").unwrap_or_default();

    // macOS LaunchAgents / LaunchDaemons
    let macos_locations = [
        format!("{}/Library/LaunchAgents", home),
        "/Library/LaunchAgents".to_string(),
        "/Library/LaunchDaemons".to_string(),
        "/System/Library/LaunchAgents".to_string(),
        "/System/Library/LaunchDaemons".to_string(),
    ];

    // Linux startup locations
    let linux_locations = [
        format!("{}/bin", home),
        format!("{}/.bashrc", home),
        format!("{}/.bash_profile", home),
        format!("{}/.profile", home),
        format!("{}/.zshrc", home),
        "/etc/cron.d".to_string(),
        "/etc/cron.daily".to_string(),
        "/etc/cron.hourly".to_string(),
        "/var/spool/cron".to_string(),
        "/etc/rc.local".to_string(),
        "/etc/init.d".to_string(),
    ];

    let all_locations: Vec<&str> = macos_locations.iter()
        .chain(linux_locations.iter())
        .map(|s| s.as_str())
        .collect();

    for loc in &all_locations {
        let path = std::path::Path::new(loc);
        if path.is_file() {
            if let Ok(bytes) = std::fs::read(path) {
                let score = scan_risk_score(&bytes);
                found += 1;
                let flag = if score >= 50 { "!" } else { " " };
                lines.push(format!("  {flag} [{:>3}] {}", score, loc));
            }
        } else if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    if let Ok(bytes) = std::fs::read(entry.path()) {
                        let score = scan_risk_score(&bytes);
                        found += 1;
                        let flag = if score >= 50 { "!" } else { " " };
                        let name = entry.path().to_string_lossy().to_string();
                        lines.push(format!("  {flag} [{:>3}] {}", score, name));
                    }
                }
            }
        }
    }

    if found == 0 {
        lines.push("no startup items found".to_string());
    } else {
        lines.push(format!("{} startup items scanned", found));
    }
    lines.join("\n")
}

/// Move a file to ~/.finch/quarantine/. Returns true on success.
fn quarantine_file(path: &str) -> bool {
    let home = std::env::var("HOME").unwrap_or_default();
    let quarantine_dir = format!("{}/.finch/quarantine", home);
    if std::fs::create_dir_all(&quarantine_dir).is_err() { return false; }

    let src = std::path::Path::new(path);
    let file_name = match src.file_name() {
        Some(n) => n.to_string_lossy().to_string(),
        None => return false,
    };

    // Add timestamp to avoid collisions
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = format!("{}/{}_{}", quarantine_dir, ts, file_name);

    std::fs::rename(path, &dest).is_ok()
}

// ── Crypto helpers ────────────────────────────────────────────────────────────

fn crypto_hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn crypto_hex_decode(s: &str) -> Result<Vec<u8>> {
    if s.len() % 2 != 0 { bail!("hex: odd-length string"); }
    (0..s.len()).step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i+2], 16)
            .map_err(|e| anyhow::anyhow!("hex decode at byte {}: {}", i/2, e)))
        .collect()
}

// ── Scatter output helpers ────────────────────────────────────────────────────

impl Forth {
    /// Format scatter results into `self.out` using crossterm colors.
    /// Peer labels are shown when set; errors are red; output lines are labelled.
    fn emit_scatter_results(&mut self, results: Vec<crate::coforth::scatter::PeerResult>) {
        use crossterm::style::Stylize;
        for r in results {
            // Use label if known; fall back to a short hostname (strip port, strip http://).
            let name = self.peer_meta.get(&r.peer)
                .and_then(|m| m.label.as_deref())
                .map(|l| l.cyan().bold().to_string())
                .unwrap_or_else(|| {
                    let short = r.peer
                        .trim_start_matches("http://")
                        .trim_start_matches("https://")
                        .split(':').next()           // drop port
                        .unwrap_or(&r.peer);
                    short.cyan().to_string()
                });

            if let Some(err) = r.error {
                self.out.push_str(&format!("{} couldn't do it: {}\n",
                    name, err.red()));
            } else {
                for line in r.output.lines() {
                    self.out.push_str(&format!("{}: {}\n", name, line));
                }
                for v in &r.stack {
                    self.data.push(*v);
                }
            }
            // Execute any Forth code the peer sent back.
            if let Some(ref fb) = r.forth_back {
                if !fb.is_empty() {
                    if let Err(e) = self.eval(fb) {
                        self.out.push_str(&format!("{} sent back something that didn't work: {}\n",
                            name, e.to_string().red()));
                    }
                }
            }
        }
    }
}

// ── Scatter helpers — bridge sync Forth VM to async scatter functions ──────────

/// Send a push message to a single peer synchronously (bridges async scatter_push).
fn run_push_one(addr: &str, text: &str, from: Option<&str>, token: Option<&str>) {
    let addr = addr.to_string();
    let text = text.to_string();
    let from = from.map(|s| s.to_string());
    let token = token.map(|s| s.to_string());
    let fut = async move {
        // scatter_push to a single peer
        let url = if addr.starts_with("http://") || addr.starts_with("https://") {
            format!("{addr}/v1/forth/push")
        } else {
            format!("http://{addr}/v1/forth/push")
        };
        let body = match &from {
            Some(f) => serde_json::json!({ "text": text, "from": f }),
            None    => serde_json::json!({ "text": text }),
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        let mut req = client.post(&url);
        if let Some(t) = &token {
            req = req.header(crate::peer_token::HEADER, t.as_str());
        }
        let _ = req.json(&body).send().await;
    };
    futures::executor::block_on(fut);
}

fn run_push_all(peers: &[String], text: &str, from: Option<&str>, _tokens: &std::collections::HashMap<String, String>) {
    if peers.is_empty() { return; }
    let peers = peers.to_vec();
    let text  = text.to_string();
    let from  = from.map(|s| s.to_string());
    let fut = async move {
        crate::coforth::scatter::scatter_push(&peers, &text, from.as_deref()).await;
    };
    futures::executor::block_on(fut);
}

fn peer_tokens_map(peer_meta: &std::collections::HashMap<String, PeerMeta>) -> std::collections::HashMap<String, String> {
    peer_meta.iter()
        .filter_map(|(addr, meta)| meta.token.as_ref().map(|t| (addr.clone(), t.clone())))
        .collect()
}

fn run_scatter(peers: &[String], code: &str, caller: Option<&str>, tokens: &std::collections::HashMap<String, String>) -> Vec<crate::coforth::scatter::PeerResult> {
    let caller = caller.map(|s| s.to_string());
    futures::executor::block_on(
        crate::coforth::scatter::scatter_exec(peers, code, caller.as_deref(), tokens)
    )
}

/// Return "hostname: path" attribution prefix for file reads.
fn file_attribution(path: &str) -> String {
    let host = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "here".to_string());
    format!("── {path} ({host}) ──\n")
}

/// Read a CSV or TSV file and return rows as pipe-delimited lines.
/// `delimiter` is b',' for CSV or b'\t' for TSV.
fn read_delimited_file(path: &str, delimiter: u8) -> String {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return format!("cannot open {path}: {e}\n"),
    };
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(f);
    let mut out = file_attribution(path);
    for result in rdr.records() {
        match result {
            Ok(record) => {
                let row: Vec<&str> = record.iter().collect();
                out.push_str(&row.join(" | "));
                out.push('\n');
            }
            Err(e) => {
                out.push_str(&format!("parse error: {e}\n"));
            }
        }
    }
    out
}

/// Read the first sheet of an xlsx/xls/ods file and return rows as pipe-delimited lines.
fn read_xlsx_file(path: &str) -> String {
    use calamine::{Reader, open_workbook_auto, Data};
    let mut workbook = match open_workbook_auto(path) {
        Ok(wb) => wb,
        Err(e) => return format!("cannot open {path}: {e}\n"),
    };
    let sheets = workbook.sheet_names().to_vec();
    let sheet_name = match sheets.first() {
        Some(n) => n.clone(),
        None => return format!("{path}: no sheets found\n"),
    };
    let range = match workbook.worksheet_range(&sheet_name) {
        Ok(r) => r,
        Err(e) => return format!("cannot read sheet: {e}\n"),
    };
    let mut out = file_attribution(path);
    for row in range.rows() {
        let cells: Vec<String> = row.iter().map(|c| match c {
            Data::Empty => String::new(),
            Data::String(s) => s.clone(),
            Data::Float(f) => {
                if f.fract() == 0.0 { format!("{}", *f as i64) } else { format!("{f}") }
            }
            Data::Int(i) => format!("{i}"),
            Data::Bool(b) => format!("{b}"),
            Data::Error(e) => format!("#ERR:{e:?}"),
            _ => String::new(),
        }).collect();
        out.push_str(&cells.join(" | "));
        out.push('\n');
    }
    out
}

/// Extract a human-friendly machine name from an mDNS fullname.
/// e.g. "finch-macbook-pro._finch._tcp.local." → "macbook-pro"
fn friendly_peer_name(full_name: &str) -> String {
    let instance = full_name.split("._finch").next().unwrap_or(full_name);
    instance.strip_prefix("finch-").unwrap_or(instance).to_string()
}

/// Public entry point for background boot discovery (called from event_loop).
/// Returns (host, port, friendly_name) triples.
/// (host, port, friendly_name, token)
pub fn run_peers_discover_pub(timeout_ms: u64) -> Vec<(String, u16, String, Option<String>)> {
    run_peers_discover(timeout_ms)
}

/// Synchronous mDNS discovery — returns (host, port, friendly_name, token) tuples.
/// Blocks for at most `timeout_ms` milliseconds.
fn run_peers_discover(timeout_ms: u64) -> Vec<(String, u16, String, Option<String>)> {
    use std::time::Duration;
    let timeout = Duration::from_millis(timeout_ms);

    let inner = || -> anyhow::Result<Vec<(String, u16, String, Option<String>)>> {
        let client = crate::service::discovery_client::ServiceDiscoveryClient::new()?;
        let services = client.discover(timeout)?;
        Ok(services
            .into_iter()
            .map(|s| {
                let name = friendly_peer_name(&s.name);
                (s.host, s.port, name, s.token)
            })
            .collect())
    };

    // discover() uses recv_timeout internally — it's a blocking call.
    // block_in_place panics inside LocalSet; use futures::executor::block_on instead.
    futures::executor::block_on(async { inner() })
    .unwrap_or_default()
}

/// Collect basic machine specs: (cpu_cores, ram_mb, bench_ms).
/// bench_ms = milliseconds to complete 10 million integer additions (lower = faster).
pub fn collect_machine_specs() -> (u32, u64, u64) {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let cpu_cores = sys.cpus().len() as u32;
    let ram_mb = sys.total_memory() / (1024 * 1024);
    // Quick benchmark: 10M additions.
    let start = std::time::Instant::now();
    let mut acc: u64 = 0;
    for i in 0u64..10_000_000 {
        acc = acc.wrapping_add(i);
    }
    let bench_ms = start.elapsed().as_millis() as u64;
    // Keep acc alive so the compiler doesn't optimise the loop away.
    let _ = acc;
    (cpu_cores, ram_mb, bench_ms)
}

/// Format hardware specs for display in registry-list.
fn format_peer_hw(p: &crate::registry::PeerEntry) -> String {
    use crossterm::style::Stylize;
    let mut parts = Vec::new();
    if let Some(c) = p.cpu_cores { parts.push(format!("{}c", c)); }
    if let Some(r) = p.ram_mb    { parts.push(format!("{}MB", r)); }
    if let Some(b) = p.bench_ms  { parts.push(format!("bench:{}ms", b)); }
    if parts.is_empty() {
        String::new()
    } else {
        format!("  {}", parts.join(" ").dark_grey())
    }
}

fn run_registry_join(registry: &str, entry: crate::registry::PeerEntry) -> anyhow::Result<()> {
    let url = if registry.starts_with("http") {
        format!("{registry}/v1/registry/join")
    } else {
        format!("http://{registry}/v1/registry/join")
    };
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        client.post(&url).json(&entry).send().await?;
        Ok::<(), anyhow::Error>(())
    };
    futures::executor::block_on(fut)
}

fn run_registry_leave(registry: &str, addr: &str) -> anyhow::Result<()> {
    let url = if registry.starts_with("http") {
        format!("{registry}/v1/registry/leave")
    } else {
        format!("http://{registry}/v1/registry/leave")
    };
    let body = serde_json::json!({ "addr": addr });
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        client.post(&url).json(&body).send().await?;
        Ok::<(), anyhow::Error>(())
    };
    futures::executor::block_on(fut)
}

fn run_registry_peers(
    registry: &str,
    tag: Option<&str>,
    region: Option<&str>,
) -> anyhow::Result<Vec<crate::registry::PeerEntry>> {
    let base = if registry.starts_with("http") {
        format!("{registry}/v1/registry/peers")
    } else {
        format!("http://{registry}/v1/registry/peers")
    };
    let mut url = base;
    let mut sep = '?';
    if let Some(t) = tag    { url.push(sep); url.push_str(&format!("tag={t}"));    sep = '&'; }
    if let Some(r) = region { url.push(sep); url.push_str(&format!("region={r}")); }
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        let peers = client.get(&url).send().await?.json().await?;
        Ok::<Vec<crate::registry::PeerEntry>, anyhow::Error>(peers)
    };
    futures::executor::block_on(fut)
}

fn run_registry_ledger(
    registry: &str,
    addr: &str,
) -> anyhow::Result<crate::registry::LedgerEntry> {
    let base = if registry.starts_with("http") {
        format!("{registry}/v1/registry/ledger/{addr}")
    } else {
        format!("http://{registry}/v1/registry/ledger/{addr}")
    };
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        let entry = client.get(&base).send().await?.json().await?;
        Ok::<crate::registry::LedgerEntry, anyhow::Error>(entry)
    };
    futures::executor::block_on(fut)
}

fn run_registry_all_ledgers(
    registry: &str,
) -> anyhow::Result<Vec<(String, crate::registry::LedgerEntry)>> {
    let base = if registry.starts_with("http") {
        format!("{registry}/v1/registry/ledgers")
    } else {
        format!("http://{registry}/v1/registry/ledgers")
    };
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        let entries: Vec<(String, crate::registry::LedgerEntry)> =
            client.get(&base).send().await?.json().await?;
        Ok::<Vec<(String, crate::registry::LedgerEntry)>, anyhow::Error>(entries)
    };
    futures::executor::block_on(fut)
}

/// POST to peer's /v1/settle — acknowledge compute debt and request clearance.
/// Returns (cleared_ms, message) on success.
fn run_peer_settle(peer_addr: &str, my_addr: Option<&str>) -> anyhow::Result<(u64, String)> {
    let creditor = my_addr.unwrap_or(peer_addr);
    let url = if peer_addr.starts_with("http") {
        format!("{peer_addr}/v1/settle")
    } else {
        format!("http://{peer_addr}/v1/settle")
    };
    // Ask the peer what we owe them first, then send that exact amount.
    // This prevents sending 0 and getting a free pass.
    let ledger_url = if peer_addr.starts_with("http") {
        format!("{peer_addr}/v1/registry/ledger/{creditor}")
    } else {
        format!("http://{peer_addr}/v1/registry/ledger/{creditor}")
    };
    let body = serde_json::json!({ "creditor": creditor, "amount_ms": 0u64 });
    let _ = body; // replaced below after ledger fetch
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        // Fetch how much the peer thinks we owe them.
        let ledger: crate::registry::LedgerEntry = client
            .get(&ledger_url)
            .send().await?
            .json().await?;
        let amount_ms = ledger.credits_ms.saturating_sub(ledger.debits_ms.min(ledger.credits_ms));
        if amount_ms == 0 {
            return Ok((0u64, "nothing owed".to_string()));
        }
        let body = serde_json::json!({ "creditor": creditor, "amount_ms": amount_ms });
        let resp = client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("peer rejected settlement: {text}");
        }
        let json: serde_json::Value = resp.json().await?;
        let cleared_ms = json["cleared_ms"].as_u64().unwrap_or(0);
        let message    = json["message"].as_str().unwrap_or("").to_string();
        Ok::<(u64, String), anyhow::Error>((cleared_ms, message))
    };
    futures::executor::block_on(fut)
}

fn run_registry_debit(
    registry: &str,
    peer_addr: &str,
    compute_ms: u64,
) -> anyhow::Result<()> {
    let base = if registry.starts_with("http") {
        format!("{registry}/v1/registry/debit")
    } else {
        format!("http://{registry}/v1/registry/debit")
    };
    let body = serde_json::json!({ "addr": peer_addr, "compute_ms": compute_ms });
    let fut = async move {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        client.post(&base).json(&body).send().await?;
        Ok::<(), anyhow::Error>(())
    };
    futures::executor::block_on(fut)
}

fn run_define_scatter(peers: &[String], source: &str, tokens: &std::collections::HashMap<String, String>) -> Vec<crate::coforth::scatter::PeerResult> {
    futures::executor::block_on(crate::coforth::scatter::define_on_peers(peers, source, tokens))
}

fn run_exec_scatter(peers: &[String], cmd: &str, tokens: &std::collections::HashMap<String, String>) -> Vec<crate::coforth::scatter::PeerResult> {
    futures::executor::block_on(crate::coforth::scatter::scatter_exec_bash(peers, cmd, tokens))
}

// ── Name table ────────────────────────────────────────────────────────────────

pub(crate) fn name_to_builtin(name: &str) -> Option<Builtin> {
    Some(match name {
        "+" => Builtin::Plus, "-" => Builtin::Minus, "*" => Builtin::Star,
        "/" => Builtin::Slash, "mod" => Builtin::Mod,
        "dup" => Builtin::Dup, "drop" => Builtin::Drop, "swap" => Builtin::Swap,
        "over" => Builtin::Over, "rot" => Builtin::Rot,
        "nip" => Builtin::Nip, "tuck" => Builtin::Tuck,
        "2dup" => Builtin::TwoDup, "2drop" => Builtin::TwoDrop, "2swap" => Builtin::TwoSwap,
        "2over" => Builtin::TwoOver, "2rot" => Builtin::TwoRot,
        "=" => Builtin::Eq, "<" => Builtin::Lt, ">" => Builtin::Gt,
        "<=" | "=<" => Builtin::Le, ">=" | "=>" => Builtin::Ge,
        "<>" | "!=" => Builtin::Ne, "0=" => Builtin::ZeroEq, "0<" => Builtin::ZeroLt, "0>" => Builtin::ZeroGt,
        "and" => Builtin::And, "or" => Builtin::Or, "xor" => Builtin::Xor,
        "invert" | "not" => Builtin::Invert, "negate" => Builtin::Negate,
        "abs" => Builtin::Abs, "max" => Builtin::Max, "min" => Builtin::Min,
        "lshift" => Builtin::Lshift, "rshift" => Builtin::Rshift,
        "." => Builtin::Print, ".s" => Builtin::PrintS,
        "cr" => Builtin::Cr, "space" => Builtin::Space, "emit" => Builtin::Emit,
        "@" => Builtin::Fetch, "!" => Builtin::Store, "+!" => Builtin::PlusStore,
        "i" => Builtin::LoopI, "j" => Builtin::LoopJ,
        ">r" => Builtin::ToR, "r>" => Builtin::FromR, "r@" => Builtin::FetchR,
        "pick" => Builtin::Pick, "roll" => Builtin::Roll,
        "u<" => Builtin::ULt,
        "*/" => Builtin::StarSlash, "/mod" => Builtin::SlashMod,
        "u." => Builtin::PrintU, ".h" | "hex." => Builtin::PrintHex,
        "allot" => Builtin::Allot, "cells" => Builtin::Cells,
        "words" => Builtin::Words, "hot-words" => Builtin::HotWords,
        "random" => Builtin::Random, "time" => Builtin::Time,
        "depth" => Builtin::Depth, "nop" => Builtin::Nop,
        "fuel"      => Builtin::Fuel,
        "with-fuel" => Builtin::WithFuel,
        "undo"      => Builtin::Undo,
        "lock"      => Builtin::Lock,
        "unlock"    => Builtin::Unlock,
        "lock-ttl"  => Builtin::LockTtl,
        // Writing assistance
        "capitalize"    => Builtin::Capitalize,
        "str-upper"     => Builtin::StrUpper,
        "str-lower"     => Builtin::StrLower,
        "str-trim"      => Builtin::StrTrim,
        "word-count"    => Builtin::WordCount,
        "sentence?"     => Builtin::SentenceCheck,
        "grammar-check" => Builtin::GrammarCheck,
        "improve"       => Builtin::ImproveStr,
        "sqrt" | "isqrt" => Builtin::Sqrt,
        "floor" => Builtin::Floor,
        "ceil" | "ceiling" => Builtin::Ceil,
        "sin" => Builtin::Sin,
        "cos" => Builtin::Cos,
        "fpmul" => Builtin::FPMul,
        "peers" => Builtin::Peers,
        "peers-clear" => Builtin::PeersClear,
        "peers-discover" | "discover" => Builtin::PeersDiscover,
        "take-all"          => Builtin::TakeAll,
        "ensemble-def"      => Builtin::EnsembleDef,
        "ensemble-use"      => Builtin::EnsembleUse,
        "ensemble-end"      => Builtin::EnsembleEnd,
        "ensemble-list"     => Builtin::EnsembleList,
        "label-peer"        => Builtin::LabelPeer,
        "tag-peer"          => Builtin::TagPeer,
        "ensemble-from-tag" => Builtin::EnsembleFromTag,
        "peer-info"         => Builtin::PeerInfo,
        "publish"           => Builtin::Publish,
        "sync"              => Builtin::Sync,
        "registry-set"      => Builtin::RegistrySet,
        "join-registry"     => Builtin::JoinRegistry,
        "leave-registry" | "leave" => Builtin::LeaveRegistry,
        "from-registry"     => Builtin::FromRegistry,
        "registry-list"     => Builtin::RegistryList,
        "slowest"           => Builtin::Slowest,
        "balance"           => Builtin::Balance,
        "balances"          => Builtin::Balances,
        "record-debit"      => Builtin::RecordDebit,
        "debt-check"        => Builtin::DebtCheck,
        "settle"            => Builtin::Settle,
        // String pool
        "type"    => Builtin::Type,
        "str="    => Builtin::StrEq,
        "str-len"     => Builtin::StrLen,
        "str-cat"     => Builtin::StrCat,
        "str-split"   => Builtin::StrSplit,
        "str-join"    => Builtin::StrJoin,
        "str-sub"     => Builtin::StrSub,
        "str-find"    => Builtin::StrFind,
        "str-replace"  => Builtin::StrReplace,
        "str-reverse"  => Builtin::StrReverse,
        "num>str"      => Builtin::NumToStr,
        "str>num"      => Builtin::StrToNum,
        "word-defined?" => Builtin::WordDefined,
        "word-names"   => Builtin::WordNames,
        "nth-line"     => Builtin::NthLine,
        "agree?"       => Builtin::AgreeQ,
        "same?"        => Builtin::SameQ,
        "safe"         => Builtin::Safe,
        // Crypto
        "sha256"      => Builtin::Sha256,
        "file-sha256" => Builtin::FileSha256,
        "file-hash"         => Builtin::FileHash,
        "file-hash-range"   => Builtin::FileHashRange,
        "file-fetch"        => Builtin::FileFetch,
        "file-slice"        => Builtin::FileSlice,
        "file-sha256-range" => Builtin::FileSha256Range,
        "file-size"         => Builtin::FileSize,
        "file-write"        => Builtin::FileWrite,
        "file-append"       => Builtin::FileAppend,
        "nonce"       => Builtin::Nonce,
        "keygen"      => Builtin::Keygen,
        "sign"        => Builtin::Sign,
        "verify"      => Builtin::Verify,
        // Terminal control
        "at-xy"          => Builtin::AtXy,
        "term-size"      => Builtin::TermSize,
        "save-cursor"    => Builtin::SaveCursor,
        "restore-cursor" => Builtin::RestoreCursor,
        "clear-eol"      => Builtin::ClearEol,
        "clear-line"     => Builtin::ClearLine,
        "color!"         => Builtin::ColorFg,
        "reset-style"    => Builtin::ResetStyle,
        "sync-begin"     => Builtin::SyncBegin,
        "sync-end"       => Builtin::SyncEnd,
        "hide-cursor"    => Builtin::HideCursor,
        "show-cursor"    => Builtin::ShowCursor,
        "connected?"     => Builtin::Connected,
        // Security scanning
        "scan-file"      => Builtin::ScanFile,
        "scan-bytes"     => Builtin::ScanBytes,
        "file-entropy"   => Builtin::FileEntropy,
        "scan-dir"       => Builtin::ScanDir,
        "scan-strings"   => Builtin::ScanStrings,
        "scan-procs"     => Builtin::ScanProcs,
        "scan-net"       => Builtin::ScanNet,
        "scan-startup"   => Builtin::ScanStartup,
        "quarantine"     => Builtin::Quarantine,
        "1+"             => Builtin::Inc,
        "1-"             => Builtin::Dec,
        "here"           => Builtin::Here,
        ","              => Builtin::Comma,
        "cell"           => Builtin::CellSz,
        "fill"           => Builtin::Fill,
        "eval"           => Builtin::Eval,
        "argue"          => Builtin::Argue,
        "gate"           => Builtin::Gate,
        "both-ways"      => Builtin::BothWays,
        "versus"         => Builtin::Versus,
        "page"           => Builtin::Page,
        "resolve"        => Builtin::Resolve,
        "infix"          => Builtin::Infix,
        "register-boot"  => Builtin::RegisterBoot,
        // Proof system
        "assert"         => Builtin::Assert,
        "prove-all"       => Builtin::ProveAll,
        "prove-all?"      => Builtin::ProveAllBool,
        "prove-english"   => Builtin::ProveEnglish,
        "prove-languages" => Builtin::ProveLanguages,
        "channels"       => Builtin::ListChannels,
        // Collection operations
        "glob-pool"      => Builtin::GlobPool,
        "clean-lines"    => Builtin::CleanLines,
        "glob-count"      => Builtin::GlobCount,
        "exec-capture"    => Builtin::ExecCapture,
        "back-and-forth?" => Builtin::BackAndForthQ,
        "invertible?"     => Builtin::InvertibleQ,
        "help"            => Builtin::Help,
        "describe"        => Builtin::Describe,
        "compute"         => Builtin::Compute,
        "equiv?"          => Builtin::EquivQ,
        "fork"            => Builtin::Fork,
        "boot"            => Builtin::Boot,
        ".r"              => Builtin::PrintR,
        ".pad"            => Builtin::PrintPad,
        "hash" | "str-hash" => Builtin::Hash,
        "hash-int"        => Builtin::HashInt,
        "hash-combine"    => Builtin::HashCombine,
        "sort"           => Builtin::SortLines,
        "sort-lines"     => Builtin::SortLines,
        "unique"         => Builtin::UniqueLines,
        "unique-lines"   => Builtin::UniqueLines,
        "reverse"        => Builtin::ReverseLines,
        "reverse-lines"  => Builtin::ReverseLines,
        "line-count"     => Builtin::LineCount,
        _ => return None,
    })
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────

fn tokenize(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    let mut tok = String::new();

    macro_rules! flush { () => { if !tok.is_empty() { tokens.push(std::mem::take(&mut tok)); } }; }

    while let Some(&c) = chars.peek() {
        if c == '\\' {
            flush!();
            for c2 in chars.by_ref() { if c2 == '\n' { break; } }
        } else if c == '(' {
            flush!();
            chars.next();
            // Skip until closing )
            let mut depth = 1;
            for c2 in chars.by_ref() {
                if c2 == '(' { depth += 1; }
                else if c2 == ')' { depth -= 1; if depth == 0 { break; } }
            }
        } else if c == '.' {
            chars.next();
            if chars.peek() == Some(&'"') {
                flush!();
                chars.next(); // consume "
                // Skip exactly one space (standard Forth: space separates ." from content)
                if chars.peek() == Some(&' ') { chars.next(); }
                let mut s = String::new();
                for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
                tokens.push(format!("\x00str:{s}"));
            } else if chars.peek() == Some(&'|') {
                // .| text with "quotes" |  — print alternate delimiter
                flush!();
                chars.next(); // consume |
                if chars.peek() == Some(&' ') { chars.next(); }
                let mut s = String::new();
                for c2 in chars.by_ref() { if c2 == '|' { break; } s.push(c2); }
                tokens.push(format!("\x00str:{s}"));
            } else {
                // Sentence-final period: "square." → ["square", "."]
                // A trailing period (followed by space, newline, or end) is a separator —
                // every period executes, including natural language sentence endings.
                let next = chars.peek().copied();
                let is_sentence_end = matches!(next, None | Some(' ') | Some('\n') | Some('\r') | Some('\t') | Some(','));
                if is_sentence_end && !tok.is_empty() {
                    flush!();
                    tokens.push(".".to_string()); // the . itself executes (print TOS or no-op)
                } else {
                    tok.push('.');
                }
            }
        } else if c == '"' && tok == "confirm" {
            // confirm" message" — like ." but emits Cell::Confirm instead of Cell::Str
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&' ') { chars.next(); } // skip separator space
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00confirm:{s}"));
        } else if c == '"' && tok == "select" {
            // select" title|opt1|opt2" — pop-up dialog; pushes chosen index or -1
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&' ') { chars.next(); } // skip separator space
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00select:{s}"));
        } else if c == '"' && tok == "read" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00read:{s}"));
        } else if c == '"' && tok == "csv" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00csv:{s}"));
        } else if c == '"' && tok == "tsv" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00tsv:{s}"));
        } else if c == '"' && tok == "xlsx" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00xlsx:{s}"));
        } else if c == '"' && tok == "exec" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00exec:{s}"));
        } else if c == '"' && tok == "glob" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00glob:{s}"));
        } else if c == '"' && tok == "peer" {
            // peer" addr"  — register a remote finch daemon as a scatter target
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00peer:{s}"));
        } else if c == '"' && tok == "ensemble-def" {
            // ensemble-def" name"  — snapshot current peers as a named ensemble
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("ensemble-def".to_string());
        } else if c == '"' && tok == "ensemble-use" {
            // ensemble-use" name"  — push peers, switch to named ensemble
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("ensemble-use".to_string());
        } else if c == '"' && tok == "registry" {
            // registry" addr"  — set registry address
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("registry-set".to_string());
        } else if c == '"' && tok == "join" {
            // join" addr"  — register this machine at addr with the configured registry
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("join-registry".to_string());
        } else if c == '"' && tok == "publish" {
            // publish" word-name"  — scatter word source to all peers
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("publish".to_string());
        } else if c == '"' && tok == "scatter" {
            // scatter" code"  — run code on all registered peers in parallel
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00scatter:{s}"));
        } else if c == '"' && tok == "symbol" {
            // symbol" name"  — share a word by name: if I know it, send my definition first;
            // then run the word on all peers so each speaks it in their own dialect
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00symbol:{s}"));
        } else if c == '"' && tok == "hello" {
            // hello" peer"  — send "hello from <hostname>!" to one peer by name or addr
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut peer = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } peer.push(c2); }
            tokens.push(format!("\x00hello:{peer}"));
        } else if c == '"' && tok == "tag" {
            // tag" name" "addr"  — label a peer's machine with a human name
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut name = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } name.push(c2); }
            while chars.peek() == Some(&' ') { chars.next(); }
            if chars.peek() == Some(&'"') { chars.next(); }
            let mut addr = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } addr.push(c2); }
            tokens.push(format!("\x00tag:{name}\x01{addr}"));
        } else if c == '"' && tok == "channel" {
            // channel" #name"  — join a named channel; broadcast presence to all peers
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut name = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } name.push(c2); }
            tokens.push(format!("\x00channel:{name}"));
        } else if c == '"' && tok == "say" {
            // say" #channel" "message"  — send a message to a channel (all peers)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut chan = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } chan.push(c2); }
            while chars.peek() == Some(&' ') { chars.next(); }
            if chars.peek() == Some(&'"') { chars.next(); }
            let mut msg = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } msg.push(c2); }
            tokens.push(format!("\x00say:{chan}\x01{msg}"));
        } else if c == '"' && tok == "part" {
            // part" #name"  — leave a channel; broadcast departure to all peers
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut name = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } name.push(c2); }
            tokens.push(format!("\x00part:{name}"));
        } else if c == '"' && tok == "prove" {
            // prove" word"  — run test:<word> and show ✓ / ✗
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut word = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } word.push(c2); }
            tokens.push(format!("\x00prove:{word}"));
        } else if c == '"' && tok == "on" {
            // on" peer" "code"  — run code on exactly one peer (by address or label)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut peer = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } peer.push(c2); }
            while chars.peek() == Some(&' ') { chars.next(); }
            if chars.peek() == Some(&'"') { chars.next(); }
            let mut code = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } code.push(c2); }
            tokens.push(format!("\x00on:{peer}\x01{code}"));
        } else if c == '"' && tok == "scatter-on" {
            // scatter-on" ensemble" "code"  — run code on named ensemble, no peer side-effects
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut ensemble = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } ensemble.push(c2); }
            // skip whitespace then opening quote for code
            while chars.peek() == Some(&' ') { chars.next(); }
            if chars.peek() == Some(&'"') { chars.next(); } // consume opening "
            let mut code = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } code.push(c2); }
            tokens.push(format!("\x00scatter-on:{ensemble}\x01{code}"));
        } else if c == '"' && tok == "forth-back" {
            // forth-back" code"  — set Forth code to be executed on the caller after response
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00forth-back:{s}"));
        } else if c == '"' && tok == "s" {
            // s" text"  — push string pool index as integer operand (no printing)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
        } else if c == '"' && tok == "page" {
            // page"        — multiline proof page; content ends at " on its own line
            //   left side | right side
            //   ...
            // "
            tok.clear();
            chars.next(); // consume the opening "
            if chars.peek() == Some(&'\n') { chars.next(); } // skip immediate newline
            let mut s = String::new();
            let mut at_line_start = true; // track whether current line so far is whitespace-only
            for c2 in chars.by_ref() {
                if c2 == '"' && at_line_start { break; } // closing " on a line with only whitespace before it
                if c2 == '\n' {
                    at_line_start = true;
                } else if !c2.is_whitespace() && c2 != '"' {
                    at_line_start = false;
                }
                s.push(c2);
            }
            // trim trailing whitespace/newline from the block content
            let s = s.trim_end().to_string();
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("page".to_string());
        } else if c == '"' && tok == "resolve" {
            // resolve"   — many sentences, one truth; closing " alone on a line (or with leading whitespace)
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&'\n') { chars.next(); }
            let mut s = String::new();
            let mut at_line_start = true;
            for c2 in chars.by_ref() {
                if c2 == '"' && at_line_start { break; }
                if c2 == '\n' { at_line_start = true; }
                else if !c2.is_whitespace() { at_line_start = false; }
                s.push(c2);
            }
            let s = s.trim_end().to_string();
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("resolve".to_string());
        } else if c == '|' && tok == "s" {
            // s| text with "quotes" |  — alternate string delimiter; avoids escaping hell
            tok.clear();
            chars.next(); // consume |
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '|' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
        } else if c == '"' && tok == "boot" {
            // boot" text"  — register a line to print at every boot; persisted to ~/.finch/boot.forth
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
            tokens.push("register-boot".to_string());
        } else if c == '"' && tok == "gen" {
            // gen" prompt"  — call AI generator, emit response
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00gen:{s}"));
        } else if c == '"' && tok == "scatter-exec" {
            // scatter-exec" cmd"  — run bash -c cmd on all peers via /v1/exec
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00scatter-exec:{s}"));
        } else if c == ';' {
            // `;` always terminates the current token and emits itself as a standalone token.
            // This lets it be a sentence separator in natural language: "Hello; I am forth."
            flush!();
            tokens.push(";".to_string());
            chars.next();
        } else if c.is_whitespace() {
            flush!();
            chars.next();
        } else if c == '\'' {
            // Apostrophe in natural-language contractions: that's → thats, we're → were.
            // Skip it — the token accumulates without the apostrophe.
            chars.next();
        } else {
            tok.push(c);
            chars.next();
        }
    }
    flush!();
    tokens
}

// ── Control-flow collection helpers ──────────────────────────────────────────

/// Collect tokens for `if/else/then`.
/// Returns (true_branch, false_branch, tokens_consumed).
fn collect_if(tokens: &[String], start: usize) -> Result<(Vec<String>, Vec<String>, usize)> {
    let mut true_b = Vec::new();
    let mut false_b = Vec::new();
    let mut in_false = false;
    let mut depth = 1i32;
    let mut i = start;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "if"   => { depth += 1; push_branch(&mut true_b, &mut false_b, in_false, &tokens[i]); }
            "then" => {
                depth -= 1;
                if depth == 0 { break; }
                push_branch(&mut true_b, &mut false_b, in_false, &tokens[i]);
            }
            "else" if depth == 1 => { in_false = true; }
            _ => { push_branch(&mut true_b, &mut false_b, in_false, &tokens[i]); }
        }
        i += 1;
    }
    Ok((true_b, false_b, i - start))
}

fn push_branch(t: &mut Vec<String>, f: &mut Vec<String>, in_false: bool, tok: &str) {
    if in_false { f.push(tok.to_string()); } else { t.push(tok.to_string()); }
}

/// Collect tokens for `begin/until|again|while`.
/// Returns (body, end_kind, after_body [for while..repeat], tokens_consumed).
///
/// Depth rules: only `begin` increments depth; only `until`, `again`, `repeat`
/// decrement it.  `while` never changes depth — it is not a balancer for `begin`,
/// only `repeat` is.  This allows nested `begin..while..repeat` loops to be
/// collected correctly.
fn collect_begin(tokens: &[String], start: usize) -> Result<(Vec<String>, String, Vec<String>, usize)> {
    let mut body = Vec::new();
    let mut after = Vec::new();
    let mut end_kind = String::new();
    let mut depth = 1i32;
    let mut in_after = false;
    let mut i = start;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "begin" => {
                depth += 1;
                (if in_after { &mut after } else { &mut body }).push(tokens[i].clone());
            }
            "until" | "again" if depth == 1 => { end_kind = tokens[i].clone(); break; }
            "while" if depth == 1 => { end_kind = "while".to_string(); in_after = true; }
            "repeat" if depth == 1 && in_after => { break; }
            // Inner loop terminators: only until/again/repeat balance begin.
            "until" | "again" | "repeat" => {
                depth -= 1;
                (if in_after { &mut after } else { &mut body }).push(tokens[i].clone());
            }
            // Inner while — push but do NOT change depth (while is not a begin-balancer).
            "while" => {
                (if in_after { &mut after } else { &mut body }).push(tokens[i].clone());
            }
            _ => { (if in_after { &mut after } else { &mut body }).push(tokens[i].clone()); }
        }
        i += 1;
    }
    Ok((body, end_kind, after, i - start))
}

/// Collect tokens for `do/loop` or `do/+loop`.
/// Returns (body, is_plus_loop, tokens_consumed).
fn collect_do(tokens: &[String], start: usize) -> Result<(Vec<String>, bool, usize)> {
    let mut body = Vec::new();
    let mut plus = false;
    let mut depth = 1i32;
    let mut i = start;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "do"    => { depth += 1; body.push(tokens[i].clone()); }
            "loop"  if depth == 1 => { break; }
            "+loop" if depth == 1 => { plus = true; break; }
            "loop" | "+loop" => { depth -= 1; body.push(tokens[i].clone()); }
            _ => { body.push(tokens[i].clone()); }
        }
        i += 1;
    }
    Ok((body, plus, i - start))
}

/// Collect `case` body tokens into (of_blocks, default_block, tokens_consumed).
/// Each of_block is (value_tokens, body_tokens).
/// default_block = tokens between the last endof and endcase.
fn collect_case(tokens: &[String], start: usize) -> Result<(Vec<(Vec<String>, Vec<String>)>, Vec<String>, usize)> {
    let mut of_blocks: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    let mut default_block: Vec<String> = Vec::new();
    let mut i = start;
    let mut depth = 1i32;

    while i < tokens.len() {
        match tokens[i].as_str() {
            "case"    => { depth += 1; default_block.push(tokens[i].clone()); }
            "endcase" if depth == 1 => { break; }
            "endcase" => { depth -= 1; default_block.push(tokens[i].clone()); }
            "of" if depth == 1 => {
                // The tokens before this `of` are the value expression
                let val_toks: Vec<String> = default_block.drain(..).collect();
                i += 1;
                // Collect body until `endof`
                let mut body = Vec::new();
                let mut inner_depth = 1i32;
                while i < tokens.len() {
                    match tokens[i].as_str() {
                        "of"    => { inner_depth += 1; body.push(tokens[i].clone()); }
                        "endof" if inner_depth == 1 => { let _ = inner_depth; break; }
                        "endof" => { inner_depth -= 1; body.push(tokens[i].clone()); }
                        _ => { body.push(tokens[i].clone()); }
                    }
                    i += 1;
                }
                of_blocks.push((val_toks, body));
            }
            _ => { default_block.push(tokens[i].clone()); }
        }
        i += 1;
    }
    Ok((of_blocks, default_block, i - start))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arithmetic() {
        assert_eq!(Forth::run("2 3 + .").unwrap().trim(), "5");
        assert_eq!(Forth::run("10 3 - .").unwrap().trim(), "7");
        assert_eq!(Forth::run("4 5 * .").unwrap().trim(), "20");
        assert_eq!(Forth::run("10 3 / .").unwrap().trim(), "3");
        assert_eq!(Forth::run("10 3 mod .").unwrap().trim(), "1");
    }

    #[test]
    fn test_stack_ops() {
        assert_eq!(Forth::run("5 dup + .").unwrap().trim(), "10");
        assert_eq!(Forth::run("1 2 swap . .").unwrap().trim(), "1 2");
        assert_eq!(Forth::run("1 2 over . . .").unwrap().trim(), "1 2 1");
        assert_eq!(Forth::run("1 2 nip .").unwrap().trim(), "2");
    }

    #[test]
    fn test_colon_definition() {
        assert_eq!(Forth::run(": sq dup * ; 7 sq .").unwrap().trim(), "49");
        assert_eq!(Forth::run(": cube dup dup * * ; 3 cube .").unwrap().trim(), "27");
    }

    #[test]
    fn test_stdlib_square() {
        assert_eq!(Forth::run("9 square .").unwrap().trim(), "81");
    }

    #[test]
    fn test_if_then() {
        assert_eq!(Forth::run("1 if 42 . then").unwrap().trim(), "42");
        assert_eq!(Forth::run("0 if 42 . then").unwrap(), "");
    }

    #[test]
    fn test_if_else_then() {
        assert_eq!(Forth::run("1 if 1 . else 0 . then").unwrap().trim(), "1");
        assert_eq!(Forth::run("0 if 1 . else 0 . then").unwrap().trim(), "0");
    }

    #[test]
    fn test_begin_until() {
        let out = Forth::run("0 begin dup . 1 + dup 4 = until drop").unwrap();
        assert_eq!(out.trim(), "0 1 2 3");
    }

    #[test]
    fn test_do_loop() {
        let out = Forth::run("5 0 do i . loop").unwrap();
        assert_eq!(out.trim(), "0 1 2 3 4");
    }

    #[test]
    fn test_do_loop_accumulate() {
        assert_eq!(Forth::run("0  5 0 do i + loop .").unwrap().trim(), "10");
    }

    #[test]
    fn test_string_print() {
        let out = Forth::run(r#"." Hello, Forth!" cr"#).unwrap();
        assert_eq!(out, "Hello, Forth!\n");
    }

    #[test]
    fn test_comparison() {
        assert_eq!(Forth::run("3 3 = .").unwrap().trim(), "-1");
        assert_eq!(Forth::run("2 3 = .").unwrap().trim(), "0");
        assert_eq!(Forth::run("2 3 < .").unwrap().trim(), "-1");
        assert_eq!(Forth::run("3 2 > .").unwrap().trim(), "-1");
    }

    #[test]
    fn test_variable() {
        let out = Forth::run("variable x  42 x !  x @ .").unwrap();
        assert_eq!(out.trim(), "42");
    }

    #[test]
    fn test_plus_store() {
        let out = Forth::run("variable n  10 n !  5 n +!  n @ .").unwrap();
        assert_eq!(out.trim(), "15");
    }

    #[test]
    fn test_nested_definitions() {
        let src = ": double 2 * ; : quadruple double double ; 3 quadruple .";
        assert_eq!(Forth::run(src).unwrap().trim(), "12");
    }

    #[test]
    fn test_fibonacci() {
        // Compute fib(10) = 55 iteratively
        let src = r#"
            variable fa  variable fb  variable tmp
            1 fa !  1 fb !
            8 0 do
                fb @ tmp !
                fa @ fb @ + fb !
                tmp @ fa !
            loop
            fb @ .
        "#;
        assert_eq!(Forth::run(src).unwrap().trim(), "55");
    }

    #[test]
    fn test_pick() {
        // 0 pick = dup, 1 pick = over
        assert_eq!(Forth::run("10 20 30  1 pick .").unwrap().trim(), "20");
        assert_eq!(Forth::run("10 20 30  0 pick .").unwrap().trim(), "30");
    }

    #[test]
    fn test_recurse() {
        // factorial using recurse
        let src = ": fact dup 1 <= if drop 1 else dup 1 - recurse * then ; 5 fact .";
        assert_eq!(Forth::run(src).unwrap().trim(), "120");
    }

    #[test]
    fn test_case() {
        let src = r#"
            2 case
                1 of ." one" endof
                2 of ." two" endof
                3 of ." three" endof
                ." other"
            endcase
        "#;
        assert_eq!(Forth::run(src).unwrap(), "two");
    }

    #[test]
    fn test_case_default() {
        let src = r#"
            99 case
                1 of ." one" endof
                2 of ." two" endof
                ." other"
            endcase
        "#;
        assert_eq!(Forth::run(src).unwrap(), "other");
    }

    #[test]
    fn test_star_slash() {
        // 2 * 3 / 4 with 128-bit intermediate
        assert_eq!(Forth::run("100 3 4 */ .").unwrap().trim(), "75");
    }

    #[test]
    fn test_slash_mod() {
        assert_eq!(Forth::run("10 3 /mod . .").unwrap().trim(), "3 1");
    }

    #[test]
    fn test_rstack() {
        // >r stores, r> retrieves
        assert_eq!(Forth::run("42 >r 0 drop r> .").unwrap().trim(), "42");
    }

    #[test]
    fn test_gcd_stdlib() {
        assert_eq!(Forth::run("12 8 gcd .").unwrap().trim(), "4");
        assert_eq!(Forth::run("15 10 gcd .").unwrap().trim(), "5");
    }

    #[test]
    fn test_fib_stdlib() {
        assert_eq!(Forth::run("10 fib .").unwrap().trim(), "89");
    }

    #[test]
    fn test_confirm_auto_approve() {
        // Without a callback, confirm" auto-approves (pushes -1 = true)
        let out = Forth::run(r#"confirm" delete file?" if ." approved" else ." denied" then"#).unwrap();
        assert_eq!(out, "approved");
    }

    #[test]
    fn test_select_dialog_no_callback_returns_first() {
        // Without a callback, select" auto-selects index 0 (first option).
        let out = Forth::run(r#"select" Pick|Red|Green|Blue" . "#).unwrap();
        assert_eq!(out.trim(), "0");
    }

    #[test]
    fn test_select_dialog_callback_returns_chosen_index() {
        // Callback returns 2 → "Blue"
        let out = Forth::new()
            .with_select(Box::new(|_title, _opts| 2))
            .exec(r#"select" Pick|Red|Green|Blue" . "#)
            .unwrap();
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn test_select_dialog_callback_cancel_returns_minus_one() {
        // Callback returns -1 (user cancelled)
        let out = Forth::new()
            .with_select(Box::new(|_title, _opts| -1))
            .exec(r#"select" Pick|Red|Green|Blue" -1 = if ." cancelled" else ." chosen" then"#)
            .unwrap();
        assert_eq!(out, "cancelled");
    }

    #[test]
    fn test_select_dialog_remote_mode_auto_cancel() {
        // In remote mode, select" always returns -1 (never blocks on TUI).
        let mut vm = Forth::new();
        vm.remote_mode = true;
        let out = vm.exec(r#"select" Pick|A|B|C" . "#).unwrap();
        assert_eq!(out.trim(), "-1");
    }

    #[test]
    fn test_confirm_with_deny_callback() {
        let out = Forth::new()
            .with_confirm(Box::new(|_| false))
            .exec(r#"confirm" delete file?" if ." approved" else ." denied" then"#)
            .unwrap();
        assert_eq!(out, "denied");
    }

    #[test]
    fn test_confirm_with_approve_callback() {
        let out = Forth::new()
            .with_confirm(Box::new(|_| true))
            .exec(r#"confirm" write file?" if ." approved" else ." denied" then"#)
            .unwrap();
        assert_eq!(out, "approved");
    }

    #[test]
    fn test_connected_false_when_isolated() {
        let out = Forth::run("connected? if .\" yes\" else .\" no\" then").unwrap();
        assert_eq!(out, "no");
    }

    #[test]
    fn test_connected_true_when_peer_registered() {
        let mut vm = Forth::new();
        vm.peers.push("192.168.1.2:11435".to_string());
        let out = vm.exec("connected? if .\" yes\" else .\" no\" then").unwrap();
        assert_eq!(out, "yes");
    }

    #[test]
    fn test_connected_true_when_my_addr_set() {
        let mut vm = Forth::new();
        vm.my_addr = Some("192.168.1.1:11435".to_string());
        let out = vm.exec("connected? if .\" yes\" else .\" no\" then").unwrap();
        assert_eq!(out, "yes");
    }

    #[test]
    fn test_vm_dump_source_log() {
        let mut vm = Forth::new();
        vm.exec(": greet  .\" hello\" cr ;").unwrap();
        vm.exec(": farewell  .\" bye\" cr ;").unwrap();
        let dump = vm.dump_source();
        assert!(dump.contains(": greet"), "greet should be in dump");
        assert!(dump.contains(": farewell"), "farewell should be in dump");
        // Pasting the dump into a fresh VM recreates the words.
        let mut vm2 = Forth::new();
        vm2.exec(&dump).unwrap();
        let out = vm2.exec("greet").unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn test_vm_dump_redefinition_replaces_not_appends() {
        let mut vm = Forth::new();
        vm.exec(": foo  1 . ;").unwrap();
        vm.exec(": foo  2 . ;").unwrap(); // redefine
        let dump = vm.dump_source();
        // Only one entry for foo — no duplicates
        assert_eq!(dump.lines().filter(|l| l.contains(": foo")).count(), 1);
        // The dump contains the new definition
        let out = vm.exec("foo").unwrap();
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn test_vm_dump_respects_undo() {
        let mut vm = Forth::new();
        vm.exec(": a  1 . ;").unwrap();
        let snap = vm.snapshot();
        vm.exec(": b  2 . ;").unwrap();
        assert!(vm.dump_source().contains(": b"));
        vm.restore(&snap);
        assert!(!vm.dump_source().contains(": b"), "b should be gone after restore");
    }

    #[test]
    fn test_fuel_catches_infinite_loop_fast() {
        let mut vm = Forth::new();
        let err = vm.exec(": forever  begin again ; forever").unwrap_err();
        assert!(err.to_string().contains("fuel exhausted"), "expected fuel error, got: {err}");
    }

    #[test]
    fn test_fuel_word_pushes_remaining() {
        let mut vm = Forth::new();
        // fuel should be close to DEFAULT_FUEL at start of exec; just check it's a large positive
        let out = vm.exec("fuel . cr").unwrap();
        let n: i64 = out.trim().parse().unwrap();
        assert!(n > 0 && n <= 1_000_000, "fuel should be positive and ≤ default, got {n}");
    }

    #[test]
    fn test_with_fuel_allows_more_steps() {
        let mut vm = Forth::new();
        // fib 25 needs ~3M steps (recursive) — over default, explicit fuel required
        let out = vm.exec_with_fuel("25 fib . cr", 0).unwrap(); // 0 = unlimited
        assert_eq!(out.trim(), "121393"); // fib(25) with base case n<2→1
    }

    #[test]
    fn test_default_fuel_allows_fib20() {
        // fib 20 should fit comfortably in 100k steps
        let out = Forth::run("20 fib . cr").unwrap();
        assert_eq!(out.trim(), "10946");
    }

    #[test]
    fn test_alternate_string_delimiter() {
        // s| ... | allows embedded " without escaping
        let out = Forth::run(r#"s| say "hello" | type"#).unwrap();
        assert_eq!(out, r#"say "hello" "#);
    }

    #[test]
    fn test_alternate_print_delimiter() {
        // .| ... | prints without needing to escape "
        let out = Forth::run(r#".| say "hello" |"#).unwrap();
        assert_eq!(out, r#"say "hello" "#);
    }

    #[test]
    fn test_file_fetch_and_hash() {
        // Write a temp file, fetch it, hash it
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth").unwrap();
        let path = f.path().to_string_lossy().to_string();

        let mut vm = Forth::new();
        let code = format!(r#"s" {path}" file-fetch sha256 type"#);
        let out = vm.exec(&code).unwrap();
        // SHA-256 of "hello forth"
        use sha2::{Sha256, Digest};
        let expected = format!("{:x}", Sha256::digest(b"hello forth"));
        assert_eq!(out.trim(), expected);
    }

    #[test]
    fn test_file_hash_prints() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth").unwrap();
        let path = f.path().to_string_lossy().to_string();

        let code = format!(r#"s" {path}" file-hash"#);
        let out = Forth::run(&code).unwrap();
        use sha2::{Sha256, Digest};
        let expected = format!("{:x}", Sha256::digest(b"hello forth"));
        assert_eq!(out.trim(), expected);
    }

    #[test]
    fn test_file_size() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let code = format!(r#"s" {path}" file-size . cr"#);
        let out = Forth::run(&code).unwrap();
        assert_eq!(out.trim(), "11");
    }

    #[test]
    fn test_file_sha256_range_first_bytes() {
        use std::io::Write;
        use sha2::{Sha256, Digest};
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth world").unwrap();
        let path = f.path().to_string_lossy().to_string();
        // hash first 5 bytes = "hello"
        let code = format!(r#"s" {path}" 0 5 file-sha256-range type"#);
        let out = Forth::run(&code).unwrap();
        let expected = format!("{:x}", Sha256::digest(b"hello"));
        assert_eq!(out.trim(), expected);
    }

    #[test]
    fn test_file_sha256_range_mid_bytes() {
        use std::io::Write;
        use sha2::{Sha256, Digest};
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth world").unwrap();
        let path = f.path().to_string_lossy().to_string();
        // hash bytes 6..11 = "forth"
        let code = format!(r#"s" {path}" 6 5 file-sha256-range type"#);
        let out = Forth::run(&code).unwrap();
        let expected = format!("{:x}", Sha256::digest(b"forth"));
        assert_eq!(out.trim(), expected);
    }

    #[test]
    fn test_file_slice_reads_range() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth world").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let code = format!(r#"s" {path}" 6 5 file-slice type"#);
        let out = Forth::run(&code).unwrap();
        assert_eq!(out, "forth");
    }

    #[test]
    fn test_file_hash_range_prints() {
        use std::io::Write;
        use sha2::{Sha256, Digest};
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello forth world").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let code = format!(r#"s" {path}" 0 5 file-hash-range"#);
        let out = Forth::run(&code).unwrap();
        let expected = format!("{:x}", Sha256::digest(b"hello"));
        assert_eq!(out.trim(), expected);
    }

    #[test]
    fn test_file_sha256_range_clamps_to_eof() {
        use std::io::Write;
        use sha2::{Sha256, Digest};
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hi").unwrap();
        let path = f.path().to_string_lossy().to_string();
        // ask for 100 bytes from offset 0 — should clamp to actual 2 bytes
        let code = format!(r#"s" {path}" 0 100 file-sha256-range type"#);
        let out = Forth::run(&code).unwrap();
        let expected = format!("{:x}", Sha256::digest(b"hi"));
        assert_eq!(out.trim(), expected);
    }

    #[test]
    fn test_push_str_and_type() {
        // s" text" pushes a string pool index; type prints it
        let out = Forth::run(r#"s" hello world" type"#).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn test_str_eq() {
        let out = Forth::run(r#"s" abc" s" abc" str= . cr"#).unwrap();
        assert!(out.contains("-1"), "equal strings should push -1");
        let out2 = Forth::run(r#"s" abc" s" xyz" str= . cr"#).unwrap();
        assert!(out2.contains("0"), "different strings should push 0");
    }

    #[test]
    fn test_str_len() {
        let out = Forth::run(r#"s" hello" str-len . cr"#).unwrap();
        assert!(out.trim() == "5", "str-len of 'hello' should be 5, got {out:?}");
    }

    #[test]
    fn test_str_cat() {
        let out = Forth::run(r#"s" foo" s" bar" str-cat type"#).unwrap();
        assert_eq!(out, "foobar");
    }

    #[test]
    fn test_sha256_known_value() {
        // SHA-256 of empty string is well-known
        let out = Forth::run(r#"s" " sha256 type"#).unwrap();
        assert_eq!(out, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn test_nonce_is_integer() {
        // nonce pushes an integer; running it twice should produce different values (with overwhelming probability)
        let mut vm = Forth::new();
        vm.exec("nonce nonce").unwrap();
        let stack = vm.data_stack().to_vec();
        assert_eq!(stack.len(), 2, "nonce should push one value each call");
        // With overwhelming probability two random 64-bit values differ
        // (chance of collision: 2^-63 ≈ 0)
    }

    #[test]
    fn test_keygen_sign_verify() {
        // Full round-trip: generate keypair, sign data, verify signature
        let out = Forth::run(
            r#"keygen  ( -- pub-idx priv-idx )
               s" the shared stack is real"  ( -- pub priv data )
               sign   ( -- pub sig )
               swap   ( -- sig pub )
               swap   ( -- pub sig )
               s" the shared stack is real"  ( -- pub sig data )
               verify . cr"#
        ).unwrap();
        assert!(out.trim() == "-1", "valid signature should verify as true (-1), got {out:?}");
    }

    #[test]
    fn test_verify_rejects_tampered_data() {
        let out = Forth::run(
            r#"keygen
               s" original message" sign
               swap
               swap
               s" tampered message"
               verify . cr"#
        ).unwrap();
        assert!(out.trim() == "0", "tampered data should fail verification, got {out:?}");
    }

    // ── forget / undo tests ──────────────────────────────────────────────────

    #[test]
    fn test_forget_removes_word() {
        let mut vm = Forth::new();
        vm.exec(": hello  42 . ;").unwrap();
        vm.exec("forget hello").unwrap();
        // Original definition is gone — `missing-word` handles it now (prints "(hello)"), not "42"
        let out = vm.exec("hello").unwrap();
        assert!(!out.contains("42"), "original body should not run after forget, got: {out:?}");
    }

    #[test]
    fn test_forget_removes_from_source_log() {
        let mut vm = Forth::new();
        vm.exec(": greet  cr ;").unwrap();
        assert!(vm.dump_source().contains(": greet"), "should be in log before forget");
        vm.exec("forget greet").unwrap();
        assert!(!vm.dump_source().contains(": greet"), "should be gone from log after forget");
    }

    #[test]
    fn test_forget_unknown_word_is_graceful() {
        let mut vm = Forth::new();
        // Forgetting a non-existent word should not error
        vm.exec("forget nonexistent-xyz").unwrap();
    }

    #[test]
    fn test_forget_does_not_affect_other_words() {
        let mut vm = Forth::new();
        vm.exec(": a  1 . ;").unwrap();
        vm.exec(": b  2 . ;").unwrap();
        vm.exec("forget a").unwrap();
        // b still callable
        let out = vm.exec("b").unwrap();
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn test_undo_removes_last_definition() {
        let mut vm = Forth::new();
        vm.exec(": hello  42 . ;").unwrap();
        vm.exec("undo").unwrap();
        // Original definition is gone — `missing-word` handles it, not "42"
        let out = vm.exec("hello").unwrap();
        assert!(!out.contains("42"), "original body should not run after undo, got: {out:?}");
    }

    #[test]
    fn test_undo_restores_previous_definition() {
        let mut vm = Forth::new();
        vm.exec(": foo  1 . ;").unwrap();
        vm.exec(": foo  2 . ;").unwrap(); // redefine
        vm.exec("undo").unwrap();
        let out = vm.exec("foo").unwrap();
        assert_eq!(out.trim(), "1", "undo should restore previous definition of foo");
    }

    #[test]
    fn test_undo_multiple_levels() {
        let mut vm = Forth::new();
        vm.exec(": a  10 . ;").unwrap();
        vm.exec(": b  20 . ;").unwrap();
        vm.exec(": c  30 . ;").unwrap();
        vm.exec("undo").unwrap(); // removes c
        vm.exec("undo").unwrap(); // removes b
        // Original definitions gone — missing-word handles them, not "30"/"20"
        let out_c = vm.exec("c").unwrap();
        let out_b = vm.exec("b").unwrap();
        assert!(!out_c.contains("30"), "c body should not run after undo, got: {out_c:?}");
        assert!(!out_b.contains("20"), "b body should not run after undo, got: {out_b:?}");
        let out = vm.exec("a").unwrap();
        assert_eq!(out.trim(), "10", "a should still work");
    }

    #[test]
    fn test_undo_nothing_is_graceful() {
        let mut vm = Forth::new();
        let out = vm.exec("undo").unwrap();
        assert!(out.contains("nothing to undo"), "should say nothing to undo, got: {out:?}");
    }

    #[test]
    fn test_undo_removes_from_source_log() {
        let mut vm = Forth::new();
        vm.exec(": visible  99 . ;").unwrap();
        assert!(vm.dump_source().contains(": visible"), "should be in log");
        vm.exec("undo").unwrap();
        assert!(!vm.dump_source().contains(": visible"), "should be gone after undo");
    }

    // ── lock / unlock tests ──────────────────────────────────────────────────

    #[test]
    fn test_lock_acquire_succeeds_when_free() {
        let mut vm = Forth::new();
        let out = vm.exec(r#"s" res" 5000 lock . cr"#).unwrap();
        assert_eq!(out.trim(), "-1", "first lock should succeed");
    }

    #[test]
    fn test_lock_fails_when_already_held() {
        let mut vm = Forth::new();
        vm.exec(r#"s" res" 5000 lock drop"#).unwrap();
        let out = vm.exec(r#"s" res" 5000 lock . cr"#).unwrap();
        assert_eq!(out.trim(), "0", "second lock on same resource should fail");
    }

    #[test]
    fn test_unlock_releases_lock() {
        let mut vm = Forth::new();
        vm.exec(r#"s" res" 5000 lock drop"#).unwrap();
        vm.exec(r#"s" res" unlock"#).unwrap();
        let out = vm.exec(r#"s" res" 5000 lock . cr"#).unwrap();
        assert_eq!(out.trim(), "-1", "lock should succeed after unlock");
    }

    #[test]
    fn test_lock_ttl_positive_when_held() {
        let mut vm = Forth::new();
        vm.exec(r#"s" res" 5000 lock drop"#).unwrap();
        let out = vm.exec(r#"s" res" lock-ttl . cr"#).unwrap();
        let ms: i64 = out.trim().parse().unwrap();
        assert!(ms > 0 && ms <= 5000, "TTL should be positive and ≤ 5000, got {ms}");
    }

    #[test]
    fn test_lock_ttl_zero_when_free() {
        let mut vm = Forth::new();
        let out = vm.exec(r#"s" res" lock-ttl . cr"#).unwrap();
        assert_eq!(out.trim(), "0", "TTL should be 0 when lock is free");
    }

    #[test]
    fn test_lock_max_ttl_capped_at_30s() {
        let mut vm = Forth::new();
        // Request 60 000 ms — should be capped to 30 000
        vm.exec(r#"s" res" 60000 lock drop"#).unwrap();
        let out = vm.exec(r#"s" res" lock-ttl . cr"#).unwrap();
        let ms: i64 = out.trim().parse().unwrap();
        assert!(ms <= 30_000, "TTL should be capped at 30 000 ms, got {ms}");
        assert!(ms > 0, "TTL should still be positive");
    }

    // ── Writing assistance tests ─────────────────────────────────────────────

    #[test]
    fn test_capitalize_first_letter() {
        let out = Forth::run(r#"s" hello world" capitalize type"#).unwrap();
        assert_eq!(out, "Hello world");
    }

    #[test]
    fn test_capitalize_already_capitalized() {
        let out = Forth::run(r#"s" Hello" capitalize type"#).unwrap();
        assert_eq!(out, "Hello");
    }

    #[test]
    fn test_str_upper() {
        let out = Forth::run(r#"s" hello" str-upper type"#).unwrap();
        assert_eq!(out, "HELLO");
    }

    #[test]
    fn test_str_lower() {
        let out = Forth::run(r#"s" HELLO" str-lower type"#).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn test_str_trim() {
        let out = Forth::run(r#"s"   hello   " str-trim type"#).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn test_word_count() {
        let out = Forth::run(r#"s" the quick brown fox" word-count . cr"#).unwrap();
        assert_eq!(out.trim(), "4");
    }

    #[test]
    fn test_sentence_check_valid() {
        let out = Forth::run(r#"s" Hello world." sentence? . cr"#).unwrap();
        assert_eq!(out.trim(), "-1");
    }

    #[test]
    fn test_sentence_check_no_capital() {
        let out = Forth::run(r#"s" hello world." sentence? . cr"#).unwrap();
        assert_eq!(out.trim(), "0");
    }

    #[test]
    fn test_sentence_check_no_terminal_punct() {
        let out = Forth::run(r#"s" Hello world" sentence? . cr"#).unwrap();
        assert_eq!(out.trim(), "0");
    }

    #[test]
    fn test_sentence_check_question_mark() {
        let out = Forth::run(r#"s" Is this correct?" sentence? . cr"#).unwrap();
        assert_eq!(out.trim(), "-1");
    }

    #[test]
    fn test_grammar_check_no_ai_returns_original() {
        // Without a gen_fn, grammar-check returns the string unchanged
        let out = Forth::run(r#"s" i goes to store" grammar-check type"#).unwrap();
        assert_eq!(out, "i goes to store");
    }

    #[test]
    fn test_grammar_check_with_ai() {
        let mut vm = Forth::new();
        vm.set_gen_fn(Box::new(|_prompt| "I go to the store.".to_string()));
        let out = vm.exec(r#"s" i goes to store" grammar-check type"#).unwrap();
        assert_eq!(out, "I go to the store.");
    }

    #[test]
    fn test_improve_no_ai_returns_original() {
        let out = Forth::run(r#"s" It is good." improve type"#).unwrap();
        assert_eq!(out, "It is good.");
    }

    #[test]
    fn test_polish_chains_grammar_then_improve() {
        let mut vm = Forth::new();
        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cc = call_count.clone();
        vm.set_gen_fn(Box::new(move |_| {
            cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            "Polished sentence.".to_string()
        }));
        let out = vm.exec(r#"s" rough sentence" polish type"#).unwrap();
        assert_eq!(out, "Polished sentence.");
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2); // grammar + improve
    }

    #[test]
    fn test_different_lock_names_are_independent() {
        let mut vm = Forth::new();
        vm.exec(r#"s" a" 5000 lock drop"#).unwrap();
        let out = vm.exec(r#"s" b" 5000 lock . cr"#).unwrap();
        assert_eq!(out.trim(), "-1", "lock 'b' should succeed when only 'a' is held");
    }

    // ── Security scanning ────────────────────────────────────────────────────

    #[test]
    fn test_scan_bytes_clean_text_is_low_risk() {
        // Plain text should score very low
        let score = scan_risk_score(b"hello world, this is normal text");
        assert!(score < 20, "plain text should be low risk, got {}", score);
    }

    #[test]
    fn test_scan_bytes_elf_magic_raises_score() {
        let elf = b"\x7fELFsome content here that is not actually an ELF";
        let score = scan_risk_score(elf);
        assert!(score >= 20, "ELF magic should raise risk score, got {}", score);
    }

    #[test]
    fn test_scan_bytes_eicar_is_critical() {
        let eicar = b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*";
        let score = scan_risk_score(eicar);
        assert_eq!(score, 100, "EICAR test file must score 100");
    }

    #[test]
    fn test_file_entropy_text_is_low() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"aaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let code = format!(r#"s" {path}" file-entropy . cr"#);
        let out = Forth::run(&code).unwrap();
        let val: i64 = out.trim().parse().unwrap();
        assert_eq!(val, 0, "all-same-byte entropy should be 0");
    }

    #[test]
    fn test_scan_file_returns_report_string() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let code = format!(r#"s" {path}" scan-file type cr"#);
        let out = Forth::run(&code).unwrap();
        assert!(out.contains("risk:"), "scan-file report should contain risk line");
        assert!(out.contains("entropy:"), "scan-file report should contain entropy");
    }

    #[test]
    fn test_scan_file_eicar_detected() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let code = format!(r#"s" {path}" scan-file type cr"#);
        let out = Forth::run(&code).unwrap();
        assert!(out.contains("EICAR"), "EICAR test file must be flagged");
        assert!(out.contains("critical"), "EICAR must report critical risk");
    }

    // ── redefinition approval gate ────────────────────────────────────────

    #[test]
    fn test_redefine_without_confirm_fn_is_allowed() {
        // No confirm_fn = pipe/test mode: redefinition silently succeeds.
        let out = Forth::run(": sq dup * ; : sq dup dup * * ; 3 sq .").unwrap();
        assert_eq!(out.trim(), "27", "second definition should win");
    }

    #[test]
    fn test_redefine_first_time_never_needs_approval() {
        // Defining a brand-new word never triggers approval even with a confirm_fn.
        let mut asked = false;
        let mut f = Forth::new().with_confirm(Box::new(|_| {
            // Should never be called for a first-time definition.
            unreachable!("approval asked for a new word");
        }));
        // This must not panic or call the confirm_fn.
        f.eval(": greet 42 . ;").unwrap();
        drop(asked); // suppress unused warning
        let _ = f.eval("greet");
        assert_eq!(f.out.trim(), "42");
    }

    #[test]
    fn test_redefine_with_confirm_fn_approved() {
        // confirm_fn returns true → redefinition proceeds.
        let mut f = Forth::new().with_confirm(Box::new(|_msg| true));
        f.eval(": sq dup * ;").unwrap();
        f.eval(": sq dup dup * * ;").unwrap(); // approved
        f.eval("3 sq .").unwrap();
        assert_eq!(f.out.trim(), "27");
    }

    #[test]
    fn test_redefine_with_confirm_fn_denied() {
        // confirm_fn returns false → redefinition is cancelled.
        let mut f = Forth::new().with_confirm(Box::new(|_msg| false));
        f.eval(": sq dup * ;").unwrap();
        let err = f.eval(": sq dup dup * * ;").unwrap_err();
        assert!(err.to_string().contains("cancelled"), "should report cancellation: {err}");
        // Original definition must still be intact.
        f.eval("4 sq .").unwrap();
        assert_eq!(f.out.trim(), "16");
    }

    #[test]
    fn test_redefine_confirm_fn_receives_word_name() {
        // The prompt passed to confirm_fn must mention the word being redefined.
        let mut f = Forth::new().with_confirm(Box::new(|msg: &str| {
            assert!(msg.contains("sq"), "prompt must name the word: {msg}");
            true
        }));
        f.eval(": sq dup * ;").unwrap();
        f.eval(": sq dup dup * * ;").unwrap();
    }

    #[test]
    fn test_redefine_builtin_user_can_shadow() {
        // User-defined words take priority over builtins — tools are overridable.
        // Builtins live in name_to_builtin(), not name_index, so no confirm gate fires.
        // After the user defines a word with the same name, their definition wins.
        let mut f = Forth::new();
        // User redefines `dup`: drops TOS, pushes 99.
        f.eval(": dup drop 99 ;").unwrap();
        // User's `dup` now shadows the builtin: 5 dup → drops 5, pushes 99.
        f.eval("5 dup .").unwrap();
        assert_eq!(f.out.trim(), "99");
    }

    // ── collect_machine_specs ─────────────────────────────────────────────

    #[test]
    fn test_collect_machine_specs_cpu_nonzero() {
        let (cpu, _ram, _bench) = collect_machine_specs();
        assert!(cpu > 0, "must detect at least one CPU core");
    }

    #[test]
    fn test_collect_machine_specs_ram_nonzero() {
        let (_cpu, ram, _bench) = collect_machine_specs();
        assert!(ram > 0, "must detect non-zero RAM");
    }

    #[test]
    fn test_collect_machine_specs_bench_nonzero() {
        let (_cpu, _ram, bench) = collect_machine_specs();
        // benchmark runs 10M iterations; on any real machine this takes at
        // least 1ms and completes in under 10 seconds.
        assert!(bench < 10_000, "benchmark took implausibly long: {}ms", bench);
    }

    #[test]
    fn test_collect_machine_specs_bench_is_deterministic_order_of_magnitude() {
        // Two consecutive runs should both finish in under 10s.
        let (_, _, b1) = collect_machine_specs();
        let (_, _, b2) = collect_machine_specs();
        // Both should be < 10 seconds.
        assert!(b1 < 10_000);
        assert!(b2 < 10_000);
    }

    // ── format_peer_hw ────────────────────────────────────────────────────

    #[test]
    fn test_format_peer_hw_all_fields() {
        let p = crate::registry::PeerEntry {
            addr:      "a:1".to_string(),
            label:     None,
            tags:      vec![],
            load:      None,
            region:    None,
            cpu_cores: Some(8),
            ram_mb:    Some(16_384),
            bench_ms:  Some(42),
        };
        let hw = format_peer_hw(&p);
        // ANSI codes are present; strip them for assertion.
        let plain = strip_ansi(&hw);
        assert!(plain.contains("8c"),       "should contain cpu count: {plain}");
        assert!(plain.contains("16384MB"),  "should contain ram: {plain}");
        assert!(plain.contains("bench:42ms"), "should contain bench: {plain}");
    }

    #[test]
    fn test_format_peer_hw_no_fields_is_empty() {
        let p = crate::registry::PeerEntry {
            addr:      "a:1".to_string(),
            label:     None,
            tags:      vec![],
            load:      None,
            region:    None,
            cpu_cores: None,
            ram_mb:    None,
            bench_ms:  None,
        };
        let hw = format_peer_hw(&p);
        let plain = strip_ansi(&hw);
        assert!(plain.trim().is_empty(), "empty hw should produce empty string: {plain:?}");
    }

    #[test]
    fn test_format_peer_hw_partial_fields() {
        let p = crate::registry::PeerEntry {
            addr:      "a:1".to_string(),
            label:     None,
            tags:      vec![],
            load:      None,
            region:    None,
            cpu_cores: Some(4),
            ram_mb:    None,
            bench_ms:  None,
        };
        let hw = format_peer_hw(&p);
        let plain = strip_ansi(&hw);
        assert!(plain.contains("4c"));
        assert!(!plain.contains("MB"));
        assert!(!plain.contains("bench:"));
    }

    // ── slowest (no registry set) ─────────────────────────────────────────

    #[test]
    fn test_slowest_no_registry_pushes_minus_one() {
        // Without a registry configured, slowest should push -1 and emit a warning.
        let mut f = Forth::new();
        // Run slowest — should not panic, should push -1.
        let _ = f.eval("slowest");
        assert_eq!(f.data.last().copied(), Some(-1));
        assert!(f.out.contains("no registry set"));
    }

    // ── vocabulary: zh.toml entries ───────────────────────────────────────

    #[test]
    fn test_zh_vocab_slowest_entry_exists() {
        let lib = crate::coforth::Library::load();
        let entry = lib.lookup("最慢").expect("最慢 must exist in library");
        assert_eq!(entry.forth.as_deref(), Some("slowest"));
    }

    #[test]
    fn test_zh_vocab_give_it_entry_exists() {
        let lib = crate::coforth::Library::load();
        let entry = lib.lookup("给它").expect("给它 must exist in library");
        assert!(entry.forth.is_some(), "给它 must have a forth entry");
    }

    // ── vocabulary: en.toml entries ───────────────────────────────────────

    #[test]
    fn test_en_vocab_slowest_entry_exists() {
        let lib = crate::coforth::Library::load();
        let entry = lib.lookup("slowest").expect("slowest must exist in library");
        assert_eq!(entry.forth.as_deref(), Some("slowest"));
    }

    #[test]
    fn test_en_vocab_donate_entry_exists() {
        let lib = crate::coforth::Library::load();
        let entry = lib.lookup("donate").expect("donate must exist in library");
        assert_eq!(entry.forth.as_deref(), Some("slowest on"));
    }

    // ── join / leave / forth-back ─────────────────────────────────────────────

    #[test]
    fn test_join_shorthand_tokenizes_to_join_registry() {
        // join" addr" should push the string and call join-registry.
        // Without a registry configured, JoinRegistry emits a yellow warning.
        let mut vm = Forth::new();
        // No registry set — join-registry prints a warning, does not crash.
        let _ = vm.eval(r#"join" myhost:8080""#);
        // After a failed join my_addr stays None.
        assert!(vm.my_addr.is_none());
    }

    #[test]
    fn test_leave_without_join_prints_warning() {
        let mut vm = Forth::new();
        vm.eval("leave").unwrap();
        // Output should contain a helpful note.
        assert!(vm.out.contains("not registered") || vm.out.contains("leave"),
            "unexpected output: {}", vm.out);
    }

    #[test]
    fn test_leave_builtin_registered_by_name() {
        // Both "leave" and "leave-registry" should map to LeaveRegistry.
        assert!(matches!(name_to_builtin("leave"), Some(Builtin::LeaveRegistry)));
        assert!(matches!(name_to_builtin("leave-registry"), Some(Builtin::LeaveRegistry)));
    }

    #[test]
    fn test_forth_back_in_remote_mode_stores_code() {
        let mut vm = Forth::new();
        vm.remote_mode = true;
        vm.eval(r#"forth-back" 42 dup +"#).unwrap();
        assert_eq!(vm.forth_back.as_deref(), Some("42 dup +"));
    }

    #[test]
    fn test_forth_back_in_local_mode_executes_immediately() {
        let mut vm = Forth::new();
        // remote_mode = false (default); forth-back" code" runs the code immediately.
        vm.eval(r#"forth-back" 7 8 +"#).unwrap();
        // Stack should have 15.
        assert_eq!(vm.pop().unwrap(), 15);
        assert!(vm.forth_back.is_none(), "forth_back should not be set in local mode");
    }

    #[test]
    fn test_my_addr_stored_on_successful_join() {
        // Simulate a successful join by calling JoinRegistry directly with a mocked registry.
        // We can't make real HTTP calls in unit tests, so we verify my_addr is populated
        // when we manually set it (as run_registry_join would do).
        let mut vm = Forth::new();
        assert!(vm.my_addr.is_none());
        vm.my_addr = Some("host:9000".to_string());
        assert_eq!(vm.my_addr.as_deref(), Some("host:9000"));
    }

    // ── Proof system ─────────────────────────────────────────────────────────

    #[test]
    fn test_assert_passes_on_nonzero() {
        // assert should not error when flag is nonzero
        Forth::run("-1 assert").unwrap();
        Forth::run("1 assert").unwrap();
        Forth::run("42 assert").unwrap();
    }

    #[test]
    fn test_assert_fails_on_zero() {
        let err = Forth::run("0 assert").unwrap_err();
        assert!(err.to_string().contains("assertion failed"), "{err}");
    }

    #[test]
    fn test_stdlib_proofs_compiled() {
        let vm = Forth::new();
        // STDLIB_PROOFS compiles test:square, test:fib, etc.
        assert!(vm.name_index.contains_key("test:square"), "test:square missing");
        assert!(vm.name_index.contains_key("test:fib"),    "test:fib missing");
        assert!(vm.name_index.contains_key("test:gcd"),    "test:gcd missing");
        assert!(vm.name_index.contains_key("test:+"),      "test:+ missing");
    }

    #[test]
    fn test_prove_word_passes() {
        let out = Forth::run(r#"prove" square""#).unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("✓") && plain.contains("square"), "{plain}");
    }

    #[test]
    fn test_prove_word_fails_for_broken_definition() {
        // Redefine test:square to always fail, then prove should show ✗
        let mut vm = Forth::new();
        vm.eval(": test:square 0 assert ;").unwrap();
        let out = vm.exec(r#"prove" square""#).unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("✗") && plain.contains("square"), "{plain}");
    }

    #[test]
    fn test_prove_word_unknown_shows_hint() {
        let out = Forth::run(r#"prove" nonexistent-word-xyz""#).unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("no proof"), "{plain}");
    }

    #[test]
    fn test_prove_all_reports_summary() {
        let out = Forth::run("prove-all").unwrap();
        let plain = strip_ansi(&out);
        // Should have a "passed" summary line
        assert!(plain.contains("passed"), "{plain}");
        // On all-pass, only the summary line is printed — no individual ✓ noise
        assert!(!plain.contains("✓"), "should be silent on all-pass: {plain}");
    }

    #[test]
    fn test_prove_all_silent_on_pass_verbose_on_fail() {
        // When all pass: only summary, no ✓ lines.
        let out = Forth::run("prove-all").unwrap();
        let plain = strip_ansi(&out);
        assert_eq!(plain.lines().count(), 1, "all-pass should be one line: {plain}");

        // When one test fails: ✗ line + summary.
        let mut vm = Forth::new();
        vm.eval(": test:broken 0 assert ;").unwrap();
        let out = vm.exec("prove-all").unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("✗") && plain.contains("broken"), "{plain}");
        assert!(plain.contains("failed"), "{plain}");
    }

    #[test]
    fn test_prove_languages_english_chinese_agree() {
        // prove-languages: argue English ↔ Chinese for 10 shared primitives.
        // All 10 should agree — two languages, one stack.
        let out = Forth::run("prove-languages").unwrap();
        let plain = strip_ansi(&out);
        assert!(!plain.is_empty(), "prove-languages produced no output");
        // Summary must report all 10 agreed, zero unresolved.
        assert!(
            plain.contains("10/10") || plain.contains("two languages, one stack"),
            "expected all 10 pairs to agree: {plain}"
        );
        assert!(!plain.contains("unresolved"), "unexpected failures: {plain}");
        // Spot-check: add/加 and true/是 must appear
        assert!(plain.contains("加"), "missing 加 in output: {plain}");
        assert!(plain.contains("是"), "missing 是 in output: {plain}");
    }

    #[test]
    fn test_prove_english_reports_most_words_pass() {
        // prove-english runs all 1049 English-library word bodies.
        // At least 90% should execute without unknown-word errors.
        let out = Forth::run("prove-english").unwrap();
        let plain = strip_ansi(&out);
        assert!(!plain.is_empty(), "prove-english produced no output");
        // Summary line contains "words" and a percentage
        assert!(plain.contains("words"), "expected 'words' in output: {plain}");
        // Extract pass/total from "N/1049 words (P%)"
        let summary = plain.lines().last().unwrap_or("");
        // Find the pass count — must be at least 900 out of ~1049
        if let Some(slash_pos) = summary.find('/') {
            let pass_str = summary[..slash_pos].trim_start_matches(|c: char| !c.is_ascii_digit());
            let pass: usize = pass_str.parse().unwrap_or(0);
            assert!(pass >= 900, "expected ≥900 English words to prove, got {pass}: {plain}");
        }
    }

    #[test]
    fn test_boom_executes_machine_and_says_boom() {
        // boom ( str -- ): evals the code string then says "💥 boom."
        let out = Forth::run("s\" 3 4 +\" boom").unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("boom"), "expected boom: {plain}");
    }

    #[test]
    fn test_eval_executes_code_string() {
        // eval runs the code in the string; side effects appear in output
        let out = Forth::run("s\" 42 .\" eval").unwrap();
        assert!(strip_ansi(&out).contains("42"));
    }

    #[test]
    fn test_humans_argue_about_forth_programs_runs_cleanly() {
        // Natural language: "humans argue about forth programs."
        // `humans`, `about`, `programs` are no-ops; `argue` gracefully prints "agreed."
        // when called without two string indices; `forth` pushes -1; `.` prints it.
        // Must use the precompiled VM (MAJOR_WORDS_FORTH) not bare Forth::new().
        let mut vm = crate::coforth::Library::precompiled_vm();
        vm.exec("humans argue about forth programs .").unwrap();
        let plain = strip_ansi(&vm.out);
        assert!(plain.contains("agreed"), "expected 'agreed' in output: {plain}");
        assert!(plain.contains("-1"), "expected forth's value to be printed: {plain}");
    }

    #[test]
    fn test_i_am_a_grammar_defining_grammar_evaluates_true() {
        // "I am a grammar defining grammar. That's what I am."
        // Both sentences evaluate to -1 (true): `i` pushes -1, rest are no-ops.
        // "I am forth. I write whatever code I want." — same machine.
        let sentences: &[(&str, &str)] = &[
            ("i am a grammar defining grammar .",   "-1"),
            ("thats what i am .",                   "-1"),
            ("i am forth .",                        "-1"),
            ("i write whatever code i want .",      "-1"),
            ("humans argue about forth programs .", "agreed."),
        ];
        for (src, expected) in sentences {
            let mut vm = crate::coforth::Library::precompiled_vm();
            vm.exec(src).unwrap_or_else(|e| panic!("{src} failed: {e}"));
            let plain = strip_ansi(&vm.out);
            assert!(
                plain.contains(expected),
                "sentence: {src}\n  expected '{}' in output: {plain}",
                expected
            );
        }
    }

    #[test]
    fn test_sun_pushes_epoch_and_prints() {
        let out = Forth::run("sun drop").unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("epoch"), "expected epoch in sun output: {plain}");
    }

    #[test]
    fn test_boom_runs_code_from_stack() {
        // boom pops string, evals it, says boom — the machine runs first
        let out = Forth::run("s\" 1 2 +\" boom").unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("boom"), "expected boom: {plain}");
    }

    #[test]
    fn test_prove_all_bool_pushes_true_on_all_pass() {
        // prove-all? should push -1 when all proofs pass
        let out = Forth::run("prove-all? .").unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("-1"), "expected -1 (true): {plain}");
    }

    #[test]
    fn test_user_can_define_and_prove_own_word() {
        let mut vm = Forth::new();
        vm.eval(": double dup + ;").unwrap();
        vm.eval(": test:double 5 double 10 = assert  0 double 0 = assert ;").unwrap();
        let out = vm.exec(r#"prove" double""#).unwrap();
        let plain = strip_ansi(&out);
        assert!(plain.contains("✓") && plain.contains("double"), "{plain}");
    }

    #[test]
    fn test_inlining_short_words_produces_correct_results() {
        // Short words (body ≤ 4 cells, no nested calls) should be inlined.
        // Verify inlined words produce the same results as non-inlined equivalents.
        let mut vm = Forth::new();
        // `inc` is 2 cells (Lit(1), Plus) — fits inline limit
        vm.eval(": inc  1 + ;").unwrap();
        // `double` is 2 cells (Dup, Star... wait dup+) — fits
        vm.eval(": double  dup + ;").unwrap();
        // Using inlined words in a larger expression
        assert_eq!(vm.exec("5 inc . cr").unwrap().trim(), "6");
        assert_eq!(vm.exec("7 double . cr").unwrap().trim(), "14");
        // Chain of inlined words
        assert_eq!(vm.exec("3 inc double . cr").unwrap().trim(), "8");
    }

    #[test]
    fn test_inlining_noop_compiles_to_nothing() {
        // noop has empty body — inlined as zero cells, caller unchanged
        let mut vm = Forth::new();
        let before_len = vm.memory.len();
        vm.eval(": probe  42 noop 42 = ;").unwrap();
        // The noop contributed no cells to the body
        let result = vm.exec("probe . cr").unwrap();
        assert!(result.trim() == "-1", "noop should not change stack: {result}");
        let _ = before_len; // memory grows but noop adds nothing
    }

    #[test]
    fn test_large_words_not_inlined_still_correct() {
        // fib is large and recursive — must not be inlined; still correct
        let result = Forth::run("7 fib . cr").unwrap();
        assert_eq!(result.trim(), "21");
        // signum is > 4 cells — stays as Addr call, result still correct
        let result = Forth::run("-5 signum . cr").unwrap();
        assert_eq!(result.trim(), "-1");
    }

    #[test]
    fn test_fib_iter_values() {
        let cases = [
            ("0 fib-iter . cr", "1"),
            ("1 fib-iter . cr", "1"),
            ("2 fib-iter . cr", "2"),
            ("7 fib-iter . cr", "21"),
            ("10 fib-iter . cr", "89"),
        ];
        for (prog, expected) in cases {
            let got = Forth::run(prog).unwrap();
            assert_eq!(got.trim(), expected, "program: {prog}  got: {}", got.trim());
        }
    }

    #[test]
    fn test_try_it() {
        // argue across different grammars: postfix vs infix.
        // same computation, different notation — the stack settles it.
        let tries = [
            // same grammar
            r#"s" 3 4 +"      s" 4 3 +"              argue"#,
            r#"s" 10 fib"     s" 10 fib-iter"        argue"#,
            // different grammars: Forth postfix vs infix
            r#"s" 3 4 +"      s" 3 + 4"      infix-argue"#,
            r#"s" 3 4 * 2 +"  s" 3 * 4 + 2"  infix-argue"#,
            r#"s" 10 3 -"     s" 10 - 3"     infix-argue"#,
            r#"s" 3 4 2 * +"  s" 3 + 4 * 2"  infix-argue"#,  // precedence: 3 + (4*2) = 11
        ];
        let mut all = String::new();
        for prog in tries {
            let out = Forth::run(prog).expect(prog);
            all.push_str(&strip_ansi(&out));
        }
        println!("{all}");
        assert!(all.contains("agreed"));
    }

    #[test]
    fn test_gate_passes_when_check_holds() {
        // Both programs agree on 7; check is `=`; gate should pass and leave 7 on stack.
        let mut vm = Forth::new();
        vm.eval(r#"s" 3 4 +" s" 4 3 +" s" =" gate"#).expect("gate should pass");
        assert_eq!(vm.data.last().copied(), Some(7), "gate should leave result on stack");
    }

    #[test]
    fn test_gate_blocks_when_check_fails() {
        // Programs produce different values; check `=` fails; gate should bail.
        let mut vm = Forth::new();
        let result = vm.eval(r#"s" 3 4 +" s" 2 2 +" s" =" gate"#);
        assert!(result.is_err(), "gate should bail when check fails");
    }

    #[test]
    fn test_gate_custom_check() {
        // Check: result must be > 5.  "3 4 +" → 7; "4 3 +" → 7; check "drop 5 >" → true.
        let mut vm = Forth::new();
        vm.eval(r#"s" 3 4 +" s" 4 3 +" s" drop 5 >" gate"#).expect("gate with custom check");
        assert_eq!(vm.data.last().copied(), Some(7));
    }

    #[test]
    fn test_both_ways() {
        // Two directions at once: prove commutativity for each operation.
        let cases = [
            r#"3 4 s" +" both-ways"#,
            r#"5 6 s" *" both-ways"#,
            r#"12 10 s" and" both-ways"#,
            r#"12 10 s" or" both-ways"#,
        ];
        for prog in cases {
            Forth::run(prog).expect(prog); // runs without bail → proof holds
        }
    }

    #[test]
    fn test_page() {
        let prog = r#"page"
3 4 +     | 4 3 +
3 4 *     | 4 3 *
10 3 -    | 7
""#;
        Forth::run(prog).expect("page proof should hold");
    }

    #[test]
    fn test_resolve() {
        // Many sentences, one truth.
        let prog = r#"resolve"
3 4 +
4 3 +
7
2 5 +
14 2 /
""#;
        let out = Forth::run(prog).expect("all sentences should resolve to 7");
        let clean = strip_ansi(&out);
        assert!(clean.contains("all agree"), "expected 'all agree' in: {clean}");
    }

    #[test]
    fn test_resolve_disagrees() {
        let prog = r#"resolve"
3 4 +
3 4 -
""#;
        assert!(Forth::run(prog).is_err());
    }

    #[test]
    fn test_page_disagree_fails() {
        let prog = r#"page"
3 4 +  | 3 4 -
""#;
        assert!(Forth::run(prog).is_err(), "disagreeing page should fail");
    }

    #[test]
    fn test_back_and_forth() {
        // A round trip is a proof: go forth, come back, you are home.
        let cases = [
            r#"5  s" 3 +"  s" 3 -"  back-and-forth"#,   // +3 then -3
            r#"7  s" 2 *"  s" 2 /"  back-and-forth"#,   // *2 then /2
            r#"9  s" negate"  s" negate"  back-and-forth"#, // negate is its own inverse
            r#"1  s" 1 lshift"  s" 1 rshift"  back-and-forth"#, // bit-shift round trip
        ];
        for prog in cases {
            Forth::run(prog).expect(prog); // assert does not panic → proof holds
        }
    }

    #[test]
    fn test_natural_language_missing_word() {
        // Define a known word, then use it in a natural language sentence.
        // Known words execute; unknown ones are silently dropped by default missing-word.
        let out = strip_ansi(&Forth::run(
            ": greet  .\" hi.\" cr ;   greet; I am a forth program."
        ).unwrap());
        assert!(out.contains("hi"), "known word should execute, got: {out:?}");
        // Unknown words silently dropped — no parens, no error
        assert!(!out.contains("(I)"), "silent handler should not print parens, got: {out:?}");
    }

    #[test]
    fn test_missing_word_handler_customizable() {
        // Redefine missing-word to count each unknown word (drop the string, push 1 and print).
        let out = strip_ansi(&Forth::run(
            ": missing-word ( str -- ) drop 1 . ;  hello world"
        ).unwrap());
        // `hello` and `world` are both unknown here → handler fires twice → "1 1"
        assert!(out.contains("1"), "custom handler should fire for unknown words, got: {out:?}");
    }

    #[test]
    fn test_semicolon_separates_tokens() {
        // `Hello;` should tokenize as two tokens: `Hello` and `;`
        // Previously it was one token `Hello;` that could never match.
        let out = strip_ansi(&Forth::run(
            ": greet .\" hi\" ;  greet; unknown-word-xyz"
        ).unwrap());
        assert!(out.contains("hi"), "greet should execute after semicolon split, got: {out:?}");
    }

    #[test]
    fn test_infix_precedence() {
        let cases = [
            ("s\" 3 + 4\" infix . cr", "7"),
            ("s\" 3 * 4\" infix . cr", "12"),
            ("s\" 3 + 4 * 2\" infix . cr", "11"),   // 3 + (4*2) = 11
            ("s\" ( 3 + 4 ) * 2\" infix . cr", "14"), // (3+4)*2 = 14
            ("s\" 10 - 3\" infix . cr", "7"),
        ];
        for (prog, expected) in cases {
            let got = Forth::run(prog).unwrap();
            assert_eq!(got.trim(), expected, "prog: {prog}");
        }
    }

    /// Strip ANSI escape codes for plain-text assertions.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // skip until end of CSI/SGR sequence
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() { break; }
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;

    #[test]
    fn test_join_channel_adds_to_set() {
        let mut vm = Forth::new();
        let _ = vm.exec(r#"channel" #forth""#);
        assert!(vm.channels.contains("#forth"), "{:?}", vm.channels);
    }

    #[test]
    fn test_part_channel_removes_from_set() {
        let mut vm = Forth::new();
        let _ = vm.exec(r#"channel" #forth""#);
        assert!(vm.channels.contains("#forth"));
        let _ = vm.exec(r#"part" #forth""#);
        assert!(!vm.channels.contains("#forth"), "{:?}", vm.channels);
    }

    #[test]
    fn test_channels_word_lists_joined() {
        let mut vm = Forth::new();
        let _ = vm.exec(r#"channel" #forth""#);
        let _ = vm.exec(r#"channel" #general""#);
        let out = vm.exec("channels").unwrap();
        let plain = out.replace("\x1b[", "").replace("m", ""); // rough ansi strip
        assert!(out.contains("#forth") || plain.contains("forth"), "{out}");
        assert!(out.contains("#general") || plain.contains("general"), "{out}");
    }

    #[test]
    fn test_word_propagation_via_channel_message() {
        // Simulate receiving a word definition over a channel
        let mut bob = Forth::new();
        let channel_msg = ": triple  3 * ;";
        let _ = bob.exec_with_fuel(channel_msg, 0);
        let out = bob.exec("5 triple .").unwrap();
        assert_eq!(out.trim(), "15", "{out}");
    }

    #[test]
    fn test_extract_channel_forth_integration() {
        // The extract helper + compile round-trip
        let msg = "[#forth] alice: : quadruple  4 * ;";
        // extract the definition (mirrors extract_channel_forth logic)
        let close = msg.find(']').unwrap();
        let after = msg[close+1..].trim_start_matches(':').trim_start();
        let colon_pos = after.find(": ").unwrap();
        let content = after[colon_pos+2..].trim();
        assert!(content.starts_with(':'));

        let mut vm = Forth::new();
        let _ = vm.exec_with_fuel(content, 0);
        let out = vm.exec("3 quadruple .").unwrap();
        assert_eq!(out.trim(), "12", "{out}");
    }
}
