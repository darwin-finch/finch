use super::effects::FileSelector;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A language-level type shared by Co-Forth and Finch Lisp.
///
/// This is independent of the physical stack representation. A JIT may keep
/// primitives in registers while the interpreter uses tagged boundary values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "parameters", rename_all = "snake_case")]
pub enum Type {
    Unit,
    Bool,
    Int,
    UInt,
    Float,
    Char,
    Symbol,
    String,
    Bytes,
    /// Managed JSON data, retained structurally so field access never needs
    /// to parse or interpolate an untyped string at an effect boundary.
    Json,
    Path(FileSelector),
    List(Box<Type>),
    Map(Box<Type>, Box<Type>),
    Option(Box<Type>),
    Result(Box<Type>, Box<Type>),
    Record(Vec<(String, Type)>),
    Variant(Vec<(String, Option<Type>)>),
    Function {
        arguments: Vec<Type>,
        result: Box<Type>,
        effects: super::effects::EffectSet,
    },
    Task(Box<Type>),
    /// An opaque, scheduler/host-owned lazy sequence. Advancing it is
    /// bounded and returns `option<T>`; source programs cannot manufacture a
    /// cursor from a string or share its backing state implicitly.
    Stream(Box<Type>),
    Resource(String),
    Capability(String),
    Variable(String),
    Dynamic,
}

impl Type {
    pub fn list(element: Type) -> Self {
        Self::List(Box::new(element))
    }

    pub fn result(ok: Type, error: Type) -> Self {
        Self::Result(Box::new(ok), Box::new(error))
    }

    /// Non-binding compatibility. Type variables are bound by the verifier.
    pub fn accepts(&self, actual: &Type) -> bool {
        self == actual
            || matches!(self, Self::Dynamic | Self::Variable(_))
            || matches!(actual, Self::Dynamic)
            || matches!((self, actual), (Self::Float, Self::Int | Self::UInt))
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unit => f.write_str("unit"),
            Self::Bool => f.write_str("bool"),
            Self::Int => f.write_str("int"),
            Self::UInt => f.write_str("uint"),
            Self::Float => f.write_str("float"),
            Self::Char => f.write_str("char"),
            Self::Symbol => f.write_str("symbol"),
            Self::String => f.write_str("string"),
            Self::Bytes => f.write_str("bytes"),
            Self::Json => f.write_str("json"),
            Self::Path(selector) => write!(f, "path<{selector}>"),
            Self::List(element) => write!(f, "list<{element}>"),
            Self::Map(key, value) => write!(f, "map<{key},{value}>"),
            Self::Option(inner) => write!(f, "option<{inner}>"),
            Self::Result(ok, error) => write!(f, "result<{ok},{error}>"),
            Self::Record(fields) => {
                f.write_str("record{")?;
                for (index, (name, ty)) in fields.iter().enumerate() {
                    if index != 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{name}:{ty}")?;
                }
                f.write_str("}")
            }
            Self::Variant(variants) => {
                f.write_str("variant{")?;
                for (index, (name, payload)) in variants.iter().enumerate() {
                    if index != 0 {
                        f.write_str("|")?;
                    }
                    f.write_str(name)?;
                    if let Some(payload) = payload {
                        write!(f, "({payload})")?;
                    }
                }
                f.write_str("}")
            }
            Self::Function {
                arguments,
                result,
                effects,
            } => {
                f.write_str("fn(")?;
                for (index, argument) in arguments.iter().enumerate() {
                    if index != 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{argument}")?;
                }
                write!(f, ")->{result}!{effects}")
            }
            Self::Task(result) => write!(f, "task<{result}>"),
            Self::Stream(element) => write!(f, "stream<{element}>"),
            Self::Resource(kind) => write!(f, "resource<{kind}>"),
            Self::Capability(kind) => write!(f, "capability<{kind}>"),
            Self::Variable(name) => f.write_str(name),
            Self::Dynamic => f.write_str("dynamic"),
        }
    }
}

/// Portable typed value used at VM, task, suspension, and wire boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// A separately orchestrated provider/agent ProgramRun.
    #[default]
    Agent,
    /// A bounded pure closure executing on the local CPU-fiber pool.
    CpuFiber,
}

/// Portable typed value used at VM, task, suspension, and wire boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum TypedValue {
    Unit,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Float(f64),
    Char(char),
    Symbol(String),
    String(String),
    Bytes(Vec<u8>),
    /// A managed JSON tree. Programs may only inspect it through typed JSON
    /// words such as `json-get`; host authority never lives in JSON text.
    Json(serde_json::Value),
    Path {
        selector: FileSelector,
        relative: String,
    },
    List {
        element_type: Type,
        values: Vec<TypedValue>,
    },
    /// An immutable, insertion-ordered typed map. Keys are compared by their
    /// full typed value; source construction and `map-set` reject duplicate
    /// keys by replacing the previous value rather than silently widening the
    /// value type.
    Map {
        key_type: Type,
        value_type: Type,
        entries: Vec<(TypedValue, TypedValue)>,
    },
    Option {
        inner_type: Type,
        value: Option<Box<TypedValue>>,
    },
    Result {
        ok_type: Type,
        error_type: Type,
        is_ok: bool,
        value: Box<TypedValue>,
    },
    Record(Vec<(String, TypedValue)>),
    Variant {
        name: String,
        value: Option<Box<TypedValue>>,
    },
    Closure {
        function: String,
        captures: Vec<TypedValue>,
        signature: super::signature::StackSignature,
    },
    /// A daemon-owned, serializable task reference. The id identifies the
    /// durable task record; the result type lets a later turn inspect or join
    /// it without falling back to `dynamic`.
    Task {
        id: String,
        result_type: Type,
        #[serde(default)]
        kind: TaskKind,
    },
    /// A host- or scheduler-owned lazy sequence. `kind` is checked by the
    /// host adapter; its opaque ID is not a file path or capability token.
    Stream {
        id: String,
        element_type: Type,
        kind: String,
        generation: u64,
    },
    Resource {
        kind: String,
        handle: String,
        generation: u64,
    },
    Dynamic {
        runtime_type: Type,
        value: Box<TypedValue>,
    },
}

impl TypedValue {
    pub fn value_type(&self) -> Type {
        match self {
            Self::Unit => Type::Unit,
            Self::Bool(_) => Type::Bool,
            Self::Int(_) => Type::Int,
            Self::UInt(_) => Type::UInt,
            Self::Float(_) => Type::Float,
            Self::Char(_) => Type::Char,
            Self::Symbol(_) => Type::Symbol,
            Self::String(_) => Type::String,
            Self::Bytes(_) => Type::Bytes,
            Self::Json(_) => Type::Json,
            Self::Path { selector, .. } => Type::Path(selector.clone()),
            Self::List { element_type, .. } => Type::list(element_type.clone()),
            Self::Map {
                key_type,
                value_type,
                ..
            } => Type::Map(Box::new(key_type.clone()), Box::new(value_type.clone())),
            Self::Option { inner_type, .. } => Type::Option(Box::new(inner_type.clone())),
            Self::Result {
                ok_type,
                error_type,
                ..
            } => Type::Result(Box::new(ok_type.clone()), Box::new(error_type.clone())),
            Self::Record(fields) => Type::Record(
                fields
                    .iter()
                    .map(|(name, value)| (name.clone(), value.value_type()))
                    .collect(),
            ),
            Self::Variant { name, value } => Type::Variant(vec![(
                name.clone(),
                value.as_ref().map(|value| value.value_type()),
            )]),
            Self::Closure { signature, .. } => Type::Function {
                arguments: signature.input.values.clone(),
                result: Box::new(
                    signature
                        .output
                        .values
                        .last()
                        .cloned()
                        .unwrap_or(Type::Unit),
                ),
                effects: signature.effects.clone(),
            },
            Self::Task { result_type, .. } => Type::Task(Box::new(result_type.clone())),
            Self::Stream { element_type, .. } => Type::Stream(Box::new(element_type.clone())),
            Self::Resource { kind, .. } => Type::Resource(kind.clone()),
            Self::Dynamic { .. } => Type::Dynamic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_values_report_language_types() {
        assert_eq!(TypedValue::Int(3).value_type(), Type::Int);
        assert_eq!(
            TypedValue::String("hello".into()).value_type(),
            Type::String
        );
        assert_eq!(
            TypedValue::Option {
                inner_type: Type::Int,
                value: Some(Box::new(TypedValue::Int(7))),
            }
            .value_type(),
            Type::Option(Box::new(Type::Int))
        );
        assert_eq!(
            TypedValue::Result {
                ok_type: Type::Int,
                error_type: Type::String,
                is_ok: false,
                value: Box::new(TypedValue::String("bad".into())),
            }
            .value_type(),
            Type::Result(Box::new(Type::Int), Box::new(Type::String))
        );
        assert_eq!(
            TypedValue::Task {
                id: "task-1".into(),
                result_type: Type::String,
                kind: TaskKind::Agent,
            }
            .value_type(),
            Type::Task(Box::new(Type::String))
        );
        assert_eq!(
            TypedValue::Stream {
                id: "stream-1".into(),
                element_type: Type::String,
                kind: "file-lines".into(),
                generation: 0,
            }
            .value_type(),
            Type::Stream(Box::new(Type::String))
        );
    }

    #[test]
    fn float_accepts_integer_without_making_integer_dynamic() {
        assert!(Type::Float.accepts(&Type::Int));
        assert!(!Type::Int.accepts(&Type::Float));
    }
}
