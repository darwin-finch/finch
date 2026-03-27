/// Lexical environment — a linked chain of scopes.
///
/// `Arc<Mutex<Env>>` lets closures capture the env by clone (cheap Arc bump)
/// and eval traverse it from multiple tasks without race conditions.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::types::Val;

pub type EnvRef = Arc<Mutex<Env>>;

#[derive(Debug)]
pub struct Env {
    bindings: HashMap<String, Val>,
    parent: Option<EnvRef>,
}

impl Env {
    /// Create an empty root environment.
    pub fn new_root() -> EnvRef {
        Arc::new(Mutex::new(Env {
            bindings: HashMap::new(),
            parent: None,
        }))
    }

    /// Create a child scope parented to `parent`.
    pub fn new_child(parent: EnvRef) -> EnvRef {
        Arc::new(Mutex::new(Env {
            bindings: HashMap::new(),
            parent: Some(parent),
        }))
    }

    /// Look up `name` in this scope and all ancestors.
    pub fn get(env: &EnvRef, name: &str) -> Option<Val> {
        let guard = env.lock().unwrap();
        if let Some(val) = guard.bindings.get(name) {
            return Some(val.clone());
        }
        if let Some(ref parent) = guard.parent {
            let parent = parent.clone();
            drop(guard);
            return Env::get(&parent, name);
        }
        None
    }

    /// Bind `name` in this scope (shadows any parent binding).
    pub fn define(env: &EnvRef, name: String, val: Val) {
        env.lock().unwrap().bindings.insert(name, val);
    }

    /// Mutate the nearest binding of `name` up the chain.
    /// Returns false if `name` is not found anywhere.
    pub fn set_existing(env: &EnvRef, name: &str, val: Val) -> bool {
        let mut guard = env.lock().unwrap();
        if guard.bindings.contains_key(name) {
            guard.bindings.insert(name.to_string(), val);
            return true;
        }
        if let Some(ref parent) = guard.parent {
            let parent = parent.clone();
            drop(guard);
            return Env::set_existing(&parent, name, val);
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lisp::types::Val;

    #[test]
    fn test_env_define_and_get() {
        let env = Env::new_root();
        Env::define(&env, "x".to_string(), Val::Int(42));
        assert_eq!(Env::get(&env, "x"), Some(Val::Int(42)));
    }

    #[test]
    fn test_env_child_inherits_parent() {
        let parent = Env::new_root();
        Env::define(&parent, "x".to_string(), Val::Int(1));
        let child = Env::new_child(parent);
        assert_eq!(Env::get(&child, "x"), Some(Val::Int(1)));
    }

    #[test]
    fn test_env_child_shadows_parent() {
        let parent = Env::new_root();
        Env::define(&parent, "x".to_string(), Val::Int(1));
        let child = Env::new_child(parent);
        Env::define(&child, "x".to_string(), Val::Int(2));
        assert_eq!(Env::get(&child, "x"), Some(Val::Int(2)));
    }

    #[test]
    fn test_env_missing_returns_none() {
        let env = Env::new_root();
        assert_eq!(Env::get(&env, "undefined"), None);
    }

    #[test]
    fn test_env_set_existing_mutates_ancestor() {
        let parent = Env::new_root();
        Env::define(&parent, "x".to_string(), Val::Int(1));
        let child = Env::new_child(parent.clone());
        let found = Env::set_existing(&child, "x", Val::Int(99));
        assert!(found);
        assert_eq!(Env::get(&parent, "x"), Some(Val::Int(99)));
    }

    #[test]
    fn test_env_set_existing_fails_when_not_found() {
        let env = Env::new_root();
        assert!(!Env::set_existing(&env, "nope", Val::Nil));
    }
}
