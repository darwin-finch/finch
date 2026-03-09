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

#[derive(Clone, Debug)]
enum Cell {
    Lit(i64),           // push literal onto data stack
    Str(usize),         // print strings[idx]  (string-literal pool)
    PushStr(usize),     // push strings[idx] index as i64 onto data stack (s" literal")
    Confirm(usize),     // ask user strings[idx]; push -1 (yes) or 0 (no)
    ReadFile(usize),    // read file at strings[idx]; emit contents to out
    ExecCmd(usize),     // run shell command strings[idx]; emit stdout to out
    GlobFiles(usize),   // list files matching glob strings[idx]; emit to out
    AddPeer(usize),     // register peer address strings[idx] for scatter
    ScatterExec(usize),     // run strings[idx] Forth code on all peers in parallel
    ScatterBashExec(usize), // run strings[idx] as bash -c on all peers via /v1/exec
    ScatterStack,           // ( code-idx -- ) scatter strings[pop()] to all peers (dynamic code)
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
    // String pool operations (stack: idx is i64 index into self.strings)
    Type,                     // ( idx -- )  print strings[idx]
    StrEq,                    // ( idx-a idx-b -- bool )  string equality
    StrLen,                   // ( idx -- n )  byte length of strings[idx]
    StrCat,                   // ( idx-a idx-b -- idx-c )  concatenate
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
}

// ── Interpreter ───────────────────────────────────────────────────────────────

/// Signature for the optional confirm callback.
pub type ConfirmFn = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// Signature for the optional AI generation callback.
///
/// Called when `gen" prompt"` executes.  Receives the prompt string and returns
/// the model's response as a String.  If unset, `gen"` emits a placeholder.
pub type GenFn = Box<dyn Fn(&str) -> String + Send + Sync>;

pub struct Forth {
    data:       Vec<i64>,
    loop_stack: Vec<(i64, i64)>,         // (index, limit) for do/loop
    rstack:     Vec<i64>,                // >r / r> scratch stack
    memory:     Vec<Cell>,               // flat code memory (von Neumann)
    strings:    Vec<String>,             // string-literal pool
    name_index: HashMap<String, usize>,  // word name → entry address in memory
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
    /// Auto-expires on each `lock` call.  Max TTL = 30 s (nobody holds for long).
    locks: HashMap<String, Instant>,
    /// Optional confirm callback.
    confirm_fn: Option<ConfirmFn>,
    /// Optional AI generation callback.  Wired to the active generator in the REPL.
    gen_fn: Option<GenFn>,
}

const MAX_CALL_DEPTH: usize = 256;
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
: within    ( n lo hi -- flag )  over - >r - r> swap < ;

\ Sum of 1..n  ( n -- n*(n+1)/2 )
: sum-to-n  dup 1 + * 2 / ;

\ Greatest common divisor  ( a b -- gcd )
: gcd  begin dup while swap over mod repeat drop ;

\ Least common multiple  ( a b -- lcm )
: lcm  2dup gcd swap rot / * abs ;

\ Integer power  ( base exp -- base^exp )
: pow  ( base exp -- result )
    1 swap
    begin dup 0> while
        swap over * swap 1 -
    repeat
    drop ;

\ Fibonacci (recursive)  ( n -- fib(n) )
: fib   ( n -- fib(n) )
    dup 2 < if drop 1 exit then
    dup 1 - fib
    swap 2 - fib
    + ;

\ ── Comparison helpers ────────────────────────────────────────────────────────
: true      ( -- -1 )  -1 ;
: false     ( -- 0  )   0 ;
: bool      ( n -- flag )  0= 0= ;
: between   ( n lo hi -- flag )  rot rot over > rot over > and ;
: clamp     ( n lo hi -- n' )    rot over max over min swap drop swap drop ;

\ ── Stack utilities ──────────────────────────────────────────────────────────
: -rot      ( a b c -- c a b )   rot rot ;

\ ── Logic ────────────────────────────────────────────────────────────────────
: sign      ( n -- -1|0|1 )  dup 0> if drop 1 else 0< if -1 else 0 then then ;
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
: noop  ( -- )  ;
: ?dup  ( n -- n n | 0 )  dup if dup then ;
: tally ( n -- )  0 do ." |" loop cr ;

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
: scatter-code      ( code -- )              scatter-code ;
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
"#;

// ── Public API ────────────────────────────────────────────────────────────────

impl Forth {
    pub fn new() -> Self {
        let mut f = Forth {
            data:       Vec::new(),
            loop_stack: Vec::new(),
            rstack:     Vec::new(),
            memory:     Vec::new(),
            strings:    Vec::new(),
            name_index: HashMap::new(),
            heap:       Vec::new(),
            var_index:  HashMap::new(),
            out:             String::new(),
            peers:           Vec::new(),
            source_log:      Vec::new(),
            log_definitions: false, // off during stdlib load
            fuel:            usize::MAX, // unlimited while loading stdlib
            undo_stack:      Vec::new(),
            locks:           HashMap::new(),
            confirm_fn:      None,
            gen_fn:          None,
        };
        // Load standard library silently — not logged (every session has stdlib)
        let _ = f.eval(STDLIB);
        f.fuel = DEFAULT_FUEL; // restore budget for user code
        f.log_definitions = true; // user words from here on are logged
        f
    }

    /// Run Forth source and return collected output.
    pub fn run(source: &str) -> Result<String> {
        let mut f = Forth::new();
        f.eval(source)?;
        Ok(f.out)
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

    /// Clone the compiled dictionary state into a fresh VM (no stack, no callbacks).
    /// Used to share a pre-compiled VM baseline across tests without re-compiling STDLIB
    /// or vocabulary on every clone.  Callbacks (confirm_fn, gen_fn) are not copied.
    pub fn clone_dict(&self) -> Self {
        Forth {
            data:       Vec::new(),
            loop_stack: Vec::new(),
            rstack:     Vec::new(),
            memory:     self.memory.clone(),
            strings:    self.strings.clone(),
            name_index: self.name_index.clone(),
            heap:       self.heap.clone(),
            var_index:  self.var_index.clone(),
            out:        String::new(),
            peers:      self.peers.clone(),
            source_log: Vec::new(),    // fresh log — don't inherit parent's history
            log_definitions: true,
            fuel:       DEFAULT_FUEL,
            undo_stack: Vec::new(),
            locks:      HashMap::new(),
            confirm_fn: None,
            gen_fn:     None,
        }
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
        self.strings.truncate(snap.strings_len);
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
                    self.compile_into(&pending)?;
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
                            self.source_log[pos] = entry;
                        } else {
                            self.source_log.push(entry);
                        }
                    }
                    // Register entry address BEFORE compiling body so `recurse` resolves.
                    let word_addr = self.memory.len();
                    self.name_index.insert(name.clone(), word_addr);
                    self.compile_into(&body)?;
                    // Patch Addr(usize::MAX) recurse placeholders with the real word_addr.
                    for cell in &mut self.memory[word_addr..] {
                        if let Cell::Addr(a) = cell { if *a == usize::MAX { *a = word_addr; } }
                    }
                    self.memory.push(Cell::Ret);
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
    fn compile_into(&mut self, tokens: &[String]) -> Result<()> {
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].as_str() {
                "if" => {
                    i += 1;
                    let (true_branch, false_branch, skip) = collect_if(tokens, i)?;
                    i += skip;
                    let jmpz_pos = self.memory.len();
                    self.memory.push(Cell::JmpZ(0)); // forward: patch after true branch
                    self.compile_into(&true_branch)?;
                    if false_branch.is_empty() {
                        let after = self.memory.len();
                        self.memory[jmpz_pos] = Cell::JmpZ(after);
                    } else {
                        let jmp_pos = self.memory.len();
                        self.memory.push(Cell::Jmp(0)); // forward: patch after false branch
                        let false_start = self.memory.len();
                        self.memory[jmpz_pos] = Cell::JmpZ(false_start);
                        self.compile_into(&false_branch)?;
                        let after = self.memory.len();
                        self.memory[jmp_pos] = Cell::Jmp(after);
                    }
                }
                "begin" => {
                    i += 1;
                    let (body, end_kind, after_body, skip) = collect_begin(tokens, i)?;
                    i += skip;
                    let begin_addr = self.memory.len();
                    self.compile_into(&body)?;
                    match end_kind.as_str() {
                        "until" => { self.memory.push(Cell::Until(begin_addr)); }
                        "again" => { self.memory.push(Cell::Jmp(begin_addr)); }
                        "while" => {
                            let while_pos = self.memory.len();
                            self.memory.push(Cell::While(0)); // forward: patch to after repeat
                            self.compile_into(&after_body)?;
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
                    self.compile_into(&body)?;
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
                        self.compile_into(val_toks)?;
                        let of_pos = self.memory.len();
                        self.memory.push(Cell::OfTest(0)); // forward: patch to next of/endcase
                        self.compile_into(body_toks)?;
                        let jmp_pos = self.memory.len();
                        self.memory.push(Cell::Jmp(0)); // forward: patch to endcase
                        endcase_patches.push(jmp_pos);
                        let next = self.memory.len();
                        self.memory[of_pos] = Cell::OfTest(next);
                    }
                    if !default_block.is_empty() {
                        self.compile_into(&default_block)?;
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

    /// Emit a single token as one or more cells into `self.memory`.
    fn emit_token(&mut self, tok: &str) -> Result<()> {
        if let Some(s) = tok.strip_prefix("\x00str:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::Str(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00push-str:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::PushStr(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00confirm:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::Confirm(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00read:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::ReadFile(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00exec:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::ExecCmd(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00glob:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::GlobFiles(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00peer:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::AddPeer(idx));
            return Ok(());
        }
        if let Some(s) = tok.strip_prefix("\x00scatter:") {
            let idx = self.strings.len();
            self.strings.push(s.to_string());
            self.memory.push(Cell::ScatterExec(idx));
            return Ok(());
        }
        if tok == "\x00scatter-stack" {
            self.memory.push(Cell::ScatterStack);
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
        let lo = tok.to_lowercase();
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
        if let Some(b) = name_to_builtin(&lo) {
            self.memory.push(Cell::Builtin(b));
            return Ok(());
        }
        if let Some(&addr) = self.name_index.get(&lo) {
            self.memory.push(Cell::Addr(addr));
            return Ok(());
        }
        if let Some(&addr) = self.var_index.get(&lo) {
            self.memory.push(Cell::Lit(addr as i64));
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
        let mut call_stack: Vec<usize> = Vec::new();
        loop {
            if self.fuel == 0 {
                bail!("fuel exhausted — word is too expensive for vocabulary use.\n\
                       hint: use  N with-fuel  for intentional heavy computation.");
            }
            self.fuel -= 1;
            if ip >= self.memory.len() { break; }
            match self.memory[ip].clone() {
                Cell::Lit(n) => { self.data.push(n); ip += 1; }
                Cell::Str(idx) => {
                    let s = self.strings[idx].clone();
                    self.out.push_str(&s);
                    ip += 1;
                }
                Cell::PushStr(idx) => {
                    // s" literal" — push string pool index as an integer operand
                    self.data.push(idx as i64);
                    ip += 1;
                }
                Cell::Confirm(idx) => {
                    let msg = self.strings[idx].clone();
                    let approved = if let Some(ref f) = self.confirm_fn {
                        f(&msg)
                    } else {
                        true // auto-approve when no TUI callback (tests, pipe mode)
                    };
                    self.data.push(if approved { -1 } else { 0 });
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
                        self.out.push_str(
                            "scatter: no peers  (use peer\" host:port\" to register one)\n"
                        );
                    } else {
                        let peers = self.peers.clone();
                        let results = run_scatter(&peers, &snippet);
                        for r in results {
                            if let Some(err) = r.error {
                                self.out.push_str(&format!("[{}] error: {}\n", r.peer, err));
                            } else {
                                // Print any output lines
                                for line in r.output.lines() {
                                    self.out.push_str(&format!("[{}] {}\n", r.peer, line));
                                }
                                // Push the peer's stack values onto the local stack.
                                // This is the "forth back" — remote results become local values.
                                for v in &r.stack {
                                    self.data.push(*v);
                                }
                            }
                        }
                    }
                    ip += 1;
                }
                Cell::ScatterBashExec(idx) => {
                    let cmd = self.strings[idx].clone();
                    if self.peers.is_empty() {
                        self.out.push_str(
                            "scatter-exec: no peers  (use peer\" host:port\" to register one)\n"
                        );
                    } else {
                        let peers = self.peers.clone();
                        // Show plan and require confirmation before executing on remote machines.
                        let plan = format!(
                            "Run on {} peer(s): bash -c {:?}\n  targets: {}",
                            peers.len(),
                            cmd,
                            peers.join(", ")
                        );
                        let approved = if let Some(ref f) = self.confirm_fn {
                            f(&plan)
                        } else {
                            true // auto-approve in tests / pipe mode
                        };
                        if !approved {
                            self.out.push_str("scatter-exec: cancelled\n");
                        } else {
                            let results = run_exec_scatter(&peers, &cmd);
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
                            "scatter-code: no peers  (use peer\" host:port\" to register one)\n"
                        );
                    } else {
                        let peers = self.peers.clone();
                        let results = run_scatter(&peers, &snippet);
                        for r in results {
                            if let Some(err) = r.error {
                                self.out.push_str(&format!("[{}] error: {}\n", r.peer, err));
                            } else {
                                for line in r.output.lines() {
                                    self.out.push_str(&format!("[{}] {}\n", r.peer, line));
                                }
                                for v in &r.stack {
                                    self.data.push(*v);
                                }
                            }
                        }
                    }
                    ip += 1;
                }
                Cell::GenAI(idx) => {
                    let prompt = self.strings[idx].clone();
                    let response = if let Some(ref f) = self.gen_fn {
                        f(&prompt)
                    } else {
                        "(no generator connected)\n".to_string()
                    };
                    self.out.push_str(&response);
                    if !response.ends_with('\n') {
                        self.out.push('\n');
                    }
                    ip += 1;
                }
                Cell::Builtin(b) => { self.exec_builtin(b)?; ip += 1; }
                Cell::Addr(addr) => {
                    if call_stack.len() >= MAX_CALL_DEPTH { bail!("return stack overflow"); }
                    call_stack.push(ip + 1); // return address
                    ip = addr;
                }
                Cell::Ret => {
                    match call_stack.pop() {
                        Some(ret) => { ip = ret; }
                        None      => break, // top-level return: halt
                    }
                }
                Cell::Jmp(addr)  => { ip = addr; }
                Cell::JmpZ(addr) => { let v = self.pop()?; if v == 0 { ip = addr; } else { ip += 1; } }
                Cell::Until(back) => { let v = self.pop()?; if v == 0 { ip = back; } else { ip += 1; } }
                Cell::While(exit) => { let v = self.pop()?; if v == 0 { ip = exit; } else { ip += 1; } }
                Cell::Repeat(back) => { ip = back; }
                Cell::DoSetup => {
                    let index = self.pop()?;
                    let limit = self.pop()?;
                    self.loop_stack.push((index, limit));
                    ip += 1;
                }
                Cell::DoLoop(back) => {
                    if let Some(top) = self.loop_stack.last_mut() {
                        top.0 += 1;
                        if top.0 < top.1 { ip = back; continue; }
                    }
                    self.loop_stack.pop();
                    ip += 1;
                }
                Cell::DoLoopPlus(back) => {
                    let step = self.pop()?;
                    if let Some(top) = self.loop_stack.last_mut() {
                        top.0 += step;
                        if top.0 < top.1 { ip = back; continue; }
                    }
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
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn exec_builtin(&mut self, b: Builtin) -> Result<()> {
        match b {
            Builtin::Plus  => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.wrapping_add(b)); }
            Builtin::Minus => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.wrapping_sub(b)); }
            Builtin::Star  => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.wrapping_mul(b)); }
            Builtin::Slash => { let b = self.pop()?; let a = self.pop()?; if b == 0 { bail!("division by zero"); } self.data.push(a / b); }
            Builtin::Mod   => { let b = self.pop()?; let a = self.pop()?; if b == 0 { bail!("division by zero"); } self.data.push(a % b); }
            Builtin::Dup   => { let a = self.pop()?; self.data.push(a); self.data.push(a); }
            Builtin::Drop  => { self.pop()?; }
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
            Builtin::Eq    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a == b { -1 } else { 0 }); }
            Builtin::Lt    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a < b { -1 } else { 0 }); }
            Builtin::Gt    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a > b { -1 } else { 0 }); }
            Builtin::Le    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a <= b { -1 } else { 0 }); }
            Builtin::Ge    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a >= b { -1 } else { 0 }); }
            Builtin::Ne    => { let b = self.pop()?; let a = self.pop()?; self.data.push(if a != b { -1 } else { 0 }); }
            Builtin::ZeroEq => { let a = self.pop()?; self.data.push(if a == 0 { -1 } else { 0 }); }
            Builtin::ZeroLt => { let a = self.pop()?; self.data.push(if a < 0 { -1 } else { 0 }); }
            Builtin::ZeroGt => { let a = self.pop()?; self.data.push(if a > 0 { -1 } else { 0 }); }
            Builtin::And   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a & b); }
            Builtin::Or    => { let b = self.pop()?; let a = self.pop()?; self.data.push(a | b); }
            Builtin::Xor   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a ^ b); }
            Builtin::Invert => { let a = self.pop()?; self.data.push(!a); }
            Builtin::Negate => { let a = self.pop()?; self.data.push(a.wrapping_neg()); }
            Builtin::Abs   => { let a = self.pop()?; self.data.push(a.abs()); }
            Builtin::Max   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.max(b)); }
            Builtin::Min   => { let b = self.pop()?; let a = self.pop()?; self.data.push(a.min(b)); }
            Builtin::Lshift => { let n = self.pop()?; let a = self.pop()?; self.data.push(a << (n & 63)); }
            Builtin::Rshift => { let n = self.pop()?; let a = self.pop()?; self.data.push(a >> (n & 63)); }
            Builtin::Print  => { let a = self.pop()?; self.out.push_str(&format!("{a} ")); }
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
            Builtin::Allot => {
                let n = self.pop()? as usize;
                for _ in 0..n { self.heap.push(0); }
            }
            Builtin::Cells => {
                // n cells → n (each cell is 1 unit; caller uses + for array indexing)
                // In our interpreter, addresses are direct heap indices so cells = identity
            }
            Builtin::Words => {
                let mut names: Vec<String> = self.name_index.keys().cloned().collect();
                names.sort();
                self.out.push_str(&names.join("  "));
                self.out.push('\n');
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
                // Acquire advisory lock.  Max TTL = 30 000 ms.  Returns -1 on success, 0 if held.
                const MAX_TTL_MS: u64 = 30_000;
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
                    for (host, port) in &found {
                        let addr = format!("{host}:{port}");
                        if !self.peers.contains(&addr) {
                            self.peers.push(addr.clone());
                            self.out.push_str(&format!("  + {addr}\n"));
                        }
                    }
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
        }
        Ok(())
    }

    fn pop(&mut self) -> Result<i64> {
        self.data.pop().ok_or_else(|| anyhow::anyhow!("stack underflow"))
    }
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

// ── Scatter helpers — bridge sync Forth VM to async scatter functions ──────────

fn run_scatter(peers: &[String], code: &str) -> Vec<crate::coforth::scatter::PeerResult> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| {
            handle.block_on(crate::coforth::scatter::scatter_exec(peers, code))
        })
    } else {
        tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(crate::coforth::scatter::scatter_exec(peers, code))
    }
}

/// Public entry point for background boot discovery (called from event_loop).
pub fn run_peers_discover_pub(timeout_ms: u64) -> Vec<(String, u16)> {
    run_peers_discover(timeout_ms)
}

/// Synchronous mDNS discovery — returns (host, port) pairs for all found finch instances.
/// Blocks for at most `timeout_ms` milliseconds.
fn run_peers_discover(timeout_ms: u64) -> Vec<(String, u16)> {
    use std::time::Duration;
    let timeout = Duration::from_millis(timeout_ms);

    let inner = || -> anyhow::Result<Vec<(String, u16)>> {
        let client = crate::service::discovery_client::ServiceDiscoveryClient::new()?;
        let services = client.discover(timeout)?;
        Ok(services.into_iter().map(|s| (s.host, s.port)).collect())
    };

    // discover() uses recv_timeout internally — it's a blocking call.
    // Must use block_in_place when inside a tokio worker thread.
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(inner)
    } else {
        inner()
    }
    .unwrap_or_default()
}

fn run_exec_scatter(peers: &[String], cmd: &str) -> Vec<crate::coforth::scatter::PeerResult> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| {
            handle.block_on(crate::coforth::scatter::scatter_exec_bash(peers, cmd))
        })
    } else {
        tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(crate::coforth::scatter::scatter_exec_bash(peers, cmd))
    }
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
        "words" => Builtin::Words, "random" => Builtin::Random, "time" => Builtin::Time,
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
        // String pool
        "type"    => Builtin::Type,
        "str="    => Builtin::StrEq,
        "str-len" => Builtin::StrLen,
        "str-cat" => Builtin::StrCat,
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
        _ => return None,
    })
}

// ── Tokenizer ─────────────────────────────────────────────────────────────────

fn tokenize(src: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    let mut tok = String::new();

    macro_rules! flush { () => { if !tok.is_empty() { tokens.push(tok.clone()); tok.clear(); } }; }

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
                tok.push('.');
            }
        } else if c == '"' && tok == "confirm" {
            // confirm" message" — like ." but emits Cell::Confirm instead of Cell::Str
            tok.clear();
            chars.next(); // consume "
            if chars.peek() == Some(&' ') { chars.next(); } // skip separator space
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00confirm:{s}"));
        } else if c == '"' && tok == "read" {
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00read:{s}"));
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
        } else if c == '"' && tok == "scatter" {
            // scatter" code"  — run code on all registered peers in parallel
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00scatter:{s}"));
        } else if c == '"' && tok == "s" {
            // s" text"  — push string pool index as integer operand (no printing)
            tok.clear();
            chars.next();
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '"' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
        } else if c == '|' && tok == "s" {
            // s| text with "quotes" |  — alternate string delimiter; avoids escaping hell
            tok.clear();
            chars.next(); // consume |
            if chars.peek() == Some(&' ') { chars.next(); }
            let mut s = String::new();
            for c2 in chars.by_ref() { if c2 == '|' { break; } s.push(c2); }
            tokens.push(format!("\x00push-str:{s}"));
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
        } else if c.is_whitespace() {
            flush!();
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
        // Word should no longer be callable
        assert!(vm.exec("hello").is_err(), "hello should be undefined after forget");
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
        assert!(vm.exec("hello").is_err(), "hello should be gone after undo");
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
        assert!(vm.exec("c").is_err(), "c should be gone");
        assert!(vm.exec("b").is_err(), "b should be gone");
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
}
