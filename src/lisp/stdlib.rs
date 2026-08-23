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
        ("+", arith_add),
        ("-", arith_sub),
        ("*", arith_mul),
        ("/", arith_div),
        ("mod", arith_mod),
        ("quotient", arith_quotient),
        ("remainder", arith_remainder),
        ("abs", arith_abs),
        ("max", arith_max),
        ("min", arith_min),
        ("floor", arith_floor),
        ("ceiling", arith_ceiling),
        ("round", arith_round),
        ("sqrt", arith_sqrt),
        ("expt", arith_expt),
        // ── Comparison ────────────────────────────────────────────────────────
        ("=", cmp_eq),
        ("<", cmp_lt),
        (">", cmp_gt),
        ("<=", cmp_le),
        (">=", cmp_ge),
        ("not", logic_not),
        // structural equality — works on any Val (booleans, strings, lists, …)
        ("equal?", val_equal),
        ("eqv?", val_equal),
        ("is", val_equal),
        ("is-not", val_not_equal),
        ("not-equal?", val_not_equal),
        // numeric coercion — every value has a numeric representation
        ("->number", val_to_number),
        ("count", val_count),
        ("zero?", pred_zero),
        ("positive?", pred_positive),
        ("negative?", pred_negative),
        ("even?", pred_even),
        ("odd?", pred_odd),
        // ── Type predicates ───────────────────────────────────────────────────
        ("null?", pred_null),
        ("pair?", pred_pair),
        ("list?", pred_list),
        ("number?", pred_number),
        ("integer?", pred_integer),
        ("string?", pred_string),
        ("symbol?", pred_symbol),
        ("boolean?", pred_boolean),
        ("bytes?", pred_bytes),
        ("procedure?", pred_procedure),
        ("ssh-session?", pred_ssh),
        // ── List operations ───────────────────────────────────────────────────
        ("cons", list_cons),
        ("car", list_car),
        ("cdr", list_cdr),
        ("cadr", list_cadr),
        ("caddr", list_caddr),
        ("list", list_list),
        ("length", list_length),
        ("append", list_append),
        ("reverse", list_reverse),
        ("list-ref", list_ref),
        ("list-tail", list_tail),
        ("map", list_map_stub), // full map needs apply — handled in eval
        ("filter", list_filter_stub),
        ("assoc", list_assoc),
        ("member", list_member),
        ("for-each", list_for_each_stub),
        // ── String operations ─────────────────────────────────────────────────
        ("string-append", str_append),
        ("string-length", str_length),
        ("substring", str_substring),
        ("string->number", str_to_number),
        ("number->string", num_to_string),
        ("string-upcase", str_upcase),
        ("string-downcase", str_downcase),
        ("string-contains", str_contains),
        ("string-split", str_split),
        ("string-trim", str_trim),
        ("string->symbol", str_to_symbol),
        ("symbol->string", symbol_to_str),
        ("string->list", str_to_list),
        ("list->string", list_to_str),
        // ── Bytes operations ──────────────────────────────────────────────────
        ("string->bytes", str_to_bytes),
        ("bytes->string", bytes_to_str),
        ("bytes->hex", bytes_to_hex),
        ("hex->bytes", hex_to_bytes),
        ("bytes-length", bytes_length),
        ("bytes-ref", bytes_ref),
        ("bytes-append", bytes_append),
        ("bytes", make_bytes),
        ("random-bytes", random_bytes),
        ("subbytes", subbytes),
        // ── I/O ───────────────────────────────────────────────────────────────
        ("display", io_display),
        ("newline", io_newline),
        ("error", io_error),
        ("format", io_format_stub),
        // ── Crypto ───────────────────────────────────────────────────────────
        ("sha256", crypto_sha256),
        ("sha512", crypto_sha512),
        ("hmac-sha256", crypto_hmac_sha256),
        ("ed25519-keygen", crypto_ed25519_keygen),
        ("ed25519-sign", crypto_ed25519_sign),
        ("ed25519-verify", crypto_ed25519_verify),
        ("chacha20-seal", crypto_chacha20_seal),
        ("chacha20-open", crypto_chacha20_open),
        ("x25519-keygen", crypto_x25519_keygen),
        ("x25519-dh", crypto_x25519_dh),
        ("base64-encode", crypto_b64_encode),
        ("base64-decode", crypto_b64_decode),
        // ── Proofs ────────────────────────────────────────────────────────────
        ("make-promise", proof_make_promise),
        ("promise?", proof_is_promise),
        ("promise-lang", proof_promise_lang),
        ("promise-code", proof_promise_code),
        ("promise-id", proof_promise_id),
        ("promise-effect", proof_promise_effect),
        ("promise-ast", proof_promise_ast),
        ("make-bundle", proof_make_bundle),
        ("bundle?", proof_is_bundle),
        ("bundle-primary", proof_bundle_primary),
        ("bundle-proofs", proof_bundle_proofs),
        ("bundle-comments", proof_bundle_comments),
        ("bundle-effects-agree?", proof_bundle_effects_agree),
        ("proof-normal-form", proof_normal_form),
        // ── System ────────────────────────────────────────────────────────────
        ("processes", sys_processes),
        ("process-info", sys_process_info),
        // ── Argumentation (Dung-style abstract argumentation frameworks) ──────
        // An argument is: (argument id claim)
        // A framework is: (framework args attacks)  where attacks = ((a b) ...)
        // meaning "a attacks b"
        ("make-argument", arg_make),
        ("argument?", arg_is),
        ("argument-id", arg_id),
        ("argument-claim", arg_claim),
        ("make-framework", arg_make_framework),
        ("framework-arguments", arg_framework_args),
        ("framework-attacks", arg_framework_attacks),
        ("add-attack", arg_add_attack),
        ("attacks?", arg_attacks_q),
        ("defended?", arg_defended_q),
        ("grounded-extension", arg_grounded_ext),
        ("acceptable?", arg_acceptable_q),
        // ── JSON ─────────────────────────────────────────────────────────────
        ("json-parse", json_parse),
        ("json-stringify", json_stringify),
        // ── YAML ─────────────────────────────────────────────────────────────
        ("yaml-parse", yaml_parse),
        ("yaml-stringify", yaml_stringify),
        // ── TOML ─────────────────────────────────────────────────────────────
        ("toml-parse", toml_parse),
        ("toml-stringify", toml_stringify),
        // ── Lisp reader ──────────────────────────────────────────────────────
        // lisp-parse: string → list of AST nodes (no eval)
        // lisp-eval:  special form in eval.rs (needs async)
        ("lisp-parse", lisp_parse),
        // ── String lines ─────────────────────────────────────────────────────
        ("lines", str_lines),
        ("unlines", str_unlines),
        ("words", str_words),
        ("unwords", str_unwords),
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
            Val::Int(n) => {
                sum += n;
                fsum += *n as f64;
            }
            Val::Float(f) => {
                has_float = true;
                fsum += f;
            }
            other => bail!("+ expects numbers, got {}", other.type_name()),
        }
    }
    if has_float {
        Ok(Val::Float(fsum))
    } else {
        Ok(Val::Int(sum))
    }
}

fn arith_sub(args: &[Val]) -> Result<Val> {
    if args.is_empty() {
        bail!("- requires at least 1 arg");
    }
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
            Val::Int(n) => {
                result -= *n as f64;
                iresult -= n;
            }
            Val::Float(f) => {
                result -= f;
                all_int = false;
            }
            other => bail!("- expects numbers, got {}", other.type_name()),
        }
    }
    if all_int {
        Ok(Val::Int(iresult))
    } else {
        Ok(Val::Float(result))
    }
}

fn arith_mul(args: &[Val]) -> Result<Val> {
    let mut prod = 1i64;
    let mut fprod = 1f64;
    let mut has_float = false;
    for a in args {
        match a {
            Val::Int(n) => {
                prod = prod.saturating_mul(*n);
                fprod *= *n as f64;
            }
            Val::Float(f) => {
                has_float = true;
                fprod *= f;
            }
            other => bail!("* expects numbers, got {}", other.type_name()),
        }
    }
    if has_float {
        Ok(Val::Float(fprod))
    } else {
        Ok(Val::Int(prod))
    }
}

fn arith_div(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("/ requires at least 2 args");
    }
    let mut result = args[0].as_float()?;
    for a in &args[1..] {
        let d = a.as_float()?;
        if d == 0.0 {
            bail!("division by zero");
        }
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
    if args.len() != 2 {
        bail!("mod requires 2 args");
    }
    let a = args[0].as_int()?;
    let b = args[1].as_int()?;
    if b == 0 {
        bail!("modulo by zero");
    }
    Ok(Val::Int(a.rem_euclid(b)))
}

fn arith_quotient(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("quotient requires 2 args");
    }
    let a = args[0].as_int()?;
    let b = args[1].as_int()?;
    if b == 0 {
        bail!("division by zero");
    }
    Ok(Val::Int(a / b))
}

fn arith_remainder(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("remainder requires 2 args");
    }
    let a = args[0].as_int()?;
    let b = args[1].as_int()?;
    if b == 0 {
        bail!("division by zero");
    }
    Ok(Val::Int(a % b))
}

fn arith_abs(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("abs requires 1 arg");
    }
    match &args[0] {
        Val::Int(n) => Ok(Val::Int(n.abs())),
        Val::Float(f) => Ok(Val::Float(f.abs())),
        other => bail!("abs expects number, got {}", other.type_name()),
    }
}

fn arith_max(args: &[Val]) -> Result<Val> {
    if args.is_empty() {
        bail!("max requires at least 1 arg");
    }
    let mut m = args[0].as_float()?;
    for a in &args[1..] {
        m = m.max(a.as_float()?);
    }
    if args.iter().all(|a| matches!(a, Val::Int(_))) {
        Ok(Val::Int(m as i64))
    } else {
        Ok(Val::Float(m))
    }
}

fn arith_min(args: &[Val]) -> Result<Val> {
    if args.is_empty() {
        bail!("min requires at least 1 arg");
    }
    let mut m = args[0].as_float()?;
    for a in &args[1..] {
        m = m.min(a.as_float()?);
    }
    if args.iter().all(|a| matches!(a, Val::Int(_))) {
        Ok(Val::Int(m as i64))
    } else {
        Ok(Val::Float(m))
    }
}

fn arith_floor(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("floor requires 1 arg");
    }
    Ok(Val::Float(args[0].as_float()?.floor()))
}

fn arith_ceiling(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("ceiling requires 1 arg");
    }
    Ok(Val::Float(args[0].as_float()?.ceil()))
}

fn arith_round(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("round requires 1 arg");
    }
    Ok(Val::Float(args[0].as_float()?.round()))
}

fn arith_sqrt(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("sqrt requires 1 arg");
    }
    Ok(Val::Float(args[0].as_float()?.sqrt()))
}

fn arith_expt(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("expt requires 2 args");
    }
    let base = args[0].as_float()?;
    let exp = args[1].as_float()?;
    Ok(Val::Float(base.powf(exp)))
}

// ── Comparison ────────────────────────────────────────────────────────────────

fn cmp_eq(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("= requires at least 2 args");
    }
    for pair in args.windows(2) {
        if pair[0] != pair[1] {
            return Ok(Val::Bool(false));
        }
    }
    Ok(Val::Bool(true))
}

fn val_equal(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("equal? requires at least 2 args");
    }
    for pair in args.windows(2) {
        if pair[0] != pair[1] {
            return Ok(Val::Bool(false));
        }
    }
    Ok(Val::Bool(true))
}

fn val_not_equal(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("is-not requires at least 2 args");
    }
    Ok(Val::Bool(args[0] != args[1]))
}

/// Coerce any value to its canonical numeric representation.
///
/// - Int / Float  → itself
/// - Bool         → 1 (true) or 0 (false)
/// - Nil          → 0
/// - Str          → parse as number, or fall back to char count
/// - Bytes        → byte length
/// - List         → number of elements (key count for JSON objects)
fn val_to_number(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("->number requires 1 arg");
    }
    Ok(match &args[0] {
        Val::Int(n) => Val::Int(*n),
        Val::Float(f) => Val::Float(*f),
        Val::Bool(b) => Val::Int(if *b { 1 } else { 0 }),
        Val::Nil => Val::Int(0),
        Val::Str(s) => {
            if let Ok(n) = s.parse::<i64>() {
                Val::Int(n)
            } else if let Ok(f) = s.parse::<f64>() {
                Val::Float(f)
            } else {
                Val::Int(s.chars().count() as i64)
            }
        }
        Val::Bytes(b) => Val::Int(b.len() as i64),
        Val::List(v) => Val::Int(v.len() as i64),
        other => bail!("->number: cannot coerce {} to number", other.type_name()),
    })
}

/// Count elements: string → char count; list/object → element count; nil → 0.
fn val_count(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("count requires 1 arg");
    }
    Ok(match &args[0] {
        Val::List(v) => Val::Int(v.len() as i64),
        Val::Nil => Val::Int(0),
        Val::Str(s) => Val::Int(s.chars().count() as i64),
        Val::Bytes(b) => Val::Int(b.len() as i64),
        other => bail!("count: not a collection ({})", other.type_name()),
    })
}

fn cmp_lt(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("< requires at least 2 args");
    }
    for pair in args.windows(2) {
        if pair[0].as_float()? >= pair[1].as_float()? {
            return Ok(Val::Bool(false));
        }
    }
    Ok(Val::Bool(true))
}

fn cmp_gt(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("> requires at least 2 args");
    }
    for pair in args.windows(2) {
        if pair[0].as_float()? <= pair[1].as_float()? {
            return Ok(Val::Bool(false));
        }
    }
    Ok(Val::Bool(true))
}

fn cmp_le(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!("<= requires at least 2 args");
    }
    for pair in args.windows(2) {
        if pair[0].as_float()? > pair[1].as_float()? {
            return Ok(Val::Bool(false));
        }
    }
    Ok(Val::Bool(true))
}

fn cmp_ge(args: &[Val]) -> Result<Val> {
    if args.len() < 2 {
        bail!(">= requires at least 2 args");
    }
    for pair in args.windows(2) {
        if pair[0].as_float()? < pair[1].as_float()? {
            return Ok(Val::Bool(false));
        }
    }
    Ok(Val::Bool(true))
}

fn logic_not(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("not requires 1 arg");
    }
    Ok(Val::Bool(!args[0].is_truthy()))
}

fn pred_zero(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("zero? requires 1 arg");
    }
    Ok(Val::Bool(args[0].as_float()? == 0.0))
}

fn pred_positive(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("positive? requires 1 arg");
    }
    Ok(Val::Bool(args[0].as_float()? > 0.0))
}

fn pred_negative(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("negative? requires 1 arg");
    }
    Ok(Val::Bool(args[0].as_float()? < 0.0))
}

fn pred_even(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("even? requires 1 arg");
    }
    Ok(Val::Bool(args[0].as_int()? % 2 == 0))
}

fn pred_odd(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("odd? requires 1 arg");
    }
    Ok(Val::Bool(args[0].as_int()? % 2 != 0))
}

// ── Type predicates ───────────────────────────────────────────────────────────

fn pred_null(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::Nil) | None)))
}

fn pred_pair(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(
        matches!(args.first(), Some(Val::List(v)) if !v.is_empty()),
    ))
}

fn pred_list(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(
        args.first(),
        Some(Val::List(_) | Val::Nil)
    )))
}

fn pred_number(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(
        args.first(),
        Some(Val::Int(_) | Val::Float(_))
    )))
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
    Ok(Val::Bool(matches!(
        args.first(),
        Some(Val::Lambda(_) | Val::Native(_))
    )))
}

fn pred_ssh(args: &[Val]) -> Result<Val> {
    Ok(Val::Bool(matches!(args.first(), Some(Val::SshSession(_)))))
}

// ── List operations ───────────────────────────────────────────────────────────

fn list_cons(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("cons requires 2 args");
    }
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
    if args.len() != 1 {
        bail!("car requires 1 arg");
    }
    match &args[0] {
        Val::List(v) if !v.is_empty() => Ok(v[0].clone()),
        Val::List(_) | Val::Nil => bail!("car: empty list"),
        other => bail!("car expects list, got {}", other.type_name()),
    }
}

fn list_cdr(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("cdr requires 1 arg");
    }
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
    if args.len() != 1 {
        bail!("length requires 1 arg");
    }
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
    Ok(if out.is_empty() {
        Val::Nil
    } else {
        Val::List(out)
    })
}

fn list_reverse(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("reverse requires 1 arg");
    }
    let mut v = args[0].as_list()?.to_vec();
    v.reverse();
    Ok(if v.is_empty() { Val::Nil } else { Val::List(v) })
}

fn list_ref(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("list-ref requires 2 args");
    }
    let list = args[0].as_list()?;
    let idx = args[1].as_int()? as usize;
    list.get(idx)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("list-ref: index {idx} out of bounds"))
}

fn list_tail(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("list-tail requires 2 args");
    }
    let list = args[0].as_list()?;
    let idx = args[1].as_int()? as usize;
    if idx > list.len() {
        bail!("list-tail: index out of bounds");
    }
    let v = list[idx..].to_vec();
    Ok(if v.is_empty() { Val::Nil } else { Val::List(v) })
}

fn list_map_stub(_args: &[Val]) -> Result<Val> {
    bail!("map is handled as a special form in eval")
}

fn list_filter_stub(_args: &[Val]) -> Result<Val> {
    bail!("filter is handled as a special form in eval")
}

fn list_for_each_stub(_args: &[Val]) -> Result<Val> {
    bail!("for-each is handled as a special form in eval")
}

fn list_assoc(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("assoc requires 2 args");
    }
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
    if args.len() != 2 {
        bail!("member requires 2 args");
    }
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
    if args.len() != 1 {
        bail!("string-length requires 1 arg");
    }
    Ok(Val::Int(args[0].as_str()?.chars().count() as i64))
}

fn str_substring(args: &[Val]) -> Result<Val> {
    if args.len() < 2 || args.len() > 3 {
        bail!("substring requires 2-3 args");
    }
    let s: Vec<char> = args[0].as_str()?.chars().collect();
    let start = args[1].as_int()? as usize;
    let end = if args.len() == 3 {
        args[2].as_int()? as usize
    } else {
        s.len()
    };
    if start > end || end > s.len() {
        bail!("substring: indices out of range");
    }
    Ok(Val::Str(s[start..end].iter().collect()))
}

fn str_to_number(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string->number requires 1 arg");
    }
    let s = args[0].as_str()?;
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Val::Int(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Ok(Val::Float(f));
    }
    Ok(Val::Bool(false))
}

fn num_to_string(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("number->string requires 1 arg");
    }
    Ok(Val::Str(args[0].to_string()))
}

fn str_upcase(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string-upcase requires 1 arg");
    }
    Ok(Val::Str(args[0].as_str()?.to_uppercase()))
}

fn str_downcase(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string-downcase requires 1 arg");
    }
    Ok(Val::Str(args[0].as_str()?.to_lowercase()))
}

fn str_contains(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("string-contains requires 2 args");
    }
    Ok(Val::Bool(args[0].as_str()?.contains(args[1].as_str()?)))
}

fn str_split(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("string-split requires 2 args");
    }
    let s = args[0].as_str()?;
    let delim = args[1].as_str()?;
    let parts: Vec<Val> = s.split(delim).map(|p| Val::Str(p.to_string())).collect();
    Ok(Val::List(parts))
}

fn str_trim(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string-trim requires 1 arg");
    }
    Ok(Val::Str(args[0].as_str()?.trim().to_string()))
}

fn str_to_symbol(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string->symbol requires 1 arg");
    }
    Ok(Val::Symbol(args[0].as_str()?.to_string()))
}

fn symbol_to_str(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("symbol->string requires 1 arg");
    }
    match &args[0] {
        Val::Symbol(s) => Ok(Val::Str(s.clone())),
        other => bail!("symbol->string expects symbol, got {}", other.type_name()),
    }
}

fn str_to_list(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string->list requires 1 arg");
    }
    let chars: Vec<Val> = args[0]
        .as_str()?
        .chars()
        .map(|c| Val::Str(c.to_string()))
        .collect();
    Ok(Val::List(chars))
}

fn list_to_str(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("list->string requires 1 arg");
    }
    let mut s = String::new();
    for c in args[0].as_list()? {
        s.push_str(c.as_str()?);
    }
    Ok(Val::Str(s))
}

// ── Bytes operations ──────────────────────────────────────────────────────────

fn str_to_bytes(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("string->bytes requires 1 arg");
    }
    Ok(Val::Bytes(args[0].as_str()?.as_bytes().to_vec()))
}

fn bytes_to_str(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bytes->string requires 1 arg");
    }
    let s = String::from_utf8_lossy(args[0].as_bytes()?).into_owned();
    Ok(Val::Str(s))
}

fn bytes_to_hex(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bytes->hex requires 1 arg");
    }
    Ok(Val::Str(hex::encode(args[0].as_bytes()?)))
}

fn hex_to_bytes(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("hex->bytes requires 1 arg");
    }
    let bytes = hex::decode(args[0].as_str()?).map_err(|e| anyhow::anyhow!("hex->bytes: {e}"))?;
    Ok(Val::Bytes(bytes))
}

fn bytes_length(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bytes-length requires 1 arg");
    }
    Ok(Val::Int(args[0].as_bytes()?.len() as i64))
}

fn bytes_ref(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("bytes-ref requires 2 args");
    }
    let b = args[0].as_bytes()?;
    let i = args[1].as_int()? as usize;
    b.get(i)
        .map(|&v| Val::Int(v as i64))
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
    if args.len() != 1 {
        bail!("random-bytes requires 1 arg (length)");
    }
    let n = args[0].as_int()? as usize;
    let mut buf = vec![0u8; n];
    use rand::RngCore;
    rand::thread_rng().fill_bytes(&mut buf);
    Ok(Val::Bytes(buf))
}

fn subbytes(args: &[Val]) -> Result<Val> {
    if args.len() < 2 || args.len() > 3 {
        bail!("subbytes requires 2-3 args");
    }
    let b = args[0].as_bytes()?;
    let start = args[1].as_int()? as usize;
    let end = if args.len() == 3 {
        args[2].as_int()? as usize
    } else {
        b.len()
    };
    if start > end || end > b.len() {
        bail!("subbytes: indices out of range");
    }
    Ok(Val::Bytes(b[start..end].to_vec()))
}

// ── I/O ───────────────────────────────────────────────────────────────────────

fn io_display(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("display requires 1 arg");
    }
    print!("{}", args[0]);
    Ok(Val::Nil)
}

fn io_newline(_args: &[Val]) -> Result<Val> {
    println!();
    Ok(Val::Nil)
}

fn io_error(args: &[Val]) -> Result<Val> {
    let msg = args
        .iter()
        .map(|a| a.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    bail!("{msg}")
}

fn io_format_stub(_args: &[Val]) -> Result<Val> {
    bail!("format is handled as a special form in eval")
}

// ── Crypto ────────────────────────────────────────────────────────────────────

fn crypto_sha256(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("sha256 requires 1 arg");
    }
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(args[0].as_bytes()?);
    Ok(Val::Bytes(hash.to_vec()))
}

fn crypto_sha512(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("sha512 requires 1 arg");
    }
    use sha2::{Digest, Sha512};
    let hash = Sha512::digest(args[0].as_bytes()?);
    Ok(Val::Bytes(hash.to_vec()))
}

fn crypto_hmac_sha256(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("hmac-sha256 requires 2 args: (key message)");
    }
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
        if s.len() != 32 {
            bail!("ed25519-keygen: seed must be 32 bytes");
        }
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
    if args.len() != 2 {
        bail!("ed25519-sign requires 2 args: (private-key message)");
    }
    use ed25519_dalek::{Signer, SigningKey};
    let key_bytes: [u8; 32] = args[0]
        .as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519-sign: private key must be 32 bytes"))?;
    let sk = SigningKey::from_bytes(&key_bytes);
    let sig = sk.sign(args[1].as_bytes()?);
    Ok(Val::Bytes(sig.to_bytes().to_vec()))
}

fn crypto_ed25519_verify(args: &[Val]) -> Result<Val> {
    if args.len() != 3 {
        bail!("ed25519-verify requires 3 args: (public-key message signature)");
    }
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let pk_bytes: [u8; 32] = args[0]
        .as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519-verify: public key must be 32 bytes"))?;
    let pk = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| anyhow::anyhow!("ed25519-verify: bad public key: {e}"))?;
    let sig_bytes: [u8; 64] = args[2]
        .as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("ed25519-verify: signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&sig_bytes);
    Ok(Val::Bool(pk.verify(args[1].as_bytes()?, &sig).is_ok()))
}

fn crypto_chacha20_seal(args: &[Val]) -> Result<Val> {
    if args.len() != 3 {
        bail!("chacha20-seal requires 3 args: (key nonce plaintext)");
    }
    use chacha20poly1305::{
        aead::{generic_array::GenericArray, Aead},
        ChaCha20Poly1305, KeyInit,
    };
    let key = args[0].as_bytes()?;
    let nonce_bytes = args[1].as_bytes()?;
    if key.len() != 32 {
        bail!("chacha20-seal: key must be 32 bytes");
    }
    if nonce_bytes.len() != 12 {
        bail!("chacha20-seal: nonce must be 12 bytes");
    }
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce = GenericArray::from_slice(nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, args[2].as_bytes()?)
        .map_err(|e| anyhow::anyhow!("chacha20-seal: {e}"))?;
    Ok(Val::Bytes(ciphertext))
}

fn crypto_chacha20_open(args: &[Val]) -> Result<Val> {
    if args.len() != 3 {
        bail!("chacha20-open requires 3 args: (key nonce ciphertext)");
    }
    use chacha20poly1305::{
        aead::{generic_array::GenericArray, Aead},
        ChaCha20Poly1305, KeyInit,
    };
    let key = args[0].as_bytes()?;
    let nonce_bytes = args[1].as_bytes()?;
    if key.len() != 32 {
        bail!("chacha20-open: key must be 32 bytes");
    }
    if nonce_bytes.len() != 12 {
        bail!("chacha20-open: nonce must be 12 bytes");
    }
    let cipher = ChaCha20Poly1305::new(GenericArray::from_slice(key));
    let nonce = GenericArray::from_slice(nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, args[2].as_bytes()?)
        .map_err(|e| anyhow::anyhow!("chacha20-open: decryption failed: {e}"))?;
    Ok(Val::Bytes(plaintext))
}

fn crypto_x25519_keygen(_args: &[Val]) -> Result<Val> {
    use rand::rngs::OsRng;
    use x25519_dalek::{PublicKey, StaticSecret};
    let sk = StaticSecret::random_from_rng(OsRng);
    let pk = PublicKey::from(&sk);
    Ok(Val::List(vec![
        Val::Bytes(sk.to_bytes().to_vec()),
        Val::Bytes(pk.to_bytes().to_vec()),
    ]))
}

fn crypto_x25519_dh(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("x25519-dh requires 2 args: (my-private-key their-public-key)");
    }
    use x25519_dalek::{PublicKey, StaticSecret};
    let sk_bytes: [u8; 32] = args[0]
        .as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("x25519-dh: private key must be 32 bytes"))?;
    let pk_bytes: [u8; 32] = args[1]
        .as_bytes()?
        .try_into()
        .map_err(|_| anyhow::anyhow!("x25519-dh: public key must be 32 bytes"))?;
    let sk = StaticSecret::from(sk_bytes);
    let pk = PublicKey::from(pk_bytes);
    let shared = sk.diffie_hellman(&pk);
    Ok(Val::Bytes(shared.to_bytes().to_vec()))
}

// ── JSON ──────────────────────────────────────────────────────────────────────

pub fn json_val_to_lisp(v: serde_json::Value) -> Val {
    match v {
        serde_json::Value::Null => Val::Nil,
        serde_json::Value::Bool(b) => Val::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Val::Int(i)
            } else {
                Val::Float(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Val::Str(s),
        serde_json::Value::Array(arr) => Val::List(arr.into_iter().map(json_val_to_lisp).collect()),
        serde_json::Value::Object(map) => Val::List(
            map.into_iter()
                .map(|(k, v)| Val::List(vec![Val::Str(k), json_val_to_lisp(v)]))
                .collect(),
        ),
    }
}

fn lisp_val_to_json(v: &Val) -> serde_json::Value {
    match v {
        Val::Nil => serde_json::Value::Null,
        Val::Bool(b) => serde_json::Value::Bool(*b),
        Val::Int(n) => serde_json::Value::Number((*n).into()),
        Val::Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Val::Str(s) => serde_json::Value::String(s.clone()),
        Val::Symbol(s) => serde_json::Value::String(s.clone()),
        Val::Bytes(b) => {
            use base64::Engine;
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
        Val::List(items) => {
            // If it looks like an alist ((key val) ...) → object; else array.
            let is_alist = !items.is_empty() && items.iter().all(|item| {
                matches!(item, Val::List(pair) if pair.len() == 2 && matches!(&pair[0], Val::Str(_) | Val::Symbol(_)))
            });
            if is_alist {
                let mut map = serde_json::Map::new();
                for item in items {
                    if let Val::List(pair) = item {
                        let key = match &pair[0] {
                            Val::Str(s) | Val::Symbol(s) => s.clone(),
                            _ => unreachable!(),
                        };
                        map.insert(key, lisp_val_to_json(&pair[1]));
                    }
                }
                serde_json::Value::Object(map)
            } else {
                serde_json::Value::Array(items.iter().map(lisp_val_to_json).collect())
            }
        }
        other => serde_json::Value::String(other.to_string()),
    }
}

fn json_parse(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("json-parse requires 1 arg");
    }
    let s = args[0].as_str()?;
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| anyhow::anyhow!("json-parse: {e}"))?;
    Ok(json_val_to_lisp(v))
}

fn json_stringify(args: &[Val]) -> Result<Val> {
    if args.is_empty() || args.len() > 2 {
        bail!("json-stringify requires 1 or 2 args: (val [pretty?])");
    }
    let json = lisp_val_to_json(&args[0]);
    let s = if args.get(1).map_or(false, |v| matches!(v, Val::Bool(true))) {
        serde_json::to_string_pretty(&json)
    } else {
        serde_json::to_string(&json)
    }
    .map_err(|e| anyhow::anyhow!("json-stringify: {e}"))?;
    Ok(Val::Str(s))
}

// ── YAML ──────────────────────────────────────────────────────────────────────

fn yaml_parse(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("yaml-parse requires 1 arg");
    }
    let s = args[0].as_str()?;
    let jv: serde_json::Value =
        serde_yaml::from_str(s).map_err(|e| anyhow::anyhow!("yaml-parse: {e}"))?;
    Ok(json_val_to_lisp(jv))
}

fn yaml_stringify(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("yaml-stringify requires 1 arg");
    }
    let jv = lisp_val_to_json(&args[0]);
    let s = serde_yaml::to_string(&jv).map_err(|e| anyhow::anyhow!("yaml-stringify: {e}"))?;
    Ok(Val::Str(s))
}

// ── TOML ──────────────────────────────────────────────────────────────────────

fn toml_parse(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("toml-parse requires 1 arg");
    }
    let s = args[0].as_str()?;
    let tv: toml::Value = toml::from_str(s).map_err(|e| anyhow::anyhow!("toml-parse: {e}"))?;
    Ok(toml_val_to_lisp(tv))
}

fn toml_val_to_lisp(v: toml::Value) -> Val {
    match v {
        toml::Value::Boolean(b) => Val::Bool(b),
        toml::Value::Integer(n) => Val::Int(n),
        toml::Value::Float(f) => Val::Float(f),
        toml::Value::String(s) => Val::Str(s),
        toml::Value::Datetime(d) => Val::Str(d.to_string()),
        toml::Value::Array(arr) => Val::List(arr.into_iter().map(toml_val_to_lisp).collect()),
        toml::Value::Table(map) => Val::List(
            map.into_iter()
                .map(|(k, v)| Val::List(vec![Val::Str(k), toml_val_to_lisp(v)]))
                .collect(),
        ),
    }
}

fn toml_stringify(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("toml-stringify requires 1 arg");
    }
    // Route through JSON value for conversion
    let jv = lisp_val_to_json(&args[0]);
    let tv: toml::Value = serde_json::from_value(jv)
        .map_err(|e| anyhow::anyhow!("toml-stringify: cannot convert to TOML: {e}"))?;
    let s = toml::to_string(&tv).map_err(|e| anyhow::anyhow!("toml-stringify: {e}"))?;
    Ok(Val::Str(s))
}

// ── Lisp reader ───────────────────────────────────────────────────────────────

fn lisp_parse(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("lisp-parse requires 1 arg");
    }
    let s = args[0].as_str()?;
    let exprs = crate::lisp::reader::parse_str(s)?;
    Ok(Val::List(exprs))
}

// ── String lines ─────────────────────────────────────────────────────────────

fn str_lines(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("lines requires 1 arg");
    }
    let s = args[0].as_str()?;
    Ok(Val::List(
        s.lines().map(|l| Val::Str(l.to_string())).collect(),
    ))
}

fn str_unlines(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("unlines requires 1 arg");
    }
    let list = args[0].as_list()?;
    let mut out = String::new();
    for (i, v) in list.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(v.as_str()?);
    }
    Ok(Val::Str(out))
}

fn str_words(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("words requires 1 arg");
    }
    let s = args[0].as_str()?;
    Ok(Val::List(
        s.split_whitespace()
            .map(|w| Val::Str(w.to_string()))
            .collect(),
    ))
}

fn str_unwords(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("unwords requires 1 arg");
    }
    let list = args[0].as_list()?;
    let parts: Result<Vec<&str>> = list.iter().map(|v| v.as_str()).collect();
    Ok(Val::Str(parts?.join(" ")))
}

fn crypto_b64_encode(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("base64-encode requires 1 arg");
    }
    use base64::Engine;
    Ok(Val::Str(
        base64::engine::general_purpose::STANDARD.encode(args[0].as_bytes()?),
    ))
}

fn crypto_b64_decode(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("base64-decode requires 1 arg");
    }
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
        if let Val::Bytes(b) = result {
            assert_eq!(b.len(), 32);
        } else {
            panic!();
        }
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

    // ── Argumentation tests ───────────────────────────────────────────────────

    fn simple_fw() -> Val {
        // A ← B ← C ← D  (each attacks the one to its left)
        let a = arg_make(&[Val::Str("A".into()), Val::Str("sky is blue".into())]).unwrap();
        let b = arg_make(&[
            Val::Str("B".into()),
            Val::Str("sky is sometimes red".into()),
        ])
        .unwrap();
        let c = arg_make(&[Val::Str("C".into()), Val::Str("red only at sunset".into())]).unwrap();
        let d = arg_make(&[Val::Str("D".into()), Val::Str("sunset is frequent".into())]).unwrap();
        let fw = arg_make_framework(&[Val::List(vec![a, b, c, d]), Val::Nil]).unwrap();
        let fw = arg_add_attack(&[fw, Val::Str("B".into()), Val::Str("A".into())]).unwrap();
        let fw = arg_add_attack(&[fw, Val::Str("C".into()), Val::Str("B".into())]).unwrap();
        arg_add_attack(&[fw, Val::Str("D".into()), Val::Str("C".into())]).unwrap()
    }

    #[test]
    fn test_arg_grounded_extension_chain() {
        // A←B←C←D: D and B are in grounded extension; A and C are out
        let fw = simple_fw();
        let ext = arg_grounded_ext(&[fw]).unwrap();
        let ids: Vec<&str> = ext
            .as_list()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().ok())
            .collect();
        assert!(ids.contains(&"D"), "D should be in extension (unattacked)");
        assert!(
            ids.contains(&"B"),
            "B should be in extension (C defeated by D)"
        );
        assert!(!ids.contains(&"A"), "A should be out (attacked by B)");
        assert!(!ids.contains(&"C"), "C should be out (attacked by D)");
    }

    #[test]
    fn test_arg_attacks_q() {
        let fw = simple_fw();
        let yes = arg_attacks_q(&[fw.clone(), Val::Str("B".into()), Val::Str("A".into())]).unwrap();
        let no = arg_attacks_q(&[fw, Val::Str("A".into()), Val::Str("B".into())]).unwrap();
        assert_eq!(yes, Val::Bool(true));
        assert_eq!(no, Val::Bool(false));
    }

    #[test]
    fn test_arg_defended_q() {
        let fw = simple_fw();
        let b_defended = arg_defended_q(&[fw.clone(), Val::Str("B".into())]).unwrap();
        let a_defended = arg_defended_q(&[fw, Val::Str("A".into())]).unwrap();
        assert_eq!(b_defended, Val::Bool(true));
        assert_eq!(a_defended, Val::Bool(false));
    }

    #[test]
    fn test_sys_processes_returns_list() {
        let procs = sys_processes(&[]).unwrap();
        let list = procs.as_list().unwrap();
        assert!(!list.is_empty(), "expected at least one process");
        // Each entry is (pid name cmd)
        let first = list[0].as_list().unwrap();
        assert_eq!(first.len(), 3);
        assert!(matches!(first[0], Val::Int(_)));
        assert!(matches!(first[1], Val::Str(_)));
        assert!(matches!(first[2], Val::Str(_)));
    }

    #[test]
    fn test_sys_process_info_self() {
        let my_pid = std::process::id();
        let result = sys_process_info(&[Val::Int(my_pid as i64)]).unwrap();
        // Should find the current process.
        assert!(
            matches!(result, Val::List(_)),
            "expected (pid name cmd), got nil"
        );
    }

    #[test]
    fn test_sys_process_info_missing() {
        // PID 0 is never a real user process.
        let result = sys_process_info(&[Val::Int(0)]).unwrap();
        assert_eq!(result, Val::Nil);
    }

    // ── JSON ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_json_parse_primitives() {
        assert_eq!(json_parse(&[Val::Str("null".into())]).unwrap(), Val::Nil);
        assert_eq!(
            json_parse(&[Val::Str("true".into())]).unwrap(),
            Val::Bool(true)
        );
        assert_eq!(json_parse(&[Val::Str("42".into())]).unwrap(), Val::Int(42));
        assert_eq!(
            json_parse(&[Val::Str("3.14".into())]).unwrap(),
            Val::Float(3.14)
        );
        assert_eq!(
            json_parse(&[Val::Str(r#""hello""#.into())]).unwrap(),
            Val::Str("hello".into())
        );
    }

    #[test]
    fn test_json_parse_array() {
        let result = json_parse(&[Val::Str("[1,2,3]".into())]).unwrap();
        assert_eq!(
            result,
            Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)])
        );
    }

    #[test]
    fn test_json_parse_object_is_alist() {
        let result = json_parse(&[Val::Str(r#"{"x":1}"#.into())]).unwrap();
        assert!(matches!(result, Val::List(_)));
        if let Val::List(items) = result {
            assert_eq!(items.len(), 1);
            assert!(
                matches!(&items[0], Val::List(pair) if pair.len() == 2 && pair[0] == Val::Str("x".into()) && pair[1] == Val::Int(1))
            );
        }
    }

    #[test]
    fn test_json_stringify_roundtrip() {
        let val = Val::List(vec![Val::Int(1), Val::Str("hi".into()), Val::Bool(false)]);
        let s = json_stringify(&[val.clone()]).unwrap();
        let back = json_parse(&[s]).unwrap();
        assert_eq!(back, val);
    }

    #[test]
    fn test_json_parse_invalid_returns_err() {
        assert!(json_parse(&[Val::Str("{bad json".into())]).is_err());
    }

    // ── ->number / count ─────────────────────────────────────────────────────

    #[test]
    fn test_val_to_number_primitives() {
        assert_eq!(val_to_number(&[Val::Int(7)]).unwrap(), Val::Int(7));
        assert_eq!(val_to_number(&[Val::Float(1.5)]).unwrap(), Val::Float(1.5));
        assert_eq!(val_to_number(&[Val::Bool(true)]).unwrap(), Val::Int(1));
        assert_eq!(val_to_number(&[Val::Bool(false)]).unwrap(), Val::Int(0));
        assert_eq!(val_to_number(&[Val::Nil]).unwrap(), Val::Int(0));
    }

    #[test]
    fn test_val_to_number_string_parse() {
        assert_eq!(
            val_to_number(&[Val::Str("42".into())]).unwrap(),
            Val::Int(42)
        );
        assert_eq!(
            val_to_number(&[Val::Str("3.14".into())]).unwrap(),
            Val::Float(3.14)
        );
        assert_eq!(
            val_to_number(&[Val::Str("hi".into())]).unwrap(),
            Val::Int(2)
        ); // char count
    }

    /// Invariant: a JSON object always produces a number via ->number.
    #[test]
    fn test_json_object_always_has_numeric_coercion() {
        // object with N keys → N
        let obj = json_val_to_lisp(serde_json::json!({"a": 1, "b": 2, "c": 3}));
        assert_eq!(val_to_number(&[obj]).unwrap(), Val::Int(3));

        // empty object → 0
        let empty = json_val_to_lisp(serde_json::json!({}));
        assert_eq!(val_to_number(&[empty]).unwrap(), Val::Int(0));

        // array with M elements → M
        let arr = json_val_to_lisp(serde_json::json!([10, 20]));
        assert_eq!(val_to_number(&[arr]).unwrap(), Val::Int(2));

        // JSON primitives coerce directly
        assert_eq!(
            val_to_number(&[json_val_to_lisp(serde_json::json!(true))]).unwrap(),
            Val::Int(1)
        );
        assert_eq!(
            val_to_number(&[json_val_to_lisp(serde_json::json!(null))]).unwrap(),
            Val::Int(0)
        );
        assert_eq!(
            val_to_number(&[json_val_to_lisp(serde_json::json!(99))]).unwrap(),
            Val::Int(99)
        );
    }

    // ── YAML ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_yaml_parse_scalar() {
        let result = yaml_parse(&[Val::Str("42".into())]).unwrap();
        assert_eq!(result, Val::Int(42));
    }

    #[test]
    fn test_yaml_parse_mapping() {
        let yaml = "name: alice\nage: 30\n";
        let result = yaml_parse(&[Val::Str(yaml.into())]).unwrap();
        let Val::List(pairs) = result else {
            panic!("expected list")
        };
        let find = |key: &str| {
            pairs
                .iter()
                .any(|p| matches!(p, Val::List(kv) if kv[0] == Val::Str(key.into())))
        };
        assert!(find("name"));
        assert!(find("age"));
    }

    #[test]
    fn test_yaml_parse_sequence() {
        let result = yaml_parse(&[Val::Str("- 1\n- 2\n- 3\n".into())]).unwrap();
        assert_eq!(
            result,
            Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)])
        );
    }

    #[test]
    fn test_yaml_stringify_roundtrip() {
        let val = Val::List(vec![Val::List(vec![Val::Str("x".into()), Val::Int(1)])]);
        let s = yaml_stringify(&[val]).unwrap();
        let back = yaml_parse(&[s]).unwrap();
        let Val::List(pairs) = back else {
            panic!("expected list")
        };
        assert!(pairs
            .iter()
            .any(|p| matches!(p, Val::List(kv) if kv[0] == Val::Str("x".into()))));
    }

    // ── TOML ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_toml_parse_table() {
        let toml = "[server]\nhost = \"localhost\"\nport = 8080\n";
        let result = toml_parse(&[Val::Str(toml.into())]).unwrap();
        let Val::List(pairs) = result else {
            panic!("expected list")
        };
        assert!(pairs
            .iter()
            .any(|p| matches!(p, Val::List(kv) if kv[0] == Val::Str("server".into()))));
    }

    #[test]
    fn test_toml_parse_invalid_returns_err() {
        assert!(toml_parse(&[Val::Str("= bad".into())]).is_err());
    }

    // ── Lisp reader ──────────────────────────────────────────────────────────

    #[test]
    fn test_lisp_parse_returns_list_of_exprs() {
        let result = lisp_parse(&[Val::Str("(+ 1 2) (+ 3 4)".into())]).unwrap();
        if let Val::List(exprs) = result {
            assert_eq!(exprs.len(), 2);
        } else {
            panic!("expected list");
        }
    }

    #[test]
    fn test_lisp_parse_invalid_returns_err() {
        assert!(lisp_parse(&[Val::Str("(unterminated".into())]).is_err());
    }

    // ── String lines ─────────────────────────────────────────────────────────

    #[test]
    fn test_str_lines_splits() {
        let result = str_lines(&[Val::Str("a\nb\nc".into())]).unwrap();
        assert_eq!(
            result,
            Val::List(vec![
                Val::Str("a".into()),
                Val::Str("b".into()),
                Val::Str("c".into()),
            ])
        );
    }

    #[test]
    fn test_str_unlines_joins() {
        let result =
            str_unlines(&[Val::List(vec![Val::Str("a".into()), Val::Str("b".into())])]).unwrap();
        assert_eq!(result, Val::Str("a\nb".into()));
    }

    #[test]
    fn test_str_words_splits_whitespace() {
        let result = str_words(&[Val::Str("hello world  foo".into())]).unwrap();
        assert_eq!(
            result,
            Val::List(vec![
                Val::Str("hello".into()),
                Val::Str("world".into()),
                Val::Str("foo".into()),
            ])
        );
    }

    #[test]
    fn test_str_unwords_joins_with_space() {
        let result = str_unwords(&[Val::List(vec![
            Val::Str("hello".into()),
            Val::Str("world".into()),
        ])])
        .unwrap();
        assert_eq!(result, Val::Str("hello world".into()));
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

fn make_promise_val(lang: &str, code: &str, effect: Option<(i64, i64)>) -> Val {
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
        Val::List(parts) if parts.len() >= 5 && parts[0] == sym("promise") => Ok(parts),
        _ => bail!("not a promise"),
    }
}

fn bundle_fields(v: &Val) -> Result<&[Val]> {
    match v {
        Val::List(parts) if parts.len() == 4 && parts[0] == sym("bundle") => Ok(parts),
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
    if args.len() != 1 {
        bail!("promise?: requires 1 arg");
    }
    Ok(Val::Bool(promise_fields(&args[0]).is_ok()))
}

fn proof_promise_lang(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("promise-lang: requires 1 arg");
    }
    let parts = promise_fields(&args[0])?;
    Ok(parts[1].clone())
}

fn proof_promise_code(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("promise-code: requires 1 arg");
    }
    let parts = promise_fields(&args[0])?;
    Ok(parts[2].clone())
}

fn proof_promise_id(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("promise-id: requires 1 arg");
    }
    let parts = promise_fields(&args[0])?;
    Ok(parts[3].clone())
}

fn proof_promise_effect(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("promise-effect: requires 1 arg");
    }
    let parts = promise_fields(&args[0])?;
    Ok(parts.get(5).cloned().unwrap_or(Val::Nil))
}

/// Parse the code of a lisp promise back into a live Val tree.
///
/// This is the "open" operation — it takes the sealed code string and
/// returns the actual AST so type annotations, structure, and subterms
/// are all accessible.
fn proof_promise_ast(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("promise-ast: requires 1 arg");
    }
    let parts = promise_fields(&args[0])?;
    if parts[1] != sym("lisp") {
        bail!(
            "promise-ast: only lisp promises have an AST (got {})",
            parts[1]
        );
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
    if args.len() != 2 {
        bail!("make-bundle: requires 2 args (promise comments)");
    }
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
    if args.len() != 1 {
        bail!("bundle?: requires 1 arg");
    }
    Ok(Val::Bool(bundle_fields(&args[0]).is_ok()))
}

fn proof_bundle_primary(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bundle-primary: requires 1 arg");
    }
    let parts = bundle_fields(&args[0])?;
    match &parts[3] {
        Val::List(proofs) if !proofs.is_empty() => Ok(proofs[0].clone()),
        _ => bail!("bundle-primary: bundle has no proofs"),
    }
}

fn proof_bundle_proofs(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bundle-proofs: requires 1 arg");
    }
    let parts = bundle_fields(&args[0])?;
    Ok(parts[3].clone())
}

fn proof_bundle_comments(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bundle-comments: requires 1 arg");
    }
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

// ── System ────────────────────────────────────────────────────────────────────
//
// Process list format: each entry is (pid name cmd)
//   pid  — int
//   name — string (executable name)
//   cmd  — string (full command line, space-joined)

fn process_entry(pid: u32, name: &str, cmd: &[String]) -> Val {
    Val::List(vec![
        Val::Int(pid as i64),
        Val::Str(name.to_string()),
        Val::Str(cmd.join(" ")),
    ])
}

/// `(processes)` → list of (pid name cmd) for every running process.
fn sys_processes(args: &[Val]) -> Result<Val> {
    if !args.is_empty() {
        bail!("processes: takes no args");
    }
    use sysinfo::{ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let mut list: Vec<Val> = sys
        .processes()
        .iter()
        .map(|(pid, p)| {
            let cmd: Vec<String> = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            process_entry(pid.as_u32(), &p.name().to_string_lossy(), &cmd)
        })
        .collect();
    // Sort by pid for stable output.
    list.sort_by_key(|e| match e {
        Val::List(v) => v[0].as_int().unwrap_or(0),
        _ => 0,
    });
    Ok(Val::List(list))
}

/// `(process-info pid)` → (pid name cmd) or nil if not found.
fn sys_process_info(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("process-info: requires 1 arg (pid)");
    }
    let target = args[0].as_int()? as u32;
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(target)]), true);
    Ok(sys
        .process(Pid::from_u32(target))
        .map(|p| {
            let cmd: Vec<String> = p
                .cmd()
                .iter()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            process_entry(target, &p.name().to_string_lossy(), &cmd)
        })
        .unwrap_or(Val::Nil))
}

/// True if all proofs that have effects agree on (pops pushes).
fn proof_bundle_effects_agree(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("bundle-effects-agree?: requires 1 arg");
    }
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

// ── Argumentation ─────────────────────────────────────────────────────────────
//
// Dung-style Abstract Argumentation Frameworks (AAF).
//
// Argument:   (argument <id:str> <claim:str>)
// Framework:  (framework <args:list> <attacks:list>)
//             attacks = list of (attacker-id attacked-id) pairs
//
// Semantics:
//   - A *attacks* B  iff (A.id B.id) ∈ attacks
//   - A set S *defends* A  iff ∀B that attacks A, ∃C∈S s.t. C attacks B
//   - Grounded extension = least fixed point of F(S) = {A | S defends A}

fn arg_id_str(v: &Val) -> Result<String> {
    match v {
        Val::List(p) if p.len() == 3 && p[0] == sym("argument") => {
            p[1].as_str().map(|s| s.to_string())
        }
        _ => bail!("not an argument"),
    }
}

/// `(make-argument id claim)` → argument
fn arg_make(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("make-argument: requires 2 args (id claim)");
    }
    let _ = args[0].as_str()?; // id must be string
    let _ = args[1].as_str()?; // claim must be string
    Ok(Val::List(vec![
        sym("argument"),
        args[0].clone(),
        args[1].clone(),
    ]))
}

fn arg_is(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("argument?: requires 1 arg");
    }
    Ok(Val::Bool(arg_id_str(&args[0]).is_ok()))
}

fn arg_id(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("argument-id: requires 1 arg");
    }
    Ok(Val::Str(arg_id_str(&args[0])?))
}

fn arg_claim(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("argument-claim: requires 1 arg");
    }
    match &args[0] {
        Val::List(p) if p.len() == 3 && p[0] == sym("argument") => Ok(p[2].clone()),
        _ => bail!("not an argument"),
    }
}

/// `(make-framework args attacks)` → framework
/// args    = list of argument values
/// attacks = list of (attacker-id attacked-id) string pairs, or nil
fn arg_make_framework(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("make-framework: requires 2 args (args attacks)");
    }
    let _ = args[0].as_list()?;
    let _ = args[1].as_list()?; // nil or list ok
    Ok(Val::List(vec![
        sym("framework"),
        args[0].clone(),
        args[1].clone(),
    ]))
}

fn fw_parts(v: &Val) -> Result<(&[Val], &[Val])> {
    match v {
        Val::List(p) if p.len() == 3 && p[0] == sym("framework") => {
            Ok((p[1].as_list()?, p[2].as_list()?))
        }
        _ => bail!("not a framework"),
    }
}

fn arg_framework_args(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("framework-arguments: requires 1 arg");
    }
    Ok(Val::List(fw_parts(&args[0])?.0.to_vec()))
}

fn arg_framework_attacks(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("framework-attacks: requires 1 arg");
    }
    let attacks = fw_parts(&args[0])?.1;
    if attacks.is_empty() {
        return Ok(Val::Nil);
    }
    Ok(Val::List(attacks.to_vec()))
}

/// `(add-attack framework attacker-id attacked-id)` → new framework
fn arg_add_attack(args: &[Val]) -> Result<Val> {
    if args.len() != 3 {
        bail!("add-attack: requires 3 args (framework attacker-id attacked-id)");
    }
    let (fw_args, fw_atk) = fw_parts(&args[0])?;
    let pair = Val::List(vec![args[1].clone(), args[2].clone()]);
    let mut new_attacks = fw_atk.to_vec();
    new_attacks.push(pair);
    Ok(Val::List(vec![
        sym("framework"),
        Val::List(fw_args.to_vec()),
        Val::List(new_attacks),
    ]))
}

/// `(attacks? framework a-id b-id)` → bool  (does a attack b?)
fn arg_attacks_q(args: &[Val]) -> Result<Val> {
    if args.len() != 3 {
        bail!("attacks?: requires 3 args");
    }
    let (_, attacks) = fw_parts(&args[0])?;
    let a = args[1].as_str()?;
    let b = args[2].as_str()?;
    let found = attacks.iter().any(|pair| match pair {
        Val::List(p) if p.len() == 2 => {
            p[0].as_str().map(|s| s == a).unwrap_or(false)
                && p[1].as_str().map(|s| s == b).unwrap_or(false)
        }
        _ => false,
    });
    Ok(Val::Bool(found))
}

/// Collect all ids that attack `target_id` in the framework.
fn attackers_of<'a>(attacks: &'a [Val], target_id: &str) -> Vec<&'a str> {
    attacks
        .iter()
        .filter_map(|pair| match pair {
            Val::List(p) if p.len() == 2 => {
                if p[1].as_str().ok()? == target_id {
                    p[0].as_str().ok()
                } else {
                    None
                }
            }
            _ => None,
        })
        .collect()
}

/// `(acceptable? framework arg-id candidate-set-ids)` → bool
///
/// Arg A is acceptable w.r.t. set S iff every attacker of A is attacked by
/// some member of S.
fn arg_acceptable_q(args: &[Val]) -> Result<Val> {
    if args.len() != 3 {
        bail!("acceptable?: requires 3 args (framework arg-id set-ids)");
    }
    let (_, attacks) = fw_parts(&args[0])?;
    let target = args[1].as_str()?;
    let set: Vec<&str> = args[2]
        .as_list()?
        .iter()
        .filter_map(|v| v.as_str().ok())
        .collect();
    let ok = attackers_of(attacks, target).iter().all(|att| {
        set.iter().any(|s| {
            attacks.iter().any(|pair| match pair {
                Val::List(p) if p.len() == 2 => {
                    p[0].as_str().map(|x| x == *s).unwrap_or(false)
                        && p[1].as_str().map(|x| x == *att).unwrap_or(false)
                }
                _ => false,
            })
        })
    });
    Ok(Val::Bool(ok))
}

/// `(defended? framework arg-id)` → bool
///
/// A is defended by the grounded extension if it ends up in it.
fn arg_defended_q(args: &[Val]) -> Result<Val> {
    if args.len() != 2 {
        bail!("defended?: requires 2 args (framework arg-id)");
    }
    let ext = arg_grounded_ext(&[args[0].clone()])?;
    let target = args[1].as_str()?;
    let ids = ext.as_list()?;
    Ok(Val::Bool(
        ids.iter().any(|v| v.as_str().ok() == Some(target)),
    ))
}

/// `(grounded-extension framework)` → list of accepted argument ids
///
/// Computes the grounded extension via the characteristic function F:
///   F(S) = { A | A is acceptable w.r.t. S }
/// Iterate from S = {} until fixed point.
fn arg_grounded_ext(args: &[Val]) -> Result<Val> {
    if args.len() != 1 {
        bail!("grounded-extension: requires 1 arg");
    }
    let (fw_args, attacks) = fw_parts(&args[0])?;
    let all_ids: Vec<String> = fw_args
        .iter()
        .map(|a| arg_id_str(a))
        .collect::<Result<_>>()?;

    let mut current: Vec<String> = vec![];
    loop {
        let current_refs: Vec<&str> = current.iter().map(|s| s.as_str()).collect();
        let set_val = Val::List(
            current_refs
                .iter()
                .map(|s| Val::Str(s.to_string()))
                .collect(),
        );
        let next: Vec<String> = all_ids
            .iter()
            .filter(|id| {
                let ok = attackers_of(attacks, id).iter().all(|att| {
                    current_refs.iter().any(|s| {
                        attacks.iter().any(|pair| match pair {
                            Val::List(p) if p.len() == 2 => {
                                p[0].as_str().map(|x| x == *s).unwrap_or(false)
                                    && p[1].as_str().map(|x| x == *att).unwrap_or(false)
                            }
                            _ => false,
                        })
                    })
                });
                let _ = set_val.clone(); // suppress unused warning
                ok
            })
            .cloned()
            .collect();
        if next == current {
            break;
        }
        current = next;
    }

    if current.is_empty() {
        Ok(Val::Nil)
    } else {
        Ok(Val::List(current.into_iter().map(Val::Str).collect()))
    }
}
