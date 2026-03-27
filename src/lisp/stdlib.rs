/// Standard library — registers all synchronous built-ins into an environment.
///
/// Async operations (SSH, network I/O) are handled as special forms in eval.rs
/// because they need `.await` and access to LispCtx.
///
/// Crypto primitives here are synchronous: SHA-256 / HMAC-SHA256 / Ed25519 /
/// ChaCha20-Poly1305 / X25519 are all CPU-bound, sub-millisecond operations.
use anyhow::{bail, Result};
use std::sync::Arc;

use super::env::{Env, EnvRef};
use super::types::{NativeFn, Val};

/// Register all builtins into `env`.
pub fn register_all(env: &EnvRef) {
    let builtins: &[(&'static str, fn(&[Val]) -> Result<Val>)] = &[
        // ── Arithmetic ────────────────────────────────────────────────────────
        ("+",        arith_add),
        ("-",        arith_sub),
        ("*",        arith_mul),
        ("/",        arith_div),
        ("mod",      arith_mod),
        ("quotient", arith_quotient),
        ("remainder",arith_remainder),
        ("abs",      arith_abs),
        ("max",      arith_max),
        ("min",      arith_min),
        ("floor",    arith_floor),
        ("ceiling",  arith_ceiling),
        ("round",    arith_round),
        ("sqrt",     arith_sqrt),
        ("expt",     arith_expt),
        // ── Comparison ────────────────────────────────────────────────────────
        ("=",        cmp_eq),
        ("<",        cmp_lt),
        (">",        cmp_gt),
        ("<=",       cmp_le),
        (">=",       cmp_ge),
        ("not",      logic_not),
        ("zero?",    pred_zero),
        ("positive?",pred_positive),
        ("negative?",pred_negative),
        ("even?",    pred_even),
        ("odd?",     pred_odd),
        // ── Type predicates ───────────────────────────────────────────────────
        ("null?",      pred_null),
        ("pair?",      pred_pair),
        ("list?",      pred_list),
        ("number?",    pred_number),
        ("integer?",   pred_integer),
        ("string?",    pred_string),
        ("symbol?",    pred_symbol),
        ("boolean?",   pred_boolean),
        ("bytes?",     pred_bytes),
        ("procedure?", pred_procedure),
        ("ssh-session?", pred_ssh),
        // ── List operations ───────────────────────────────────────────────────
        ("cons",      list_cons),
        ("car",       list_car),
        ("cdr",       list_cdr),
        ("cadr",      list_cadr),
        ("caddr",     list_caddr),
        ("list",      list_list),
        ("length",    list_length),
        ("append",    list_append),
        ("reverse",   list_reverse),
        ("list-ref",  list_ref),
        ("list-tail", list_tail),
        ("map",       list_map_stub),  // full map needs apply — handled in eval
        ("assoc",     list_assoc),
        ("member",    list_member),
        ("for-each",  list_for_each_stub),
        // ── String operations ─────────────────────────────────────────────────
        ("string-append",     str_append),
        ("string-length",     str_length),
        ("substring",         str_substring),
        ("string->number",    str_to_number),
        ("number->string",    num_to_string),
        ("string-upcase",     str_upcase),
        ("string-downcase",   str_downcase),
        ("string-contains",   str_contains),
        ("string-split",      str_split),
        ("string-trim",       str_trim),
        ("string->symbol",    str_to_symbol),
        ("symbol->string",    symbol_to_str),
        ("string->list",      str_to_list),
        ("list->string",      list_to_str),
        // ── Bytes operations ──────────────────────────────────────────────────
        ("string->bytes",     str_to_bytes),
        ("bytes->string",     bytes_to_str),
        ("bytes->hex",        bytes_to_hex),
        ("hex->bytes",        hex_to_bytes),
        ("bytes-length",      bytes_length),
        ("bytes-ref",         bytes_ref),
        ("bytes-append",      bytes_append),
        ("bytes",             make_bytes),
        ("random-bytes",      random_bytes),
        ("subbytes",          subbytes),
        // ── I/O ───────────────────────────────────────────────────────────────
        ("display",   io_display),
        ("newline",   io_newline),
        ("error",     io_error),
        ("format",    io_format_stub),
        // ── Crypto ───────────────────────────────────────────────────────────
        ("sha256",             crypto_sha256),
        ("sha512",             crypto_sha512),
        ("hmac-sha256",        crypto_hmac_sha256),
        ("ed25519-keygen",     crypto_ed25519_keygen),
        ("ed25519-sign",       crypto_ed25519_sign),
        ("ed25519-verify",     crypto_ed25519_verify),
        ("chacha20-seal",      crypto_chacha20_seal),
        ("chacha20-open",      crypto_chacha20_open),
        ("x25519-keygen",      crypto_x25519_keygen),
        ("x25519-dh",          crypto_x25519_dh),
        ("base64-encode",      crypto_b64_encode),
        ("base64-decode",      crypto_b64_decode),
        // ── Proofs ────────────────────────────────────────────────────────────
        ("make-promise",          proof_make_promise),
        ("promise?",              proof_is_promise),
        ("promise-lang",          proof_promise_lang),
        ("promise-code",          proof_promise_code),
        ("promise-id",            proof_promise_id),
        ("promise-effect",        proof_promise_effect),
        ("promise-ast",           proof_promise_ast),
        ("make-bundle",           proof_make_bundle),
        ("bundle?",               proof_is_bundle),
        ("bundle-primary",        proof_bundle_primary),
        ("bundle-proofs",         proof_bundle_proofs),
        ("bundle-comments",       proof_bundle_comments),
        ("bundle-effects-agree?", proof_bundle_effects_agree),
        ("proof-normal-form",     proof_normal_form),
    ];

    for (name, f) in builtins {
        Env::define(env, name.to_string(), Val::Native(NativeFn { name, f: *f }));
    }
}

// ── Arithmetic ────────────────────────────────────────────────────────────────

fn arith_add(args: &[Val]) -> Result<Val> {
    let mut sum = 0i64;
    let mut has_float = false;
    let mut fsum = 0f64;
    for a in args {
        match a {
            Val::Int(n) => { sum += n; fsum += *n as f64; }
            Val::Float(f) => { has_float = true; fsum += f; }
            other => bail!("+ expects numbers, got {}", other.type_name()),
        }
    }
    if has_float { Ok(Val::Float(fsum)) } else { Ok(Val::Int(sum)) }
}

fn arith_sub(args: &[Val]) -> Result<Val> {
    if args.is_empty() { bail!("- requires at least 1 arg"); }
    if args.len() == 1 {
        return match &args[0] {
            Val::Int(n) => Ok(Val::Int(-n)),
            Val::Float(f) => Ok(Val::Float(-f)),
            other => bail!("- expects number, got {}", other.type_name()),
        };
    }
    let mut result = args[0].as_float()?;
    let is_float = matches!(&args[0], Val::Float(_));
    let mut all_int = !is_float;
    let mut iresult = args[0].as_int().unwrap_or(0);
    for a in &args[1..] {
        match a {
            Val::Int(n) => { result -= *n as f64; iresult -= n; }
            Val::Float(f) => { result -= f; all_int = false; }
            other => bail!("- expects numbers, got {}", other.type_name()),
        }
    }
    if all_int { Ok(Val::Int(iresult)) } else { Ok(Val::Float(result)) }
}

fn arith_mul(args: &[Val]) -> Result<Val> {
    let mut prod = 1i64;
    let mut fprod = 1f64;
    let mut has_float = false;
    for a in args {
        match a {
            Val::Int(n) => { prod = prod.saturating_mul(*n); fprod *= *n as f64; }
            Val::Float(f) => { has_float = true; fprod *= f; }
            other => bail!("* expects numbers, got {}", other.type_name()),
        }
    }
    if has_float { Ok(Val::Float(fprod)) } else { Ok(Val::Int(prod)) }
}

fn arith_div(args: &[Val]) -> Result<Val> {
    if args.len() < 2 { bail!("/ requires at least 2 args"); }
    let mut result = args[0].as_float()?;
    for a in &args[1..] {
        let d = a.as_float()?;
        if d == 0.0 { bail!("division by zero"); }
        result /= d;
    }
    // Return int if all args are int and result is whole
    if args.iter().all(|a| matches!(a, Val::Int(_))) && result.fract() == 0.0 {
        Ok(Val::Int(result as i64))
    } else {
        Ok(Val::Float(result))
    }
}

fn arith_mod(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("mod requires 2 args"); }
    let a = args[0].as_int()?;
    let b = args[1].as_int()?;
    if b == 0 { bail!("modulo by zero"); }
    Ok(Val::Int(a.rem_euclid(b)))
}

fn arith_quotient(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("quotient requires 2 args"); }
    let a = args[0].as_int()?;
    let b = args[1].as_int()?;
    if b == 0 { bail!("division by zero"); }
    Ok(Val::Int(a / b))
}

fn arith_remainder(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("remainder requires 2 args"); }
    let a = args[0].as_int()?;
    let b = args[1].as_int()?;
    if b == 0 { bail!("division by zero"); }
    Ok(Val::Int(a % b))
}

fn arith_abs(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("abs requires 1 arg"); }
    match &args[0] {
        Val::Int(n) => Ok(Val::Int(n.abs())),
        Val::Float(f) => Ok(Val::Float(f.abs())),
        other => bail!("abs expects number, got {}", other.type_name()),
    }
}

fn arith_max(args: &[Val]) -> Result<Val> {
    if args.is_empty() { bail!("max requires at least 1 arg"); }
    let mut m = args[0].as_float()?;
    for a in &args[1..] { m = m.max(a.as_float()?); }
    if args.iter().all(|a| matches!(a, Val::Int(_))) { Ok(Val::Int(m as i64)) } else { Ok(Val::Float(m)) }
}

fn arith_min(args: &[Val]) -> Result<Val> {
    if args.is_empty() { bail!("min requires at least 1 arg"); }
    let mut m = args[0].as_float()?;
    for a in &args[1..] { m = m.min(a.as_float()?); }
    if args.iter().all(|a| matches!(a, Val::Int(_))) { Ok(Val::Int(m as i64)) } else { Ok(Val::Float(m)) }
}

fn arith_floor(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("floor requires 1 arg"); }
    Ok(Val::Float(args[0].as_float()?.floor()))
}

fn arith_ceiling(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("ceiling requires 1 arg"); }
    Ok(Val::Float(args[0].as_float()?.ceil()))
}

fn arith_round(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("round requires 1 arg"); }
    Ok(Val::Float(args[0].as_float()?.round()))
}

fn arith_sqrt(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("sqrt requires 1 arg"); }
    Ok(Val::Float(args[0].as_float()?.sqrt()))
}

fn arith_expt(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("expt requires 2 args"); }
    let base = args[0].as_float()?;
    let exp = args[1].as_float()?;
    Ok(Val::Float(base.powf(exp)))
}

// ── Comparison ────────────────────────────────────────────────────────────────

fn cmp_eq(args: &[Val]) -> Result<Val> {
    if args.len() < 2 { bail!("= requires at least 2 args"); }
    for pair in args.windows(2) {
        if pair[0] != pair[1] { return Ok(Val::Bool(false)); }
    }
    Ok(Val::Bool(true))
}

fn cmp_lt(args: &[Val]) -> Result<Val> {
    if args.len() < 2 { bail!("< requires at least 2 args"); }
    for pair in args.windows(2) {
        if pair[0].as_float()? >= pair[1].as_float()? { return Ok(Val::Bool(false)); }
    }
    Ok(Val::Bool(true))
}

fn cmp_gt(args: &[Val]) -> Result<Val> {
    if args.len() < 2 { bail!("> requires at least 2 args"); }
    for pair in args.windows(2) {
        if pair[0].as_float()? <= pair[1].as_float()? { return Ok(Val::Bool(false)); }
    }
    Ok(Val::Bool(true))
}

fn cmp_le(args: &[Val]) -> Result<Val> {
    if args.len() < 2 { bail!("<= requires at least 2 args"); }
    for pair in args.windows(2) {
        if pair[0].as_float()? > pair[1].as_float()? { return Ok(Val::Bool(false)); }
    }
    Ok(Val::Bool(true))
}

fn cmp_ge(args: &[Val]) -> Result<Val> {
    if args.len() < 2 { bail!(">= requires at least 2 args"); }
    for pair in args.windows(2) {
        if pair[0].as_float()? < pair[1].as_float()? { return Ok(Val::Bool(false)); }
    }
    Ok(Val::Bool(true))
}

fn logic_not(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("not requires 1 arg"); }
    Ok(Val::Bool(!args[0].is_truthy()))
}

fn pred_zero(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("zero? requires 1 arg"); }
    Ok(Val::Bool(args[0].as_float()? == 0.0))
}

fn pred_positive(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("positive? requires 1 arg"); }
    Ok(Val::Bool(args[0].as_float()? > 0.0))
}

fn pred_negative(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("negative? requires 1 arg"); }
    Ok(Val::Bool(args[0].as_float()? < 0.0))
}

fn pred_even(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("even? requires 1 arg"); }
    Ok(Val::Bool(args[0].as_int()? % 2 == 0))
}

fn pred_odd(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("odd? requires 1 arg"); }
    Ok(Val::Bool(args[0].as_int()? % 2 != 0))
}

// ── Type predicates ───────────────────────────────────────────────────────────

fn pred_null(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Nil) | None)))
}

fn pred_pair(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::List(v)) if !v.is_empty())))
}

fn pred_list(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::List(_) | Val::Nil))))
}

fn pred_number(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Int(_) | Val::Float(_)))))
}

fn pred_integer(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Int(_)))))
}

fn pred_string(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Str(_)))))
}

fn pred_symbol(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Symbol(_)))))
}

fn pred_boolean(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Bool(_)))))
}

fn pred_bytes(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Bytes(_)))))
}

fn pred_procedure(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Lambda(_) | Val::Native(_)))))
}

fn pred_ssh(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::SshSession(_)))))
}

// ── List operations ───────────────────────────────────────────────────────────

fn list_cons(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("cons requires 2 args"); }
    let head = args[0].clone();
    match &args[1] {
        Val::List(tail) => {
            let mut v = vec![head];
            v.extend(tail.iter().cloned());
            Ok(Val::List(v))
        }
        Val::Nil => Ok(Val::List(vec![head])),
        other => Ok(Val::List(vec![head, other.clone()])), // improper pair
    }
}

fn list_car(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("car requires 1 arg"); }
    match &args[0] {
        Val::List(v) if !v.is_empty() => Ok(v[0].clone()),
        Val::List(_) | Val::Nil => bail!("car: empty list"),
        other => bail!("car expects list, got {}", other.type_name()),
    }
}

fn list_cdr(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("cdr requires 1 arg"); }
    match &args[0] {
        Val::List(v) if v.len() > 1 => Ok(Val::List(v[1..].to_vec())),
        Val::List(v) if v.len() == 1 => Ok(Val::Nil),
        Val::List(_) | Val::Nil => bail!("cdr: empty list"),
        other => bail!("cdr expects list, got {}", other.type_name()),
    }
}

fn list_cadr(args: &[Val]) -> Result<Val> {
    let cdr = list_cdr(args)?;
    list_car(&[cdr])
}

fn list_caddr(args: &[Val]) -> Result<Val> {
    let cdr = list_cdr(args)?;
    let cdr2 = list_cdr(&[cdr])?;
    list_car(&[cdr2])
}

fn list_list(args: &[Val]) -> Result<Val> {
    Ok(Val::List(args.to_vec()))
}

fn list_length(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("length requires 1 arg"); }
    match &args[0] {
        Val::List(v) => Ok(Val::Int(v.len() as i64)),
        Val::Nil => Ok(Val::Int(0)),
        other => bail!("length expects list, got {}", other.type_name()),
    }
}

fn list_append(args: &[Val]) -> Result<Val> {
    let mut out = Vec::new();
    for a in args {
        match a {
            Val::List(v) => out.extend(v.iter().cloned()),
            Val::Nil => {}
            other => bail!("append expects list, got {}", other.type_name()),
        }
    }
    Ok(if out.is_empty() { Val::Nil } else { Val::List(out) })
}

fn list_reverse(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("reverse requires 1 arg"); }
    let mut v = args[0].as_list()?.to_vec();
    v.reverse();
    Ok(if v.is_empty() { Val::Nil } else { Val::List(v) })
}

fn list_ref(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("list-ref requires 2 args"); }
    let list = args[0].as_list()?;
    let idx = args[1].as_int()? as usize;
    list.get(idx).cloned().ok_or_else(|| anyhow::anyhow!("list-ref: index {idx} out of bounds"))
}

fn list_tail(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("list-tail requires 2 args"); }
    let list = args[0].as_list()?;
    let idx = args[1].as_int()? as usize;
    if idx > list.len() { bail!("list-tail: index out of bounds"); }
    let v = list[idx..].to_vec();
    Ok(if v.is_empty() { Val::Nil } else { Val::List(v) })
}

fn list_map_stub(_args: &[Val]) -> Result<Val> {
    bail!("map is handled as a special form in eval")
}

fn list_for_each_stub(_args: &[Val]) -> Result<Val> {
    bail!("for-each is handled as a special form in eval")
}

fn list_assoc(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("assoc requires 2 args"); }
    let key = &args[0];
    for item in args[1].as_list()? {
        if let Val::List(pair) = item {
            if pair.first() == Some(key) {
                return Ok(item.clone());
            }
        }
    }
    Ok(Val::Bool(false))
}

fn list_member(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("member requires 2 args"); }
    let key = &args[0];
    let list = args[1].as_list()?;
    for (i, item) in list.iter().enumerate() {
        if item == key {
            return Ok(Val::List(list[i..].to_vec()));
        }
    }
    Ok(Val::Bool(false))
}

// ── String operations ─────────────────────────────────────────────────────────

fn str_append(args: &[Val]) -> Result<Val> {
    let mut out = String::new();
    for a in args {
        out.push_str(a.as_str()?);
    }
    Ok(Val::Str(out))
}

fn str_length(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string-length requires 1 arg"); }
    Ok(Val::Int(args[0].as_str()?.chars().count() as i64))
}

fn str_substring(args: &[Val]) -> Result<Val> {
    if args.len() < 2 || args.len() > 3 { bail!("substring requires 2-3 args"); }
    let s: Vec<char> = args[0].as_str()?.chars().collect();
    let start = args[1].as_int()? as usize;
    let end = if args.len() == 3 { args[2].as_int()? as usize } else { s.len() };
    if start > end || end > s.len() { bail!("substring: indices out of range"); }
    Ok(Val::Str(s[start..end].iter().collect()))
}

fn str_to_number(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string->number requires 1 arg"); }
    let s = args[0].as_str()?;
    if let Ok(n) = s.parse::<i64>() { return Ok(Val::Int(n)); }
    if let Ok(f) = s.parse::<f64>() { return Ok(Val::Float(f)); }
    Ok(Val::Bool(false))
}

fn num_to_string(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("number->string requires 1 arg"); }
    Ok(Val::Str(args[0].to_string()))
}

fn str_upcase(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string-upcase requires 1 arg"); }
    Ok(Val::Str(args[0].as_str()?.to_uppercase()))
}

fn str_downcase(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string-downcase requires 1 arg"); }
    Ok(Val::Str(args[0].as_str()?.to_lowercase()))
}

fn str_contains(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("string-contains requires 2 args"); }
    Ok(Val::Bool(args[0].as_str()?.contains(args[1].as_str()?)))
}

fn str_split(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("string-split requires 2 args"); }
    let s = args[0].as_str()?;
    let delim = args[1].as_str()?;
    let parts: Vec<Val> = s.split(delim).map(|p| Val::Str(p.to_string())).collect();
    Ok(Val::List(parts))
}

fn str_trim(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string-trim requires 1 arg"); }
    Ok(Val::Str(args[0].as_str()?.trim().to_string()))
}

fn str_to_symbol(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string->symbol requires 1 arg"); }
    Ok(Val::Symbol(args[0].as_str()?.to_string()))
}

fn symbol_to_str(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("symbol->string requires 1 arg"); }
    match &args[0] {
        Val::Symbol(s) => Ok(Val::Str(s.clone())),
        other => bail!("symbol->string expects symbol, got {}", other.type_name()),
    }
}

fn str_to_list(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string->list requires 1 arg"); }
    let chars: Vec<Val> = args[0].as_str()?.chars()
        .map(|c| Val::Str(c.to_string()))
        .collect();
    Ok(Val::List(chars))
}

fn list_to_str(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("list->string requires 1 arg"); }
    let mut s = String::new();
    for c in args[0].as_list()? {
        s.push_str(c.as_str()?);
    }
    Ok(Val::Str(s))
}

// ── Bytes operations ──────────────────────────────────────────────────────────

fn str_to_bytes(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("string->bytes requires 1 arg"); }
    Ok(Val::Bytes(args[0].as_str()?.as_bytes().to_vec()))
}

fn bytes_to_str(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bytes->string requires 1 arg"); }
    let s = String::from_utf8_lossy(args[0].as_bytes()?).into_owned();
    Ok(Val::Str(s))
}

fn bytes_to_hex(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bytes->hex requires 1 arg"); }
    Ok(Val::Str(hex::encode(args[0].as_bytes()?)))
}

fn hex_to_bytes(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("hex->bytes requires 1 arg"); }
    let bytes = hex::decode(args[0].as_str()?)
        .map_err(|e| anyhow::anyhow!("hex->bytes: {e}"))?;
    Ok(Val::Bytes(bytes))
}

fn bytes_length(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bytes-length requires 1 arg"); }
    Ok(Val::Int(args[0].as_bytes()?.len() as i64))
}

fn bytes_ref(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("bytes-ref requires 2 args"); }
    let b = args[0].as_bytes()?;
    let i = args[1].as_int()? as usize;
    b.get(i).map(|&v| Val::Int(v as i64))
        .ok_or_else(|| anyhow::anyhow!("bytes-ref: index out of bounds"))
}

fn bytes_append(args: &[Val]) -> Result<Val> {
    let mut out = Vec::new();
    for a in args {
        out.extend_from_slice(a.as_bytes()?);
    }
    Ok(Val::Bytes(out))
}

fn make_bytes(args: &[Val]) -> Result<Val> {
    let out: Result<Vec<u8>> = args.iter().map(|a| Ok(a.as_int()? as u8)).collect();
    Ok(Val::Bytes(out?))
}

fn random_bytes(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("random-bytes requires 1 arg (length)"); }
    let n = args[0].as_int()? as usize;
    let mut buf = vec![0u8; n];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut buf);
    Ok(Val::Bytes(buf))
}

fn subbytes(args: &[Val]) -> Result<Val> {
    if args.len() < 2 || args.len() > 3 { bail!("subbytes requires 2-3 args"); }
    let b = args[0].as_bytes()?;
    let start = args[1].as_int()? as usize;
    let end = if args.len() == 3 { args[2].as_int()? as usize } else { b.len() };
    if start > end || end > b.len() { bail!("subbytes: indices out of range"); }
    Ok(Val::Bytes(b[start..end].to_vec()))
}

// ── I/O ───────────────────────────────────────────────────────────────────────

fn io_display(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("display requires 1 arg"); }
    print!("{}", args[0]);
    Ok(Val::Nil)
}

fn io_newline(_args: &[Val]) -> Result<Val> {
    println!();
    Ok(Val::Nil)
}

fn io_error(args: &[Val]) -> Result<Val> {
    let msg = args.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(" ");
    bail!("{msg}")
}

fn io_format_stub(_args: &[Val]) -> Result<Val> {
    bail!("format is handled as a special form in eval")
}

// ── Crypto ────────────────────────────────────────────────────────────────────

fn crypto_sha256(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("sha256 requires 1 arg"); }
    use sha2::{Sha256, Digest};
    let hash = Sha256::digest(args[0].as_bytes()?);
    Ok(Val::Bytes(hash.to_vec()))
}

fn crypto_sha512(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("sha512 requires 1 arg"); }
    use sha2::{Sha512, Digest};
    let hash = Sha512::digest(args[0].as_bytes()?);
    Ok(Val::Bytes(hash.to_vec()))
}

fn crypto_hmac_sha256(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("hmac-sha256 requires 2 args: (key message)"); }
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(args[0].as_bytes()?)
        .map_err(|e| anyhow::anyhow!("hmac-sha256: {e}"))?;
    mac.update(args[1].as_bytes()?);
    Ok(Val::Bytes(mac.finalize().into_bytes().to_vec()))
}

fn crypto_ed25519_keygen(args: &[Val]) -> Result<Val> {
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;
    let seed: [u8; 32] = if args.is_empty() {
        let mut b = [0u8; 32];
        use rand::RngCore;
        OsRng.fill_bytes(&mut b);
        b
    } else {
        let s = args[0].as_bytes()?;
        if s.len() != 32 { bail!("ed25519-keygen: seed must be 32 bytes"); }
        s.try_into().unwrap()
    };
    let sk = SigningKey::from_bytes(&seed);
    let vk = sk.verifying_key();
    Ok(Val::List(vec![
        Val::Bytes(sk.to_bytes().to_vec()),
        Val::Bytes(vk.to_bytes().to_vec()),
    ]))
}

fn crypto_ed25519_sign(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("ed25519-sign requires 2 args: (private-key message)"); }
    use ed25519_dalek::{SigningKey, Signer};
    let key_bytes: [u8; 32] = args[0].as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519-sign: private key must be 32 bytes"))?;
    let sk = SigningKey::from_bytes(&key_bytes);
    let sig = sk.sign(args[1].as_bytes()?);
    Ok(Val::Bytes(sig.to_bytes().to_vec()))
}

fn crypto_ed25519_verify(args: &[Val]) -> Result<Val> {
    if args.len() != 3 { bail!("ed25519-verify requires 3 args: (public-key message signature)"); }
    use ed25519_dalek::{VerifyingKey, Verifier, Signature};
    let pk_bytes: [u8; 32] = args[0].as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519-verify: public key must be 32 bytes"))?;
    let pk = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| anyhow::anyhow!("ed25519-verify: bad public key: {e}"))?;
    let sig_bytes: [u8; 64] = args[2].as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519-verify: signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&sig_bytes);
    Ok(Val::Bool(pk.verify(args[1].as_bytes()?, &sig).is_ok()))
}

fn crypto_chacha20_seal(args: &[Val]) -> Result<Val> {
    if args.len() != 3 { bail!("chacha20-seal requires 3 args: (key nonce plaintext)"); }
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::{Aead, generic_array::GenericArray}};
    let key = args[0].as_bytes()?;
    let nonce_bytes = args[1].as_bytes()?;
    if key.len() != 32 { bail!("chacha20-seal: key must be 32 bytes"); }
    if nonce_bytes.len() != 12 { bail!("chacha20-seal: nonce must be 12 bytes"); }
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce = GenericArray::from_slice(nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, args[2].as_bytes()?)
        .map_err(|e| anyhow::anyhow!("chacha20-seal: {e}"))?;
    Ok(Val::Bytes(ciphertext))
}

fn crypto_chacha20_open(args: &[Val]) -> Result<Val> {
    if args.len() != 3 { bail!("chacha20-open requires 3 args: (key nonce ciphertext)"); }
    use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::{Aead, generic_array::GenericArray}};
    let key = args[0].as_bytes()?;
    let nonce_bytes = args[1].as_bytes()?;
    if key.len() != 32 { bail!("chacha20-open: key must be 32 bytes"); }
    if nonce_bytes.len() != 12 { bail!("chacha20-open: nonce must be 12 bytes"); }
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce = GenericArray::from_slice(nonce_bytes);
    let plaintext = cipher.decrypt(nonce, args[2].as_bytes()?)
        .map_err(|e| anyhow::anyhow!("chacha20-open: decryption failed: {e}"))?;
    Ok(Val::Bytes(plaintext))
}

fn crypto_x25519_keygen(_args: &[Val]) -> Result<Val> {
    use x25519_dalek::{StaticSecret, PublicKey};
    use rand::rngs::OsRng;
    let sk = StaticSecret::random_from_rng(OsRng);
    let pk = PublicKey::from(&sk);
    Ok(Val::List(vec![
        Val::Bytes(sk.to_bytes().to_vec()),
        Val::Bytes(pk.to_bytes().to_vec()),
    ]))
}

fn crypto_x25519_dh(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("x25519-dh requires 2 args: (my-private-key their-public-key)"); }
    use x25519_dalek::{StaticSecret, PublicKey};
    let sk_bytes: [u8; 32] = args[0].as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("x25519-dh: private key must be 32 bytes"))?;
    let pk_bytes: [u8; 32] = args[1].as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("x25519-dh: public key must be 32 bytes"))?;
    let sk = StaticSecret::from(sk_bytes);
    let pk = PublicKey::from(pk_bytes);
    let shared = sk.diffie_hellman(&pk);
    Ok(Val::Bytes(shared.to_bytes().to_vec()))
}

fn crypto_b64_encode(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("base64-encode requires 1 arg"); }
    use base64::Engine;
    Ok(Val::Str(base64::engine::general_purpose::STANDARD.encode(args[0].as_bytes()?)))
}

fn crypto_b64_decode(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("base64-decode requires 1 arg"); }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(args[0].as_str()?)
        .map_err(|e| anyhow::anyhow!("base64-decode: {e}"))?;
    Ok(Val::Bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::env::Env;

    fn make_env() -> EnvRef {
        let env = Env::new_root();
        register_all(&env);
        env
    }

    fn get_fn(env: &EnvRef, name: &str) -> NativeFn {
        match Env::get(env, name).unwrap() {
            Val::Native(f) => f,
            _ => panic!("{name} is not a native fn"),
        }
    }

    #[test]
    fn test_stdlib_add() {
        let env = make_env();
        let f = get_fn(&env, "+");
        assert_eq!((f.f)(&[Val::Int(1), Val::Int(2)]).unwrap(), Val::Int(3));
    }

    #[test]
    fn test_stdlib_sub_negate() {
        let env = make_env();
        let f = get_fn(&env, "-");
        assert_eq!((f.f)(&[Val::Int(5)]).unwrap(), Val::Int(-5));
    }

    #[test]
    fn test_stdlib_mod() {
        let env = make_env();
        let f = get_fn(&env, "mod");
        assert_eq!((f.f)(&[Val::Int(10), Val::Int(3)]).unwrap(), Val::Int(1));
    }

    #[test]
    fn test_stdlib_sha256_produces_32_bytes() {
        let env = make_env();
        let f = get_fn(&env, "sha256");
        let result = (f.f)(&[Val::Bytes(b"hello".to_vec())]).unwrap();
        if let Val::Bytes(b) = result { assert_eq!(b.len(), 32); } else { panic!(); }
    }

    #[test]
    fn test_stdlib_ed25519_roundtrip() {
        let env = make_env();
        let keygen = get_fn(&env, "ed25519-keygen");
        let sign = get_fn(&env, "ed25519-sign");
        let verify = get_fn(&env, "ed25519-verify");

        let keypair = (keygen.f)(&[]).unwrap();
        let keys = keypair.as_list().unwrap().to_vec();
        let msg = Val::Bytes(b"hello world".to_vec());
        let sig = (sign.f)(&[keys[0].clone(), msg.clone()]).unwrap();
        let ok = (verify.f)(&[keys[1].clone(), msg, sig]).unwrap();
        assert_eq!(ok, Val::Bool(true));
    }

    #[test]
    fn test_stdlib_chacha20_roundtrip() {
        let env = make_env();
        let seal = get_fn(&env, "chacha20-seal");
        let open = get_fn(&env, "chacha20-open");
        let key = Val::Bytes(vec![0u8; 32]);
        let nonce = Val::Bytes(vec![0u8; 12]);
        let pt = Val::Bytes(b"secret message".to_vec());
        let ct = (seal.f)(&[key.clone(), nonce.clone(), pt.clone()]).unwrap();
        let recovered = (open.f)(&[key, nonce, ct]).unwrap();
        assert_eq!(recovered, pt);
    }

    #[test]
    fn test_stdlib_x25519_dh_shared_secret() {
        let env = make_env();
        let keygen = get_fn(&env, "x25519-keygen");
        let dh = get_fn(&env, "x25519-dh");
        let alice = (keygen.f)(&[]).unwrap().as_list().unwrap().to_vec();
        let bob = (keygen.f)(&[]).unwrap().as_list().unwrap().to_vec();
        // alice private, bob public
        let shared_ab = (dh.f)(&[alice[0].clone(), bob[1].clone()]).unwrap();
        // bob private, alice public
        let shared_ba = (dh.f)(&[bob[0].clone(), alice[1].clone()]).unwrap();
        assert_eq!(shared_ab, shared_ba);
    }

    #[test]
    fn test_stdlib_bytes_hex_roundtrip() {
        let env = make_env();
        let b2h = get_fn(&env, "bytes->hex");
        let h2b = get_fn(&env, "hex->bytes");
        let original = Val::Bytes(vec![0xca, 0xfe, 0xba, 0xbe]);
        let hex_val = (b2h.f)(&[original.clone()]).unwrap();
        let back = (h2b.f)(&[hex_val]).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn test_stdlib_cons_car_cdr() {
        let env = make_env();
        let cons = get_fn(&env, "cons");
        let car = get_fn(&env, "car");
        let cdr = get_fn(&env, "cdr");
        let list = (cons.f)(&[Val::Int(1), Val::List(vec![Val::Int(2), Val::Int(3)])]).unwrap();
        assert_eq!((car.f)(&[list.clone()]).unwrap(), Val::Int(1));
        let tail = (cdr.f)(&[list]).unwrap();
        assert_eq!(tail, Val::List(vec![Val::Int(2), Val::Int(3)]));
    }
}

// ── Proofs ────────────────────────────────────────────────────────────────────
//
// Promises and ProofBundles are represented as tagged Lisp lists so they are
// fully inspectable, serializable, and composable in the REPL without needing
// new Val variants.
//
// Promise wire format:  (promise <lang> <code> <id> <hash> [<effect>])
//   lang   — symbol: forth | lisp | natural
//   code   — string: the source text
//   id     — string: UUID
//   hash   — string: hex digest of code
//   effect — optional (pops pushes) list (Forth only)
//
// Bundle wire format:   (bundle <id> <comments> <proofs>)
//   id       — string: UUID
//   comments — list of strings
//   proofs   — list of promise lists

fn sym(s: &'static str) -> Val {
    Val::Symbol(s.to_string())
}

fn make_promise_val(
    lang: &str,
    code: &str,
    effect: Option<(i64, i64)>,
) -> Val {
    use uuid::Uuid;
    let id = Uuid::new_v4().to_string();
    // Fast non-crypto integrity hash (same approach as Promise::sha256).
    let hash = {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash_slice(code.as_bytes(), &mut h);
        format!("{:016x}", h.finish())
    };
    let mut fields = vec![
        sym("promise"),
        Val::Symbol(lang.to_string()),
        Val::Str(code.to_string()),
        Val::Str(id),
        Val::Str(hash),
    ];
    if let Some((pops, pushes)) = effect {
        fields.push(Val::List(vec![Val::Int(pops), Val::Int(pushes)]));
    }
    Val::List(fields)
}

fn promise_fields(v: &Val) -> Result<&[Val]> {
    match v {
        Val::List(parts)
            if parts.len() >= 5
                && parts[0] == sym("promise") =>
        {
            Ok(parts)
        }
        _ => bail!("not a promise"),
    }
}

fn bundle_fields(v: &Val) -> Result<&[Val]> {
    match v {
        Val::List(parts)
            if parts.len() == 4
                && parts[0] == sym("bundle") =>
        {
            Ok(parts)
        }
        _ => bail!("not a bundle"),
    }
}

// (make-promise lang code)          → promise (no stack effect)
// (make-promise lang code pops pushes) → promise with effect
fn proof_make_promise(args: &[Val]) -> Result<Val> {
    if args.len() != 2 && args.len() != 4 {
        bail!("make-promise: requires 2 or 4 args (lang code [pops pushes])");
    }
    let lang = match &args[0] {
        Val::Symbol(s) => s.as_str(),
        _ => bail!("make-promise: lang must be a symbol"),
    };
    if !matches!(lang, "forth" | "lisp" | "natural") {
        bail!("make-promise: lang must be forth, lisp, or natural");
    }
    let code = args[1].as_str()?;
    let effect = if args.len() == 4 {
        Some((args[2].as_int()?, args[3].as_int()?))
    } else {
        None
    };
    Ok(make_promise_val(lang, code, effect))
}

fn proof_is_promise(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("promise?: requires 1 arg"); }
    Ok(Val::Bool(promise_fields(&args[0]).is_ok()))
}

fn proof_promise_lang(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("promise-lang: requires 1 arg"); }
    let parts = promise_fields(&args[0])?;
    Ok(parts[1].clone())
}

fn proof_promise_code(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("promise-code: requires 1 arg"); }
    let parts = promise_fields(&args[0])?;
    Ok(parts[2].clone())
}

fn proof_promise_id(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("promise-id: requires 1 arg"); }
    let parts = promise_fields(&args[0])?;
    Ok(parts[3].clone())
}

fn proof_promise_effect(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("promise-effect: requires 1 arg"); }
    let parts = promise_fields(&args[0])?;
    Ok(parts.get(5).cloned().unwrap_or(Val::Nil))
}

/// Parse the code of a lisp promise back into a live Val tree.
///
/// This is the "open" operation — it takes the sealed code string and
/// returns the actual AST so type annotations, structure, and subterms
/// are all accessible.
fn proof_promise_ast(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("promise-ast: requires 1 arg"); }
    let parts = promise_fields(&args[0])?;
    if parts[1] != sym("lisp") {
        bail!("promise-ast: only lisp promises have an AST (got {})", parts[1]);
    }
    let code = parts[2].as_str()?;
    let exprs = crate::lisp::reader::parse_str(code)?;
    Ok(match exprs.len() {
        0 => Val::Nil,
        1 => exprs.into_iter().next().unwrap(),
        _ => Val::List(exprs),
    })
}

// (make-bundle promise comments) → bundle
// comments is a list of strings or nil
fn proof_make_bundle(args: &[Val]) -> Result<Val> {
    if args.len() != 2 { bail!("make-bundle: requires 2 args (promise comments)"); }
    let _ = promise_fields(&args[0])?; // validate it's a promise
    let id = match &args[0] {
        Val::List(p) => p[3].clone(), // reuse promise id as bundle id
        _ => unreachable!(),
    };
    let comments = match &args[1] {
        Val::List(_) | Val::Nil => args[1].clone(),
        _ => bail!("make-bundle: comments must be a list"),
    };
    Ok(Val::List(vec![
        sym("bundle"),
        id,
        comments,
        Val::List(vec![args[0].clone()]),
    ]))
}

fn proof_is_bundle(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bundle?: requires 1 arg"); }
    Ok(Val::Bool(bundle_fields(&args[0]).is_ok()))
}

fn proof_bundle_primary(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bundle-primary: requires 1 arg"); }
    let parts = bundle_fields(&args[0])?;
    match &parts[3] {
        Val::List(proofs) if !proofs.is_empty() => Ok(proofs[0].clone()),
        _ => bail!("bundle-primary: bundle has no proofs"),
    }
}

fn proof_bundle_proofs(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bundle-proofs: requires 1 arg"); }
    let parts = bundle_fields(&args[0])?;
    Ok(parts[3].clone())
}

fn proof_bundle_comments(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bundle-comments: requires 1 arg"); }
    let parts = bundle_fields(&args[0])?;
    Ok(parts[2].clone())
}

/// The normal form of a Lisp expression is the canonical `repr()` of its AST.
///
/// Two source strings that parse to structurally identical ASTs have the same
/// normal form — whitespace, comments, and formatting are erased.  This means
/// `(+ 1  2)` and `(+  1 2)` are the same proof; alpha-equivalent expressions
/// are NOT collapsed (that requires evaluation).
///
/// `(proof-normal-form promise-or-str)` → string
fn proof_normal_form(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("proof-normal-form: requires 1 arg");
    }
    let code = match &args[0] {
        Val::Str(s) => s.as_str(),
        other => {
            // Accept a lisp promise too.
            if let Ok(parts) = promise_fields(other) {
                if parts[1] != sym("lisp") {
                    bail!("proof-normal-form: only lisp promises have a normal form");
                }
                return proof_normal_form(&[parts[2].clone()]);
            }
            bail!("proof-normal-form: expected string or lisp promise");
        }
    };
    let exprs = crate::lisp::reader::parse_str(code)?;
    let norm = match exprs.len() {
        0 => Val::Nil,
        1 => exprs.into_iter().next().unwrap(),
        _ => Val::List(exprs),
    };
    Ok(Val::Str(norm.repr()))
}

/// True if all proofs that have effects agree on (pops pushes).
fn proof_bundle_effects_agree(args: &[Val]) -> Result<Val> {
    if args.len() != 1 { bail!("bundle-effects-agree?: requires 1 arg"); }
    let parts = bundle_fields(&args[0])?;
    let effects: Vec<(i64, i64)> = match &parts[3] {
        Val::List(proofs) => proofs
            .iter()
            .filter_map(|p| {
                let fields = promise_fields(p).ok()?;
                let eff = fields.get(5)?;
                match eff {
                    Val::List(v) if v.len() == 2 => {
                        Some((v[0].as_int().ok()?, v[1].as_int().ok()?))
                    }
                    _ => None,
                }
            })
            .collect(),
        _ => return Ok(Val::Bool(true)),
    };
    if effects.len() < 2 {
        return Ok(Val::Bool(true));
    }
    let first = effects[0];
    Ok(Val::Bool(effects.iter().all(|e| *e == first)))
}
