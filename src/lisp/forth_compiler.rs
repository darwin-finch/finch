//! Compiler for the portable, first-order Lisp subset.
//!
//! Calls are lowered by post-order traversal: arguments first, operator last.
//! Lexical `let` bindings are represented by positions on the Forth data stack.

use super::types::Val;
use anyhow::{bail, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledLisp {
    pub forth_source: String,
}

pub fn compile_source(source: &str) -> Result<CompiledLisp> {
    let expressions = super::reader::parse_str(source)?;
    if expressions.is_empty() {
        return Ok(CompiledLisp {
            forth_source: String::new(),
        });
    }
    let mut compiler = Compiler::default();
    for (index, expression) in expressions.iter().enumerate() {
        compiler.compile(expression)?;
        if index + 1 != expressions.len() {
            compiler.emit("drop");
            compiler.depth = compiler.depth.saturating_sub(1);
        }
    }
    Ok(CompiledLisp {
        forth_source: compiler.output.join(" "),
    })
}

#[derive(Default)]
struct Compiler {
    output: Vec<String>,
    /// Values introduced by this compiled expression, excluding any pre-existing VM stack.
    depth: usize,
    /// Symbol -> stack slot relative to the expression's initial depth.
    locals: HashMap<String, usize>,
}

impl Compiler {
    fn emit(&mut self, word: impl Into<String>) {
        self.output.push(word.into());
    }

    fn compile(&mut self, expression: &Val) -> Result<()> {
        match expression {
            Val::Int(value) => {
                self.emit(value.to_string());
                self.depth += 1;
                Ok(())
            }
            Val::Bool(value) => {
                self.emit(if *value { "-1" } else { "0" });
                self.depth += 1;
                Ok(())
            }
            Val::Symbol(name) => self.compile_symbol(name),
            Val::List(items) if items.is_empty() => bail!("empty list is not portable yet"),
            Val::List(items) => self.compile_list(items),
            other => bail!("{} is not in the portable Lisp subset", other.type_name()),
        }
    }

    fn compile_symbol(&mut self, name: &str) -> Result<()> {
        let slot = self
            .locals
            .get(name)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("unbound portable local: {name}"))?;
        let distance = self
            .depth
            .checked_sub(slot + 1)
            .ok_or_else(|| anyhow::anyhow!("invalid stack position for local: {name}"))?;
        self.emit(distance.to_string());
        self.emit("pick");
        self.depth += 1;
        Ok(())
    }

    fn compile_list(&mut self, items: &[Val]) -> Result<()> {
        let operator = match &items[0] {
            Val::Symbol(operator) => operator.as_str(),
            _ => bail!("portable call target must be a symbol"),
        };
        match operator {
            "let" => self.compile_let(&items[1..]),
            "begin" => self.compile_begin(&items[1..]),
            "if" => self.compile_if(&items[1..]),
            "+" | "*" => self.compile_identity_fold(operator, &items[1..]),
            "-" => self.compile_subtract(&items[1..]),
            "=" | "<" | ">" | "<=" | ">=" | "max" | "min" => {
                self.compile_fixed(operator, &items[1..], 2)
            }
            "abs" | "sqrt" | "zero?" | "positive?" | "negative?" | "not" => {
                let forth = match operator {
                    "zero?" | "not" => "0=",
                    "positive?" => "0>",
                    "negative?" => "0<",
                    other => other,
                };
                self.compile_fixed(forth, &items[1..], 1)
            }
            other => bail!("unsupported portable Lisp operator: {other}"),
        }
    }

    fn compile_fixed(&mut self, operator: &str, args: &[Val], arity: usize) -> Result<()> {
        if args.len() != arity {
            bail!("{operator} requires {arity} arguments in portable Lisp");
        }
        for argument in args {
            self.compile(argument)?;
        }
        self.emit(operator);
        self.depth = self.depth - arity + 1;
        Ok(())
    }

    fn compile_identity_fold(&mut self, operator: &str, args: &[Val]) -> Result<()> {
        if args.is_empty() {
            self.emit(if operator == "+" { "0" } else { "1" });
            self.depth += 1;
            return Ok(());
        }
        self.compile(&args[0])?;
        for argument in &args[1..] {
            self.compile(argument)?;
            self.emit(operator);
            self.depth -= 1;
        }
        Ok(())
    }

    fn compile_subtract(&mut self, args: &[Val]) -> Result<()> {
        let Some(first) = args.first() else {
            bail!("- requires at least one argument");
        };
        self.compile(first)?;
        if args.len() == 1 {
            self.emit("negate");
            return Ok(());
        }
        for argument in &args[1..] {
            self.compile(argument)?;
            self.emit("-");
            self.depth -= 1;
        }
        Ok(())
    }

    fn compile_begin(&mut self, expressions: &[Val]) -> Result<()> {
        let Some((last, preceding)) = expressions.split_last() else {
            bail!("begin requires at least one expression");
        };
        for expression in preceding {
            self.compile(expression)?;
            self.emit("drop");
            self.depth -= 1;
        }
        self.compile(last)
    }

    fn compile_if(&mut self, expressions: &[Val]) -> Result<()> {
        if expressions.len() != 3 {
            bail!("portable if requires condition, then, and else expressions");
        }
        self.compile(&expressions[0])?;
        self.emit("if");
        self.depth -= 1;
        let branch_depth = self.depth;
        self.compile(&expressions[1])?;
        let then_depth = self.depth;
        self.emit("else");
        self.depth = branch_depth;
        self.compile(&expressions[2])?;
        if self.depth != then_depth {
            bail!("portable if branches must leave the same number of values");
        }
        self.emit("then");
        Ok(())
    }

    fn compile_let(&mut self, expressions: &[Val]) -> Result<()> {
        if expressions.len() < 2 {
            bail!("let requires bindings and a body");
        }
        let bindings = match &expressions[0] {
            Val::List(bindings) => bindings,
            _ => bail!("let bindings must be a list"),
        };
        let outer_locals = self.locals.clone();
        let base_depth = self.depth;
        let mut names = Vec::with_capacity(bindings.len());

        // Scheme `let` initializers see only the outer lexical environment.
        for binding in bindings {
            let pair = match binding {
                Val::List(pair) if pair.len() == 2 => pair,
                _ => bail!("each let binding must be (name value)"),
            };
            let name = match &pair[0] {
                Val::Symbol(name) => name.clone(),
                _ => bail!("let binding name must be a symbol"),
            };
            self.locals = outer_locals.clone();
            self.compile(&pair[1])?;
            names.push(name);
        }
        self.locals = outer_locals.clone();
        for (index, name) in names.iter().enumerate() {
            self.locals.insert(name.clone(), base_depth + index);
        }
        self.compile_begin(&expressions[1..])?;

        // Preserve the result while removing lexical slots beneath it.
        self.emit(">r");
        for _ in bindings {
            self.emit("drop");
        }
        self.emit("r>");
        self.depth = base_depth + 1;
        self.locals = outer_locals;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_nested_calls_by_postorder_traversal() {
        let compiled = compile_source("(+ 3 (* 4 2))").unwrap();
        assert_eq!(compiled.forth_source, "3 4 2 * +");
    }

    #[test]
    fn compiles_left_associative_subtraction() {
        let compiled = compile_source("(- 10 3 2)").unwrap();
        assert_eq!(compiled.forth_source, "10 3 - 2 -");
    }

    #[test]
    fn lowers_let_bindings_to_stack_positions() {
        let compiled = compile_source("(let ((a 10) (b 5)) (- a b))").unwrap();
        assert_eq!(
            compiled.forth_source,
            "10 5 1 pick 1 pick - >r drop drop r>"
        );
        assert_eq!(
            crate::coforth::Forth::run(&compiled.forth_source).unwrap(),
            ""
        );
        let mut vm = crate::coforth::Forth::new();
        vm.exec(&compiled.forth_source).unwrap();
        assert_eq!(vm.data_stack(), &[5]);
    }

    #[test]
    fn rejects_closures_instead_of_changing_semantics() {
        let error = compile_source("((lambda (x) (+ x 1)) 2)").unwrap_err();
        assert!(error.to_string().contains("call target"));
    }
}
