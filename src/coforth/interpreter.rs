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
    Confirm(usize),     // ask user strings[idx]; push -1 (yes) or 0 (no)
    ReadFile(usize),    // read file at strings[idx]; emit contents to out
    ExecCmd(usize),     // run shell command strings[idx]; emit stdout to out
    GlobFiles(usize),   // list files matching glob strings[idx]; emit to out
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
    // Integer math extras
    Sqrt,                     // ( n -- isqrt(n) )  integer square root
    Floor,                    // ( n d -- n/d*d )   floor division result
    Ceil,                     // ( n d -- ceil )    ceiling division
    // Trig (scaled: input in degrees * 1000, output * 1000)
    Sin,                      // ( deg*1000 -- sin*1000 )
    Cos,                      // ( deg*1000 -- cos*1000 )
    // Fixed-point helpers
    FPMul,                    // ( a b scale -- a*b/scale )  fixed-point multiply
}

// ── Interpreter ───────────────────────────────────────────────────────────────

/// Signature for the optional confirm callback.
///
/// Called when the `confirm` word executes.  Returns `true` (continue) or
/// `false` (deny).  The string argument is a label/message from the program.
pub type ConfirmFn = Box<dyn Fn(&str) -> bool + Send + Sync>;

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
    /// Optional confirm callback.  `confirm" msg"` calls this and pushes -1 or 0.
    /// If unset, defaults to `true` (auto-approve — useful in tests / CLI).
    confirm_fn: Option<ConfirmFn>,
}

const MAX_CALL_DEPTH: usize = 256;
const MAX_STEPS: usize = 2_000_000;

/// Snapshot of the Forth dictionary state (not the data stack).
/// Used to implement undo: restore the dictionary to a previous point.
#[derive(Clone)]
pub struct DictionarySnapshot {
    memory_len:  usize,
    strings_len: usize,
    heap_len:    usize,
    name_index:  HashMap<String, usize>,
    var_index:   HashMap<String, usize>,
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
            out:        String::new(),
            confirm_fn: None,
        };
        // Load standard library silently (errors ignored — all words should be valid)
        let _ = f.eval(STDLIB);
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

    /// Execute Forth source on this instance and return collected output.
    pub fn exec(&mut self, source: &str) -> Result<String> {
        self.out.clear();
        self.eval(source)?;
        Ok(self.out.clone())
    }

    /// Snapshot the current dictionary state (word definitions only, not the data stack).
    pub fn snapshot(&self) -> DictionarySnapshot {
        DictionarySnapshot {
            memory_len:  self.memory.len(),
            strings_len: self.strings.len(),
            heap_len:    self.heap.len(),
            name_index:  self.name_index.clone(),
            var_index:   self.var_index.clone(),
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
        let mut steps = 0usize;
        loop {
            steps += 1;
            if steps > MAX_STEPS { bail!("step limit exceeded — possible infinite loop"); }
            if ip >= self.memory.len() { break; }
            match self.memory[ip].clone() {
                Cell::Lit(n) => { self.data.push(n); ip += 1; }
                Cell::Str(idx) => {
                    let s = self.strings[idx].clone();
                    self.out.push_str(&s);
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
                    // Simple glob: expand via shell to avoid adding a dep
                    let result = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(format!("ls -1 {pattern} 2>/dev/null"))
                        .output();
                    match result {
                        Ok(o) => self.out.push_str(&String::from_utf8_lossy(&o.stdout)),
                        Err(e) => self.out.push_str(&format!("glob error: {e}\n")),
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
        }
        Ok(())
    }

    fn pop(&mut self) -> Result<i64> {
        self.data.pop().ok_or_else(|| anyhow::anyhow!("stack underflow"))
    }
}

// ── Name table ────────────────────────────────────────────────────────────────

fn name_to_builtin(name: &str) -> Option<Builtin> {
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
        "sqrt" | "isqrt" => Builtin::Sqrt,
        "floor" => Builtin::Floor,
        "ceil" | "ceiling" => Builtin::Ceil,
        "sin" => Builtin::Sin,
        "cos" => Builtin::Cos,
        "fpmul" => Builtin::FPMul,
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
fn collect_begin(tokens: &[String], start: usize) -> Result<(Vec<String>, String, Vec<String>, usize)> {
    let mut body = Vec::new();
    let mut after = Vec::new();
    let mut end_kind = String::new();
    let mut depth = 1i32;
    let mut in_after = false;
    let mut i = start;
    while i < tokens.len() {
        match tokens[i].as_str() {
            "begin" => { depth += 1; (if in_after { &mut after } else { &mut body }).push(tokens[i].clone()); }
            "until" | "again" if depth == 1 => { end_kind = tokens[i].clone(); break; }
            "while" if depth == 1 => { end_kind = "while".to_string(); in_after = true; }
            "repeat" if depth == 1 && in_after => { break; }
            "until" | "again" | "while" | "repeat" => {
                depth -= 1;
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
}
