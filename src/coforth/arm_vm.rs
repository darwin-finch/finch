/// Minimal AArch64 VM for stack-operation proofs.
///
/// Not a full CPU emulator — just enough to run and verify stack operations
/// so they can participate in `(a, b, check, conclusion)` proofs against
/// Co-Forth equivalents.
///
/// Register file: x0–x30 (indices 0–30), sp (index 31).
/// Memory: a flat Vec<i64> used as a descending stack.
/// Addressing: sp points to the top-of-stack slot (last written cell).
///
/// Supported instructions
/// ───────────────────────
///   mov  xD, #imm          load immediate into register
///   mov  xD, xN            copy register
///   ldr  xD, [xN]          load from memory[reg[N]]
///   ldr  xD, [xN], #imm    post-increment load  (pop pattern)
///   ldr  xD, [xN, #imm]    offset load
///   str  xD, [xN]          store to memory[reg[N]]
///   str  xD, [xN, #imm]!   pre-decrement store  (push pattern)
///   str  xD, [xN, #imm]    offset store
///   add  xD, xN, xM        register add
///   add  xD, xN, #imm      immediate add
///   sub  xD, xN, xM        register subtract
///   sub  xD, xN, #imm      immediate subtract
///   mul  xD, xN, xM        register multiply
///   ret                    halt execution
///
/// Addressing notes
/// ─────────────────
/// Memory is indexed in i64 cells (8 bytes each).  The sp starts at the top
/// of a 256-cell region.  Negative immediate offsets (e.g. `#-8`) work in
/// units of one cell (not bytes), keeping the model simple.

use anyhow::{anyhow, Result};

const MEM_SIZE: usize = 256;
const SP_INIT: i64 = MEM_SIZE as i64; // sp starts just past the top; first push → MEM_SIZE-1

/// Parsed AArch64 instruction (subset).
#[derive(Debug, Clone, PartialEq)]
pub enum Instr {
    MovImm { dst: usize, imm: i64 },
    MovReg { dst: usize, src: usize },
    LdrBase { dst: usize, base: usize },
    LdrOffset { dst: usize, base: usize, offset: i64 },
    LdrPostInc { dst: usize, base: usize, inc: i64 },
    StrBase { src: usize, base: usize },
    StrOffset { src: usize, base: usize, offset: i64 },
    StrPreDec { src: usize, base: usize, offset: i64 }, // [base, #offset]!
    AddReg { dst: usize, lhs: usize, rhs: usize },
    AddImm { dst: usize, lhs: usize, imm: i64 },
    SubReg { dst: usize, lhs: usize, rhs: usize },
    SubImm { dst: usize, lhs: usize, imm: i64 },
    MulReg { dst: usize, lhs: usize, rhs: usize },
    Ret,
}

/// The VM state.
pub struct ArmVm {
    pub regs: [i64; 32], // x0–x30 + sp (index 31)
    pub mem:  Vec<i64>,  // flat cell memory
}

impl Default for ArmVm {
    fn default() -> Self {
        Self::new()
    }
}

impl ArmVm {
    pub fn new() -> Self {
        let mut vm = Self {
            regs: [0i64; 32],
            mem: vec![0i64; MEM_SIZE],
        };
        vm.regs[31] = SP_INIT; // sp
        vm
    }

    /// Run a sequence of instructions.  Stops on `Ret` or when the list is exhausted.
    pub fn run(&mut self, instrs: &[Instr]) -> Result<()> {
        for instr in instrs {
            match instr {
                Instr::MovImm { dst, imm } => {
                    self.regs[*dst] = *imm;
                }
                Instr::MovReg { dst, src } => {
                    self.regs[*dst] = self.regs[*src];
                }
                Instr::LdrBase { dst, base } => {
                    let addr = self.regs[*base] as usize;
                    self.regs[*dst] = *self.mem.get(addr)
                        .ok_or_else(|| anyhow!("ldr: address {addr} out of bounds"))?;
                }
                Instr::LdrOffset { dst, base, offset } => {
                    let addr = (self.regs[*base] + offset) as usize;
                    self.regs[*dst] = *self.mem.get(addr)
                        .ok_or_else(|| anyhow!("ldr offset: address {addr} out of bounds"))?;
                }
                Instr::LdrPostInc { dst, base, inc } => {
                    let addr = self.regs[*base] as usize;
                    self.regs[*dst] = *self.mem.get(addr)
                        .ok_or_else(|| anyhow!("ldr post-inc: address {addr} out of bounds"))?;
                    self.regs[*base] += inc;
                }
                Instr::StrBase { src, base } => {
                    let addr = self.regs[*base] as usize;
                    let cell = self.mem.get_mut(addr)
                        .ok_or_else(|| anyhow!("str: address {addr} out of bounds"))?;
                    *cell = self.regs[*src];
                }
                Instr::StrOffset { src, base, offset } => {
                    let addr = (self.regs[*base] + offset) as usize;
                    let cell = self.mem.get_mut(addr)
                        .ok_or_else(|| anyhow!("str offset: address {addr} out of bounds"))?;
                    *cell = self.regs[*src];
                }
                Instr::StrPreDec { src, base, offset } => {
                    self.regs[*base] += offset; // offset is negative for push
                    let addr = self.regs[*base] as usize;
                    let cell = self.mem.get_mut(addr)
                        .ok_or_else(|| anyhow!("str pre-dec: address {addr} out of bounds"))?;
                    *cell = self.regs[*src];
                }
                Instr::AddReg { dst, lhs, rhs } => {
                    self.regs[*dst] = self.regs[*lhs].wrapping_add(self.regs[*rhs]);
                }
                Instr::AddImm { dst, lhs, imm } => {
                    self.regs[*dst] = self.regs[*lhs].wrapping_add(*imm);
                }
                Instr::SubReg { dst, lhs, rhs } => {
                    self.regs[*dst] = self.regs[*lhs].wrapping_sub(self.regs[*rhs]);
                }
                Instr::SubImm { dst, lhs, imm } => {
                    self.regs[*dst] = self.regs[*lhs].wrapping_sub(*imm);
                }
                Instr::MulReg { dst, lhs, rhs } => {
                    self.regs[*dst] = self.regs[*lhs].wrapping_mul(self.regs[*rhs]);
                }
                Instr::Ret => break,
            }
        }
        Ok(())
    }

    /// Return the top of the VM's stack (the cell at mem[sp]).
    pub fn stack_top(&self) -> Option<i64> {
        let sp = self.regs[31] as usize;
        if sp >= MEM_SIZE { return None; }
        Some(self.mem[sp])
    }

    /// Return the full stack as a Vec (bottom to top).
    pub fn stack_snapshot(&self) -> Vec<i64> {
        let sp = self.regs[31] as usize;
        if sp >= MEM_SIZE { return vec![]; }
        self.mem[sp..MEM_SIZE].to_vec()
    }
}

// ── Parser ─────────────────────────────────────────────────────────────────────

/// Parse a register name ("x0"–"x30", "sp") into an index (0–31).
fn parse_reg(s: &str) -> Result<usize> {
    let s = s.trim().trim_matches(',');
    if s == "sp" { return Ok(31); }
    if let Some(n) = s.strip_prefix('x') {
        let idx: usize = n.parse().map_err(|_| anyhow!("bad register: {s}"))?;
        if idx <= 30 { return Ok(idx); }
    }
    Err(anyhow!("unknown register: {s}"))
}

/// Parse an immediate value ("#5", "#-8", "5").
fn parse_imm(s: &str) -> Result<i64> {
    let s = s.trim().trim_matches(',').trim_start_matches('#');
    s.parse::<i64>().map_err(|_| anyhow!("bad immediate: {s}"))
}

/// Parse one line of assembly into an `Instr`.
pub fn parse_instr(line: &str) -> Result<Instr> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(';') || line.starts_with("//") {
        return Err(anyhow!("empty or comment"));
    }

    // Strip inline comments.
    let line = line.split(';').next().unwrap_or(line).trim();
    let line = line.split("//").next().unwrap_or(line).trim();

    let mut tokens = line.splitn(2, char::is_whitespace);
    let mnemonic = tokens.next().unwrap_or("").to_lowercase();
    let rest = tokens.next().unwrap_or("").trim();

    match mnemonic.as_str() {
        "ret" => return Ok(Instr::Ret),
        _ => {}
    }

    // Split operands on ',' but be careful: memory expressions contain commas too.
    // Strategy: split on ',' outside brackets.
    let operands = split_operands(rest);

    match mnemonic.as_str() {
        "mov" => {
            let dst = parse_reg(operands.get(0).copied().unwrap_or(""))?;
            let src = operands.get(1).copied().unwrap_or("").trim();
            if src.starts_with('#') {
                Ok(Instr::MovImm { dst, imm: parse_imm(src)? })
            } else {
                Ok(Instr::MovReg { dst, src: parse_reg(src)? })
            }
        }
        "ldr" => {
            let dst = parse_reg(operands.get(0).copied().unwrap_or(""))?;
            let mem_expr = operands.get(1).copied().unwrap_or("").trim();
            parse_ldr(dst, mem_expr)
        }
        "str" => {
            let src = parse_reg(operands.get(0).copied().unwrap_or(""))?;
            let mem_expr = operands.get(1).copied().unwrap_or("").trim();
            parse_str(src, mem_expr)
        }
        "add" => {
            let dst = parse_reg(operands.get(0).copied().unwrap_or(""))?;
            let lhs = parse_reg(operands.get(1).copied().unwrap_or(""))?;
            let rhs_s = operands.get(2).copied().unwrap_or("").trim();
            if rhs_s.starts_with('#') {
                Ok(Instr::AddImm { dst, lhs, imm: parse_imm(rhs_s)? })
            } else {
                Ok(Instr::AddReg { dst, lhs, rhs: parse_reg(rhs_s)? })
            }
        }
        "sub" => {
            let dst = parse_reg(operands.get(0).copied().unwrap_or(""))?;
            let lhs = parse_reg(operands.get(1).copied().unwrap_or(""))?;
            let rhs_s = operands.get(2).copied().unwrap_or("").trim();
            if rhs_s.starts_with('#') {
                Ok(Instr::SubImm { dst, lhs, imm: parse_imm(rhs_s)? })
            } else {
                Ok(Instr::SubReg { dst, lhs, rhs: parse_reg(rhs_s)? })
            }
        }
        "mul" => {
            let dst = parse_reg(operands.get(0).copied().unwrap_or(""))?;
            let lhs = parse_reg(operands.get(1).copied().unwrap_or(""))?;
            let rhs = parse_reg(operands.get(2).copied().unwrap_or(""))?;
            Ok(Instr::MulReg { dst, lhs, rhs })
        }
        _ => Err(anyhow!("unknown mnemonic: {mnemonic}")),
    }
}

fn parse_ldr(dst: usize, expr: &str) -> Result<Instr> {
    // [xN]           → LdrBase
    // [xN, #imm]     → LdrOffset
    // [xN], #imm     → LdrPostInc
    let expr = expr.trim();
    if let Some(rest) = expr.strip_suffix(']') {
        // No post-increment.
        let inner = rest.trim_start_matches('[');
        if let Some((base_s, off_s)) = inner.split_once(',') {
            let base = parse_reg(base_s)?;
            let offset = parse_imm(off_s)?;
            Ok(Instr::LdrOffset { dst, base, offset })
        } else {
            let base = parse_reg(inner)?;
            Ok(Instr::LdrBase { dst, base })
        }
    } else if let Some((bracket, post)) = expr.split_once(']') {
        // Post-increment: [xN], #imm
        let inner = bracket.trim_start_matches('[');
        let base = parse_reg(inner)?;
        let inc = parse_imm(post.trim_start_matches(',').trim())?;
        Ok(Instr::LdrPostInc { dst, base, inc })
    } else {
        Err(anyhow!("ldr: bad memory expression: {expr}"))
    }
}

fn parse_str(src: usize, expr: &str) -> Result<Instr> {
    // [xN]           → StrBase
    // [xN, #imm]     → StrOffset
    // [xN, #imm]!    → StrPreDec
    let expr = expr.trim();
    if expr.ends_with("!") {
        // Pre-decrement.
        let inner = expr.trim_end_matches('!').trim();
        let inner = inner.trim_start_matches('[').trim_end_matches(']');
        if let Some((base_s, off_s)) = inner.split_once(',') {
            let base = parse_reg(base_s)?;
            let offset = parse_imm(off_s)?;
            Ok(Instr::StrPreDec { src, base, offset })
        } else {
            Err(anyhow!("str pre-dec: expected [base, #offset]!"))
        }
    } else if let Some(rest) = expr.strip_suffix(']') {
        let inner = rest.trim_start_matches('[');
        if let Some((base_s, off_s)) = inner.split_once(',') {
            let base = parse_reg(base_s)?;
            let offset = parse_imm(off_s)?;
            Ok(Instr::StrOffset { src, base, offset })
        } else {
            let base = parse_reg(inner)?;
            Ok(Instr::StrBase { src, base })
        }
    } else {
        Err(anyhow!("str: bad memory expression: {expr}"))
    }
}

/// Split "x0, x1, [sp, #-8]!" correctly — commas inside brackets are not separators.
fn split_operands(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                result.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    result.push(s[start..].trim());
    result
}

/// Parse a multi-instruction assembly string (semicolons or newlines as separators).
pub fn parse_program(src: &str) -> Result<Vec<Instr>> {
    let mut instrs = Vec::new();
    for line in src.split(|c| c == ';' || c == '\n') {
        match parse_instr(line) {
            Ok(i) => instrs.push(i),
            Err(_) => {} // skip blanks and comments
        }
    }
    if instrs.is_empty() {
        return Err(anyhow!("arm: empty program"));
    }
    Ok(instrs)
}

/// Run an assembly string and return the result.
///
/// Return convention (ARM ABI): the result is in `x0` after execution.
/// If code also pushes to the memory stack (sp-based), `stack_top()` holds
/// that value — but for register-based proofs the caller leaves the answer
/// in x0, matching the real ARM calling convention.
pub fn run_asm(src: &str) -> Result<i64> {
    let instrs = parse_program(src)?;
    let mut vm = ArmVm::new();
    vm.run(&instrs)?;
    // Prefer the memory stack if anything was pushed; otherwise fall back to x0.
    // This lets stack-based programs (dup, swap, etc.) and register-based
    // arithmetic programs both work without extra boilerplate.
    if let Some(top) = vm.stack_top() {
        Ok(top)
    } else {
        Ok(vm.regs[0])
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sp() -> usize { 31 }

    #[test]
    fn test_push_pop() {
        // push 42: str x0, [sp, #-1]!   (1 cell = 1 unit in our model)
        // pop  x1: ldr x1, [sp], #1
        let mut vm = ArmVm::new();
        vm.run(&[
            Instr::MovImm { dst: 0, imm: 42 },
            Instr::StrPreDec { src: 0, base: sp(), offset: -1 },
            Instr::LdrPostInc { dst: 1, base: sp(), inc: 1 },
        ]).unwrap();
        assert_eq!(vm.regs[1], 42);
    }

    #[test]
    fn test_dup() {
        // Push 5, then dup: ldr x0, [sp]; str x0, [sp, #-1]!
        let mut vm = ArmVm::new();
        vm.run(&[
            Instr::MovImm { dst: 0, imm: 5 },
            Instr::StrPreDec { src: 0, base: sp(), offset: -1 }, // push 5
            Instr::LdrBase   { dst: 0, base: sp() },              // ldr x0, [sp]
            Instr::StrPreDec { src: 0, base: sp(), offset: -1 }, // push x0 (dup)
        ]).unwrap();
        let stack = vm.stack_snapshot();
        assert_eq!(stack, vec![5, 5], "dup must leave two copies");
    }

    #[test]
    fn test_add() {
        // push 3; push 4; pop x0; pop x1; add x0,x0,x1; push result
        let mut vm = ArmVm::new();
        vm.run(&[
            Instr::MovImm   { dst: 0, imm: 3 },
            Instr::StrPreDec { src: 0, base: sp(), offset: -1 },
            Instr::MovImm   { dst: 0, imm: 4 },
            Instr::StrPreDec { src: 0, base: sp(), offset: -1 },
            Instr::LdrPostInc { dst: 0, base: sp(), inc: 1 },
            Instr::LdrPostInc { dst: 1, base: sp(), inc: 1 },
            Instr::AddReg   { dst: 0, lhs: 0, rhs: 1 },
            Instr::StrPreDec { src: 0, base: sp(), offset: -1 },
        ]).unwrap();
        assert_eq!(vm.stack_top(), Some(7));
    }

    #[test]
    fn test_parse_push_pop() {
        let prog = parse_program("mov x0, #99; str x0, [sp, #-1]!").unwrap();
        assert_eq!(prog.len(), 2);
        assert_eq!(prog[0], Instr::MovImm { dst: 0, imm: 99 });
        assert_eq!(prog[1], Instr::StrPreDec { src: 0, base: 31, offset: -1 });
    }

    #[test]
    fn test_run_asm_push_value() {
        // Push 7 onto the ARM stack; top should be 7.
        let top = run_asm("mov x0, #7; str x0, [sp, #-1]!").unwrap();
        assert_eq!(top, 7);
    }

    #[test]
    fn test_dup_equivalent_to_forth_dup() {
        // ARM dup: push N, ldr x0,[sp], str x0,[sp,#-1]!  → top == N
        // Forth dup: N dup  → top == N
        // Both should produce the same top-of-stack for N=5.
        let arm_top = run_asm("mov x0, #5; str x0, [sp, #-1]!; ldr x0, [sp]; str x0, [sp, #-1]!").unwrap();
        let forth_top = crate::coforth::interpreter::Forth::run("5 dup .")
            .unwrap()
            .trim()
            .parse::<i64>()
            .unwrap();
        assert_eq!(arm_top, forth_top, "ARM dup must agree with Forth dup");
    }
}
