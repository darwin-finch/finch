/// Async Lisp evaluator.
///
/// `eval()` returns a `BoxFuture` so recursive calls allocate on the heap
/// rather than the Rust stack.  10 000 mutual recursions → 10 000 heap futures,
/// not a stack overflow.  This is the standard Scheme/async pattern when you
/// cannot do TCO at the compiler level.
///
/// Special forms handled here:
///   define  set!  lambda  let  let*  letrec
///   if  cond  and  or  when  unless
///   begin  do  quote  quasiquote
///   map  for-each  apply
///   ssh-connect  ssh-auth-key  ssh-exec  ssh-exec/all  ssh-read-file
///   ssh-write-file  ssh-close  ssh-info
use anyhow::{bail, Result};
use futures::future::BoxFuture;
use std::sync::Arc;

use super::env::{Env, EnvRef};
use super::types::{Lambda, Type, Val};
use super::LispCtx;

/// Evaluate `expr` in `env`.  Returns a `BoxFuture` to enable async recursion
/// without blowing the Rust call stack.
pub fn eval(expr: Val, env: EnvRef, ctx: Arc<LispCtx>) -> BoxFuture<'static, Result<Val>> {
    Box::pin(eval_inner(expr, env, ctx))
}

async fn eval_inner(expr: Val, env: EnvRef, ctx: Arc<LispCtx>) -> Result<Val> {
    match expr {
        // ── Self-evaluating atoms ─────────────────────────────────────────────
        Val::Nil
        | Val::Bool(_)
        | Val::Int(_)
        | Val::Float(_)
        | Val::Str(_)
        | Val::Bytes(_)
        | Val::Lambda(_)
        | Val::Native(_)
        | Val::SshSession(_) => Ok(expr),

        // ── Symbol lookup ─────────────────────────────────────────────────────
        Val::Symbol(ref name) => Env::get(&env, name)
            .ok_or_else(|| anyhow::anyhow!("undefined: {name}")),

        // ── List ─────────────────────────────────────────────────────────────
        Val::List(ref list) if list.is_empty() => Ok(Val::Nil),

        Val::List(list) => eval_list(list, env, ctx).await,
    }
}

async fn eval_list(list: Vec<Val>, env: EnvRef, ctx: Arc<LispCtx>) -> Result<Val> {
    let head = list[0].clone();
    let rest = list[1..].to_vec();

    // ── Special forms (head is a known symbol) ────────────────────────────────
    if let Val::Symbol(ref name) = head {
        match name.as_str() {
            // ── (quote datum) ─────────────────────────────────────────────────
            "quote" => {
                check_arity("quote", &rest, 1, 1)?;
                return Ok(rest.into_iter().next().unwrap());
            }

            // ── (quasiquote datum) ────────────────────────────────────────────
            "quasiquote" => {
                check_arity("quasiquote", &rest, 1, 1)?;
                return eval_quasiquote(rest.into_iter().next().unwrap(), env, ctx).await;
            }

            // ── (define name expr) or (define (name params) body) ─────────────
            "define" => {
                if rest.is_empty() {
                    bail!("define: missing name");
                }
                match &rest[0] {
                    Val::Symbol(name) => {
                        let val = if rest.len() > 1 {
                            eval(rest[1].clone(), env.clone(), ctx.clone()).await?
                        } else {
                            Val::Nil
                        };
                        Env::define(&env, name.clone(), val);
                        return Ok(Val::Nil);
                    }
                    Val::List(header) if !header.is_empty() => {
                        let fname = match &header[0] {
                            Val::Symbol(s) => s.clone(),
                            _ => bail!("define: function name must be a symbol"),
                        };
                        let typed_params = parse_typed_params_list(&header[1..])?;
                        let (typed_params, variadic) = split_variadic_typed(typed_params);
                        let (params, param_types): (Vec<_>, Vec<_>) =
                            typed_params.into_iter().unzip();
                        // Optional return-type annotation: (define (f ...) : ret body)
                        let (body_start, return_type) =
                            parse_return_type_annotation(&rest[1..])?;
                        let body = make_begin(rest[1 + body_start..].to_vec());
                        let lambda = Val::Lambda(Arc::new(Lambda {
                            params,
                            param_types,
                            variadic,
                            body: Box::new(body),
                            env: env.clone(),
                            return_type,
                        }));
                        Env::define(&env, fname, lambda);
                        return Ok(Val::Nil);
                    }
                    other => bail!("define: expected name or (name params...), got {other}"),
                }
            }

            // ── (set! name expr) ──────────────────────────────────────────────
            "set!" => {
                check_arity("set!", &rest, 2, 2)?;
                let name = match &rest[0] {
                    Val::Symbol(s) => s.clone(),
                    other => bail!("set!: expected symbol, got {other}"),
                };
                let val = eval(rest[1].clone(), env.clone(), ctx).await?;
                if !Env::set_existing(&env, &name, val) {
                    bail!("set!: undefined variable '{name}'");
                }
                return Ok(Val::Nil);
            }

            // ── (lambda (params...) body...) ──────────────────────────────────
            "lambda" => {
                if rest.len() < 2 {
                    bail!("lambda: missing params or body");
                }
                let typed_params = parse_typed_params(&rest[0])?;
                let (typed_params, variadic) = split_variadic_typed(typed_params);
                let (params, param_types): (Vec<_>, Vec<_>) =
                    typed_params.into_iter().unzip();
                let (body_start, return_type) =
                    parse_return_type_annotation(&rest[1..])?;
                let body = make_begin(rest[1 + body_start..].to_vec());
                return Ok(Val::Lambda(Arc::new(Lambda {
                    params,
                    param_types,
                    variadic,
                    body: Box::new(body),
                    env,
                    return_type,
                })));
            }

            // ── (if cond then [else]) ─────────────────────────────────────────
            "if" => {
                if rest.len() < 2 || rest.len() > 3 {
                    bail!("if: requires 2-3 sub-expressions");
                }
                let cond = eval(rest[0].clone(), env.clone(), ctx.clone()).await?;
                return if cond.is_truthy() {
                    eval(rest[1].clone(), env, ctx).await
                } else if rest.len() == 3 {
                    eval(rest[2].clone(), env, ctx).await
                } else {
                    Ok(Val::Nil)
                };
            }

            // ── (cond (test expr)...) ─────────────────────────────────────────
            "cond" => {
                for clause in &rest {
                    match clause {
                        Val::List(parts) if !parts.is_empty() => {
                            let test = match &parts[0] {
                                Val::Symbol(s) if s == "else" => Val::Bool(true),
                                t => eval(t.clone(), env.clone(), ctx.clone()).await?,
                            };
                            if test.is_truthy() {
                                return if parts.len() == 1 {
                                    Ok(test)
                                } else {
                                    eval(make_begin(parts[1..].to_vec()), env, ctx).await
                                };
                            }
                        }
                        _ => bail!("cond: malformed clause"),
                    }
                }
                return Ok(Val::Nil);
            }

            // ── (and expr...) ─────────────────────────────────────────────────
            "and" => {
                if rest.is_empty() {
                    return Ok(Val::Bool(true));
                }
                let mut last = Val::Bool(true);
                for e in &rest {
                    last = eval(e.clone(), env.clone(), ctx.clone()).await?;
                    if !last.is_truthy() {
                        return Ok(Val::Bool(false));
                    }
                }
                return Ok(last);
            }

            // ── (or expr...) ──────────────────────────────────────────────────
            "or" => {
                for e in &rest {
                    let v = eval(e.clone(), env.clone(), ctx.clone()).await?;
                    if v.is_truthy() {
                        return Ok(v);
                    }
                }
                return Ok(Val::Bool(false));
            }

            // ── (the type expr) — runtime type assertion ──────────────────────
            "the" => {
                check_arity("the", &rest, 2, 2)?;
                let ty = Type::from_val(&rest[0])?;
                let val = eval(rest[1].clone(), env, ctx).await?;
                if !ty.check(&val) {
                    bail!(
                        "type error: expected {ty}, got {} (value: {val})",
                        val.type_name()
                    );
                }
                return Ok(val);
            }

            // ── (when test body...) ───────────────────────────────────────────
            "when" => {
                if rest.is_empty() { bail!("when: missing test"); }
                let cond = eval(rest[0].clone(), env.clone(), ctx.clone()).await?;
                return if cond.is_truthy() {
                    eval(make_begin(rest[1..].to_vec()), env, ctx).await
                } else {
                    Ok(Val::Nil)
                };
            }

            // ── (unless test body...) ─────────────────────────────────────────
            "unless" => {
                if rest.is_empty() { bail!("unless: missing test"); }
                let cond = eval(rest[0].clone(), env.clone(), ctx.clone()).await?;
                return if !cond.is_truthy() {
                    eval(make_begin(rest[1..].to_vec()), env, ctx).await
                } else {
                    Ok(Val::Nil)
                };
            }

            // ── (begin expr...) ───────────────────────────────────────────────
            "begin" | "do" => {
                if rest.is_empty() {
                    return Ok(Val::Nil);
                }
                let mut last = Val::Nil;
                for e in &rest {
                    last = eval(e.clone(), env.clone(), ctx.clone()).await?;
                }
                return Ok(last);
            }

            // ── (let ((x v)...) body...) ──────────────────────────────────────
            "let" => {
                check_arity("let", &rest, 2, usize::MAX)?;
                let bindings = parse_bindings(&rest[0])?;
                let child = Env::new_child(env.clone());
                for (name, val_expr) in bindings {
                    let val = eval(val_expr, env.clone(), ctx.clone()).await?;
                    Env::define(&child, name, val);
                }
                return eval(make_begin(rest[1..].to_vec()), child, ctx).await;
            }

            // ── (let* ((x v)...) body...) ─────────────────────────────────────
            "let*" => {
                check_arity("let*", &rest, 2, usize::MAX)?;
                let bindings = parse_bindings(&rest[0])?;
                let mut cur_env = env.clone();
                for (name, val_expr) in bindings {
                    let child = Env::new_child(cur_env.clone());
                    let val = eval(val_expr, cur_env, ctx.clone()).await?;
                    Env::define(&child, name, val);
                    cur_env = child;
                }
                return eval(make_begin(rest[1..].to_vec()), cur_env, ctx).await;
            }

            // ── (letrec ((x v)...) body...) ──────────────────────────────────
            "letrec" => {
                check_arity("letrec", &rest, 2, usize::MAX)?;
                let bindings = parse_bindings(&rest[0])?;
                let child = Env::new_child(env);
                for (name, _) in &bindings {
                    Env::define(&child, name.clone(), Val::Nil);
                }
                for (name, val_expr) in bindings {
                    let val = eval(val_expr, child.clone(), ctx.clone()).await?;
                    Env::set_existing(&child, &name, val);
                }
                return eval(make_begin(rest[1..].to_vec()), child, ctx).await;
            }

            // ── (map fn list) ─────────────────────────────────────────────────
            "map" => {
                check_arity("map", &rest, 2, 2)?;
                let func = eval(rest[0].clone(), env.clone(), ctx.clone()).await?;
                let items = eval(rest[1].clone(), env.clone(), ctx.clone()).await?;
                let mut out = Vec::new();
                for item in items.as_list()?.to_vec() {
                    out.push(apply(func.clone(), vec![item], ctx.clone()).await?);
                }
                return Ok(Val::List(out));
            }

            // ── (for-each fn list) ────────────────────────────────────────────
            "for-each" => {
                check_arity("for-each", &rest, 2, 2)?;
                let func = eval(rest[0].clone(), env.clone(), ctx.clone()).await?;
                let items = eval(rest[1].clone(), env.clone(), ctx.clone()).await?;
                for item in items.as_list()?.to_vec() {
                    apply(func.clone(), vec![item], ctx.clone()).await?;
                }
                return Ok(Val::Nil);
            }

            // ── (apply fn args-list) ──────────────────────────────────────────
            "apply" => {
                check_arity("apply", &rest, 2, 2)?;
                let func = eval(rest[0].clone(), env.clone(), ctx.clone()).await?;
                let args_val = eval(rest[1].clone(), env.clone(), ctx.clone()).await?;
                let args = args_val.as_list()?.to_vec();
                return apply(func, args, ctx).await;
            }

            // ── SSH special forms ─────────────────────────────────────────────

            // (ssh-connect host port user password)
            "ssh-connect" => {
                check_arity("ssh-connect", &rest, 4, 4)?;
                let host = eval_str(rest[0].clone(), env.clone(), ctx.clone()).await?;
                let port = eval(rest[1].clone(), env.clone(), ctx.clone()).await?.as_int()? as u16;
                let user = eval_str(rest[2].clone(), env.clone(), ctx.clone()).await?;
                let pass = eval_str(rest[3].clone(), env.clone(), ctx.clone()).await?;
                let session = crate::ssh::client::SshSession::connect_password(
                    &host, port, &user, &pass,
                ).await?;
                let id = ctx.ssh_sessions.insert(session).await;
                return Ok(Val::SshSession(id));
            }

            // (ssh-auth-key host port user private-key-bytes)
            "ssh-auth-key" => {
                check_arity("ssh-auth-key", &rest, 4, 4)?;
                let host = eval_str(rest[0].clone(), env.clone(), ctx.clone()).await?;
                let port = eval(rest[1].clone(), env.clone(), ctx.clone()).await?.as_int()? as u16;
                let user = eval_str(rest[2].clone(), env.clone(), ctx.clone()).await?;
                let key_val = eval(rest[3].clone(), env.clone(), ctx.clone()).await?;
                let key_bytes = key_val.as_bytes()?.to_vec();
                let session = crate::ssh::client::SshSession::connect_key(
                    &host, port, &user, &key_bytes,
                ).await?;
                let id = ctx.ssh_sessions.insert(session).await;
                return Ok(Val::SshSession(id));
            }

            // (ssh-exec session cmd) → stdout string
            "ssh-exec" => {
                check_arity("ssh-exec", &rest, 2, 2)?;
                let id = eval(rest[0].clone(), env.clone(), ctx.clone()).await?.as_ssh_id()?;
                let cmd = eval_str(rest[1].clone(), env.clone(), ctx.clone()).await?;
                let (stdout, stderr, code) = ctx.ssh_sessions.exec(id, &cmd).await?;
                return Ok(Val::Str(if code != 0 && !stderr.is_empty() {
                    format!("{stdout}{stderr}")
                } else {
                    stdout
                }));
            }

            // (ssh-exec/all session cmd) → (stdout stderr exit-code)
            "ssh-exec/all" => {
                check_arity("ssh-exec/all", &rest, 2, 2)?;
                let id = eval(rest[0].clone(), env.clone(), ctx.clone()).await?.as_ssh_id()?;
                let cmd = eval_str(rest[1].clone(), env.clone(), ctx.clone()).await?;
                let (stdout, stderr, code) = ctx.ssh_sessions.exec(id, &cmd).await?;
                return Ok(Val::List(vec![
                    Val::Str(stdout),
                    Val::Str(stderr),
                    Val::Int(code as i64),
                ]));
            }

            // (ssh-read-file session path) → bytes
            "ssh-read-file" => {
                check_arity("ssh-read-file", &rest, 2, 2)?;
                let id = eval(rest[0].clone(), env.clone(), ctx.clone()).await?.as_ssh_id()?;
                let path = eval_str(rest[1].clone(), env.clone(), ctx.clone()).await?;
                let bytes = ctx.ssh_sessions.read_file(id, &path).await?;
                return Ok(Val::Bytes(bytes));
            }

            // (ssh-write-file session path bytes-or-string)
            "ssh-write-file" => {
                check_arity("ssh-write-file", &rest, 3, 3)?;
                let id = eval(rest[0].clone(), env.clone(), ctx.clone()).await?.as_ssh_id()?;
                let path = eval_str(rest[1].clone(), env.clone(), ctx.clone()).await?;
                let content = eval(rest[2].clone(), env.clone(), ctx.clone()).await?;
                let bytes = content.as_bytes()?.to_vec();
                ctx.ssh_sessions.write_file(id, &path, bytes).await?;
                return Ok(Val::Nil);
            }

            // (ssh-info session) → "user@host"
            "ssh-info" => {
                check_arity("ssh-info", &rest, 1, 1)?;
                let id = eval(rest[0].clone(), env.clone(), ctx.clone()).await?.as_ssh_id()?;
                return Ok(Val::Str(ctx.ssh_sessions.info(id).await?));
            }

            // (ssh-close session)
            "ssh-close" => {
                check_arity("ssh-close", &rest, 1, 1)?;
                let id = eval(rest[0].clone(), env.clone(), ctx.clone()).await?.as_ssh_id()?;
                if let Some(session) = ctx.ssh_sessions.remove(id).await {
                    let _ = session.close().await;
                }
                return Ok(Val::Nil);
            }

            // Not a recognised special form — fall through to function application.
            _ => {}
        }
    }

    // ── Function application ──────────────────────────────────────────────────
    let func = eval(head, env.clone(), ctx.clone()).await?;
    let mut args = Vec::with_capacity(rest.len());
    for arg in rest {
        args.push(eval(arg, env.clone(), ctx.clone()).await?);
    }
    apply(func, args, ctx).await
}

// ── Application ───────────────────────────────────────────────────────────────

async fn apply(func: Val, args: Vec<Val>, ctx: Arc<LispCtx>) -> Result<Val> {
    match func {
        Val::Native(f) => (f.f)(&args),
        Val::Lambda(lambda) => {
            let call_env = Env::new_child(lambda.env.clone());
            bind_args(&lambda, args, &call_env)?;
            let result = eval((*lambda.body).clone(), call_env, ctx).await?;
            if let Some(ref ret_ty) = lambda.return_type {
                if !ret_ty.check(&result) {
                    bail!(
                        "return type error: expected {ret_ty}, got {} (value: {result})",
                        result.type_name()
                    );
                }
            }
            Ok(result)
        }
        other => bail!("not a procedure: {other}"),
    }
}

fn bind_args(lambda: &Lambda, mut args: Vec<Val>, env: &EnvRef) -> Result<()> {
    let n_required = if lambda.variadic {
        lambda.params.len() - 1
    } else {
        lambda.params.len()
    };

    if lambda.variadic {
        if args.len() < n_required {
            bail!(
                "procedure expects at least {} args, got {}",
                n_required,
                args.len()
            );
        }
    } else if args.len() != lambda.params.len() {
        bail!(
            "procedure expects {} args, got {}",
            lambda.params.len(),
            args.len()
        );
    }

    let rest_args = if lambda.variadic {
        args.split_off(n_required)
    } else {
        vec![]
    };

    for (i, (name, val)) in lambda.params[..n_required].iter().zip(args).enumerate() {
        if let Some(Some(ref ty)) = lambda.param_types.get(i) {
            if !ty.check(&val) {
                bail!(
                    "type error for param '{name}': expected {ty}, got {} (value: {val})",
                    val.type_name()
                );
            }
        }
        Env::define(env, name.clone(), val);
    }

    if lambda.variadic {
        let rest_param = lambda.params.last().unwrap();
        let rest_val = if rest_args.is_empty() { Val::Nil } else { Val::List(rest_args) };
        Env::define(env, rest_param.clone(), rest_val);
    }

    Ok(())
}

// ── Quasiquote ────────────────────────────────────────────────────────────────

fn eval_quasiquote(
    expr: Val,
    env: EnvRef,
    ctx: Arc<LispCtx>,
) -> BoxFuture<'static, Result<Val>> {
    Box::pin(async move {
        match expr {
            Val::List(items) => {
                let mut out = Vec::new();
                for item in items {
                    match &item {
                        Val::List(inner)
                            if inner.first() == Some(&Val::Symbol("unquote".to_string())) =>
                        {
                            if inner.len() != 2 { bail!("unquote requires 1 sub-expression"); }
                            out.push(eval(inner[1].clone(), env.clone(), ctx.clone()).await?);
                        }
                        Val::List(inner)
                            if inner.first()
                                == Some(&Val::Symbol("unquote-splicing".to_string())) =>
                        {
                            if inner.len() != 2 { bail!("unquote-splicing requires 1 sub-expression"); }
                            match eval(inner[1].clone(), env.clone(), ctx.clone()).await? {
                                Val::List(vs) => out.extend(vs),
                                Val::Nil => {}
                                other => bail!("unquote-splicing: expected list, got {}", other.type_name()),
                            }
                        }
                        other => {
                            out.push(eval_quasiquote(other.clone(), env.clone(), ctx.clone()).await?);
                        }
                    }
                }
                Ok(if out.is_empty() { Val::Nil } else { Val::List(out) })
            }
            other => Ok(other),
        }
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn check_arity(name: &str, args: &[Val], min: usize, max: usize) -> Result<()> {
    if args.len() < min || args.len() > max {
        if min == max {
            bail!("{name}: requires exactly {min} args, got {}", args.len());
        } else {
            bail!("{name}: requires {min}–{max} args, got {}", args.len());
        }
    }
    Ok(())
}

/// Parse a param from a single `Val`: either a bare symbol `x` or an
/// annotated form `(x : type)`.
fn parse_one_typed_param(p: &Val) -> Result<(String, Option<Type>)> {
    match p {
        Val::Symbol(s) => Ok((s.clone(), None)),
        Val::List(parts)
            if parts.len() == 3
                && parts[1] == Val::Symbol(":".to_string()) =>
        {
            let name = match &parts[0] {
                Val::Symbol(s) => s.clone(),
                _ => bail!("lambda: param name must be a symbol"),
            };
            let ty = Type::from_val(&parts[2])?;
            Ok((name, Some(ty)))
        }
        _ => bail!("lambda: parameter must be a symbol or (name : type)"),
    }
}

/// Parse a param-list `Val` (list, nil, or bare symbol).
fn parse_typed_params(expr: &Val) -> Result<Vec<(String, Option<Type>)>> {
    match expr {
        Val::List(params) => params.iter().map(parse_one_typed_param).collect(),
        Val::Nil => Ok(vec![]),
        Val::Symbol(s) => Ok(vec![(s.clone(), None)]),
        _ => bail!("lambda: params must be a list or symbol"),
    }
}

/// Parse typed params from a slice of `Val`s (used for the `define` shorthand
/// where params are already extracted from the header list).
fn parse_typed_params_list(params: &[Val]) -> Result<Vec<(String, Option<Type>)>> {
    params.iter().map(parse_one_typed_param).collect()
}

fn split_variadic_typed(
    mut params: Vec<(String, Option<Type>)>,
) -> (Vec<(String, Option<Type>)>, bool) {
    if params
        .last()
        .map(|(s, _)| s.starts_with('&'))
        .unwrap_or(false)
    {
        let (name, ty) = params.pop().unwrap();
        params.push((name.trim_start_matches('&').to_string(), ty));
        (params, true)
    } else {
        (params, false)
    }
}

/// Check for an optional `: return-type` prefix in a body slice.
///
/// Returns `(skip, Option<Type>)` where `skip` is the number of elements
/// consumed (0 or 2).
fn parse_return_type_annotation(body: &[Val]) -> Result<(usize, Option<Type>)> {
    if body.first() == Some(&Val::Symbol(":".to_string())) {
        let ty_val = body
            .get(1)
            .ok_or_else(|| anyhow::anyhow!("expected type after ':'"))?;
        Ok((2, Some(Type::from_val(ty_val)?)))
    } else {
        Ok((0, None))
    }
}

fn parse_bindings(expr: &Val) -> Result<Vec<(String, Val)>> {
    match expr {
        Val::List(pairs) => pairs
            .iter()
            .map(|pair| match pair {
                Val::List(kv) if kv.len() == 2 => {
                    let name = match &kv[0] {
                        Val::Symbol(s) => s.clone(),
                        _ => bail!("let: binding name must be a symbol"),
                    };
                    Ok((name, kv[1].clone()))
                }
                _ => bail!("let: each binding must be (name expr)"),
            })
            .collect(),
        Val::Nil => Ok(vec![]),
        _ => bail!("let: bindings must be a list"),
    }
}

fn make_begin(exprs: Vec<Val>) -> Val {
    match exprs.len() {
        0 => Val::Nil,
        1 => exprs.into_iter().next().unwrap(),
        _ => {
            let mut list = vec![Val::Symbol("begin".to_string())];
            list.extend(exprs);
            Val::List(list)
        }
    }
}

async fn eval_str(expr: Val, env: EnvRef, ctx: Arc<LispCtx>) -> Result<String> {
    let v = eval(expr, env, ctx).await?;
    match v {
        Val::Str(s) => Ok(s),
        other => bail!("expected string, got {}", other.type_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::{env::Env, stdlib, LispCtx};

    fn ctx() -> Arc<LispCtx> {
        Arc::new(LispCtx::new())
    }

    fn make_env() -> EnvRef {
        let env = Env::new_root();
        stdlib::register_all(&env);
        env
    }

    async fn run(src: &str) -> Result<Val> {
        let ctx = ctx();
        let env = make_env();
        crate::lisp::run_in(src, env, ctx).await
    }

    #[tokio::test]
    async fn test_eval_literal_int() {
        assert_eq!(run("42").await.unwrap(), Val::Int(42));
    }

    #[tokio::test]
    async fn test_eval_add() {
        assert_eq!(run("(+ 1 2 3)").await.unwrap(), Val::Int(6));
    }

    #[tokio::test]
    async fn test_eval_nested_arithmetic() {
        assert_eq!(run("(* 2 (+ 3 4))").await.unwrap(), Val::Int(14));
    }

    #[tokio::test]
    async fn test_eval_define_and_use() {
        assert_eq!(run("(define x 10) (+ x 5)").await.unwrap(), Val::Int(15));
    }

    #[tokio::test]
    async fn test_eval_lambda_call() {
        assert_eq!(
            run("(define square (lambda (x) (* x x))) (square 7)").await.unwrap(),
            Val::Int(49)
        );
    }

    #[tokio::test]
    async fn test_eval_lambda_shorthand() {
        assert_eq!(
            run("(define (cube x) (* x x x)) (cube 3)").await.unwrap(),
            Val::Int(27)
        );
    }

    #[tokio::test]
    async fn test_eval_if_true_branch() {
        assert_eq!(run("(if #t 1 2)").await.unwrap(), Val::Int(1));
    }

    #[tokio::test]
    async fn test_eval_if_false_branch() {
        assert_eq!(run("(if #f 1 2)").await.unwrap(), Val::Int(2));
    }

    #[tokio::test]
    async fn test_eval_if_no_else_returns_nil() {
        assert_eq!(run("(if #f 1)").await.unwrap(), Val::Nil);
    }

    #[tokio::test]
    async fn test_eval_cond() {
        assert_eq!(
            run("(cond ((= 1 2) 10) ((= 2 2) 20) (else 30))").await.unwrap(),
            Val::Int(20)
        );
    }

    #[tokio::test]
    async fn test_eval_let() {
        assert_eq!(run("(let ((x 3) (y 4)) (+ x y))").await.unwrap(), Val::Int(7));
    }

    #[tokio::test]
    async fn test_eval_let_star() {
        assert_eq!(
            run("(let* ((x 2) (y (* x 3))) y)").await.unwrap(),
            Val::Int(6)
        );
    }

    #[tokio::test]
    async fn test_eval_letrec_mutual_recursion() {
        let result = run("
            (letrec ((even? (lambda (n)
                              (if (= n 0) #t (odd? (- n 1)))))
                     (odd?  (lambda (n)
                              (if (= n 0) #f (even? (- n 1))))))
              (even? 10))
        ").await.unwrap();
        assert_eq!(result, Val::Bool(true));
    }

    #[tokio::test]
    async fn test_eval_and_short_circuit() {
        assert_eq!(run("(and 1 2 #f 3)").await.unwrap(), Val::Bool(false));
        assert_eq!(run("(and 1 2 3)").await.unwrap(), Val::Int(3));
    }

    #[tokio::test]
    async fn test_eval_or_short_circuit() {
        assert_eq!(run("(or #f #f 42)").await.unwrap(), Val::Int(42));
        assert_eq!(run("(or #f #f)").await.unwrap(), Val::Bool(false));
    }

    #[tokio::test]
    async fn test_eval_begin_returns_last() {
        assert_eq!(run("(begin 1 2 3)").await.unwrap(), Val::Int(3));
    }

    #[tokio::test]
    async fn test_eval_quote() {
        assert_eq!(
            run("'(1 2 3)").await.unwrap(),
            Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3)])
        );
    }

    #[tokio::test]
    async fn test_eval_map() {
        let result = run("(map (lambda (x) (* x 2)) '(1 2 3))").await.unwrap();
        assert_eq!(result, Val::List(vec![Val::Int(2), Val::Int(4), Val::Int(6)]));
    }

    #[tokio::test]
    async fn test_eval_for_each_returns_nil() {
        let result = run("(for-each (lambda (x) x) '(1 2 3))").await.unwrap();
        assert_eq!(result, Val::Nil);
    }

    #[tokio::test]
    async fn test_eval_apply() {
        assert_eq!(run("(apply + '(1 2 3 4))").await.unwrap(), Val::Int(10));
    }

    #[tokio::test]
    async fn test_eval_quasiquote_unquote() {
        let result = run("(define x 42) `(the answer is ,x)").await.unwrap();
        assert_eq!(
            result,
            Val::List(vec![
                Val::Symbol("the".to_string()),
                Val::Symbol("answer".to_string()),
                Val::Symbol("is".to_string()),
                Val::Int(42),
            ])
        );
    }

    #[tokio::test]
    async fn test_eval_quasiquote_unquote_splicing() {
        let result = run("(define xs '(2 3)) `(1 ,@xs 4)").await.unwrap();
        assert_eq!(
            result,
            Val::List(vec![Val::Int(1), Val::Int(2), Val::Int(3), Val::Int(4)])
        );
    }

    #[tokio::test]
    async fn test_eval_variadic_lambda() {
        let result = run("
            (define (sum &args)
              (apply + args))
            (sum 1 2 3 4 5)
        ").await.unwrap();
        assert_eq!(result, Val::Int(15));
    }

    #[tokio::test]
    async fn test_eval_closure_captures_env() {
        let result = run("
            (define (make-adder n)
              (lambda (x) (+ x n)))
            (define add10 (make-adder 10))
            (add10 5)
        ").await.unwrap();
        assert_eq!(result, Val::Int(15));
    }

    #[tokio::test]
    async fn test_eval_set_bang() {
        let result = run("(define x 1) (set! x 99) x").await.unwrap();
        assert_eq!(result, Val::Int(99));
    }

    #[tokio::test]
    async fn test_eval_ssh_connect_fails_gracefully() {
        let err = run(r#"(ssh-connect "127.0.0.1" 1 "nobody" "nopass")"#).await;
        assert!(err.is_err());
    }

    // ── Typed annotations ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_typed_param_accepted() {
        let result = run("(define (double (x : int)) (* x 2)) (double 5)").await.unwrap();
        assert_eq!(result, Val::Int(10));
    }

    #[tokio::test]
    async fn test_typed_param_rejected() {
        let err = run(r#"(define (f (x : int)) x) (f "hello")"#).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("type error"));
    }

    #[tokio::test]
    async fn test_typed_return_accepted() {
        let result = run("(define (f x) : int (* x 2)) (f 3)").await.unwrap();
        assert_eq!(result, Val::Int(6));
    }

    #[tokio::test]
    async fn test_typed_return_rejected() {
        let err = run(r#"(define (f x) : int "oops") (f 1)"#).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("return type error"));
    }

    #[tokio::test]
    async fn test_the_passes() {
        let result = run("(the int 42)").await.unwrap();
        assert_eq!(result, Val::Int(42));
    }

    #[tokio::test]
    async fn test_the_fails() {
        let err = run(r#"(the int "not an int")"#).await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("type error"));
    }

    #[tokio::test]
    async fn test_untyped_params_still_work() {
        let result = run("(lambda (x y) (+ x y))").await.unwrap();
        assert!(matches!(result, Val::Lambda(_)));
    }

    #[tokio::test]
    async fn test_fn_type_annotation() {
        let result = run("(the (-> int int) (lambda ((x : int)) : int (* x x))) ").await.unwrap();
        assert!(matches!(result, Val::Lambda(_)));
    }
}
