/// Neutral Lisp reader values used as a syntax tree by the typed frontend.
use std::fmt;

// ── Serde ─────────────────────────────────────────────────────────────────────

impl serde::Serialize for Val {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Val::Nil => {
                let mut m = s.serialize_map(Some(1))?;
                m.serialize_entry("t", "nil")?;
                m.end()
            }
            Val::Bool(b) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "bool")?;
                m.serialize_entry("v", b)?;
                m.end()
            }
            Val::Int(n) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "int")?;
                m.serialize_entry("v", n)?;
                m.end()
            }
            Val::Float(f) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "float")?;
                m.serialize_entry("v", f)?;
                m.end()
            }
            Val::Str(s_val) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "str")?;
                m.serialize_entry("v", s_val)?;
                m.end()
            }
            Val::Symbol(sym) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "symbol")?;
                m.serialize_entry("v", sym)?;
                m.end()
            }
            Val::Bytes(b) => {
                let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "bytes")?;
                m.serialize_entry("v", &hex)?;
                m.end()
            }
            Val::List(vs) => {
                let mut m = s.serialize_map(Some(2))?;
                m.serialize_entry("t", "list")?;
                m.serialize_entry("v", vs)?;
                m.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for Val {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{self, MapAccess, Visitor};
        use std::fmt;

        struct ValVisitor;

        impl<'de> Visitor<'de> for ValVisitor {
            type Value = Val;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Val map with keys \"t\" and optionally \"v\"")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Val, A::Error> {
                let mut t: Option<String> = None;
                let mut v: Option<serde_json::Value> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "t" => t = Some(map.next_value()?),
                        "v" => v = Some(map.next_value()?),
                        _ => {
                            let _ = map.next_value::<serde_json::Value>()?;
                        }
                    }
                }

                let tag = t.ok_or_else(|| de::Error::missing_field("t"))?;
                match tag.as_str() {
                    "nil" => Ok(Val::Nil),
                    "bool" => {
                        let b = v
                            .and_then(|v| v.as_bool())
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        Ok(Val::Bool(b))
                    }
                    "int" => {
                        let n = v
                            .and_then(|v| v.as_i64())
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        Ok(Val::Int(n))
                    }
                    "float" => {
                        let f = v
                            .and_then(|v| v.as_f64())
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        Ok(Val::Float(f))
                    }
                    "str" => {
                        let s = v
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        Ok(Val::Str(s))
                    }
                    "symbol" => {
                        let s = v
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        Ok(Val::Symbol(s))
                    }
                    "bytes" => {
                        let hex = v
                            .and_then(|v| v.as_str().map(|s| s.to_string()))
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        let bytes = (0..hex.len())
                            .step_by(2)
                            .map(|i| {
                                u8::from_str_radix(&hex[i..i + 2], 16)
                                    .map_err(|_| de::Error::custom("invalid hex in bytes"))
                            })
                            .collect::<Result<Vec<u8>, _>>()?;
                        Ok(Val::Bytes(bytes))
                    }
                    "list" => {
                        let arr = v
                            .and_then(|v| {
                                if let serde_json::Value::Array(a) = v {
                                    Some(a)
                                } else {
                                    None
                                }
                            })
                            .ok_or_else(|| de::Error::missing_field("v"))?;
                        let items: Result<Vec<Val>, _> = arr
                            .into_iter()
                            .map(|item| {
                                serde_json::from_value::<Val>(item).map_err(de::Error::custom)
                            })
                            .collect();
                        Ok(Val::List(items?))
                    }
                    other => Err(de::Error::unknown_variant(
                        other,
                        &[
                            "nil", "bool", "int", "float", "str", "symbol", "bytes", "list",
                        ],
                    )),
                }
            }
        }

        d.deserialize_map(ValVisitor)
    }
}

#[derive(Clone, Debug)]
pub enum Val {
    /// Empty list / falsy nil
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Symbol(String),
    /// Raw bytes — output of crypto operations, SSH payloads, etc.
    Bytes(Vec<u8>),
    /// Proper list
    List(Vec<Val>),
}

// ── Display ───────────────────────────────────────────────────────────────────

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Val::Nil => write!(f, "()"),
            Val::Bool(true) => write!(f, "#t"),
            Val::Bool(false) => write!(f, "#f"),
            Val::Int(n) => write!(f, "{n}"),
            Val::Float(n) => {
                if n.fract() == 0.0 {
                    write!(f, "{n:.1}")
                } else {
                    write!(f, "{n}")
                }
            }
            Val::Str(s) => write!(f, "{s}"),
            Val::Symbol(s) => write!(f, "{s}"),
            Val::Bytes(b) => {
                for byte in b {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
            Val::List(vs) => {
                write!(f, "(")?;
                for (i, v) in vs.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{}", v.repr())?;
                }
                write!(f, ")")
            }
        }
    }
}

impl Val {
    /// Like Display but wraps strings in quotes (for printing inside lists).
    pub fn repr(&self) -> String {
        match self {
            Val::Str(s) => format!("\"{s}\""),
            other => other.to_string(),
        }
    }

    pub fn is_truthy(&self) -> bool {
        !matches!(self, Val::Nil | Val::Bool(false) | Val::Int(0))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Val::Nil => "nil",
            Val::Bool(_) => "bool",
            Val::Int(_) => "int",
            Val::Float(_) => "float",
            Val::Str(_) => "string",
            Val::Symbol(_) => "symbol",
            Val::Bytes(_) => "bytes",
            Val::List(_) => "list",
        }
    }

    pub fn as_int(&self) -> anyhow::Result<i64> {
        match self {
            Val::Int(n) => Ok(*n),
            Val::Float(f) => Ok(*f as i64),
            other => anyhow::bail!("expected int, got {}", other.type_name()),
        }
    }

    pub fn as_float(&self) -> anyhow::Result<f64> {
        match self {
            Val::Int(n) => Ok(*n as f64),
            Val::Float(f) => Ok(*f),
            other => anyhow::bail!("expected number, got {}", other.type_name()),
        }
    }

    pub fn as_str(&self) -> anyhow::Result<&str> {
        match self {
            Val::Str(s) => Ok(s.as_str()),
            other => anyhow::bail!("expected string, got {}", other.type_name()),
        }
    }

    pub fn as_bytes(&self) -> anyhow::Result<&[u8]> {
        match self {
            Val::Bytes(b) => Ok(b.as_slice()),
            Val::Str(s) => Ok(s.as_bytes()),
            other => anyhow::bail!("expected bytes, got {}", other.type_name()),
        }
    }

    pub fn as_list(&self) -> anyhow::Result<&[Val]> {
        match self {
            Val::List(v) => Ok(v),
            Val::Nil => Ok(&[]),
            other => anyhow::bail!("expected list, got {}", other.type_name()),
        }
    }
}

impl PartialEq for Val {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Val::Nil, Val::Nil) => true,
            (Val::Bool(a), Val::Bool(b)) => a == b,
            (Val::Int(a), Val::Int(b)) => a == b,
            (Val::Float(a), Val::Float(b)) => a == b,
            (Val::Str(a), Val::Str(b)) => a == b,
            (Val::Symbol(a), Val::Symbol(b)) => a == b,
            (Val::Bytes(a), Val::Bytes(b)) => a == b,
            (Val::List(a), Val::List(b)) => a == b,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_val_display_nil() {
        assert_eq!(Val::Nil.to_string(), "()");
    }

    #[test]
    fn test_val_display_bool() {
        assert_eq!(Val::Bool(true).to_string(), "#t");
        assert_eq!(Val::Bool(false).to_string(), "#f");
    }

    #[test]
    fn test_val_display_list() {
        let v = Val::List(vec![Val::Int(1), Val::Str("hi".to_string()), Val::Nil]);
        assert_eq!(v.to_string(), "(1 \"hi\" ())");
    }

    #[test]
    fn test_val_is_truthy() {
        assert!(Val::Bool(true).is_truthy());
        assert!(Val::Int(1).is_truthy());
        assert!(Val::Str("x".to_string()).is_truthy());
        assert!(!Val::Bool(false).is_truthy());
        assert!(!Val::Nil.is_truthy());
        assert!(!Val::Int(0).is_truthy());
    }

    #[test]
    fn test_val_as_int_from_float() {
        assert_eq!(Val::Float(3.7).as_int().unwrap(), 3);
    }

    #[test]
    fn test_val_as_bytes_from_str() {
        let v = Val::Str("abc".to_string());
        assert_eq!(v.as_bytes().unwrap(), b"abc");
    }

    #[test]
    fn test_bytes_display_is_hex() {
        let v = Val::Bytes(vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(v.to_string(), "deadbeef");
    }

    #[test]
    fn test_val_equality() {
        assert_eq!(Val::Int(42), Val::Int(42));
        assert_ne!(Val::Int(42), Val::Float(42.0));
        assert_ne!(Val::Nil, Val::Bool(false));
    }

    #[test]
    fn test_val_roundtrip_primitives() {
        for val in [
            Val::Nil,
            Val::Bool(true),
            Val::Bool(false),
            Val::Int(-7),
            Val::Float(3.14),
            Val::Str("hello".to_string()),
            Val::Symbol("foo".to_string()),
            Val::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        ] {
            let json = serde_json::to_string(&val).unwrap();
            let back: Val = serde_json::from_str(&json).unwrap();
            assert_eq!(val, back, "roundtrip failed for {json}");
        }
    }

    #[test]
    fn test_val_roundtrip_list() {
        let val = Val::List(vec![Val::Int(1), Val::Str("x".to_string()), Val::Nil]);
        let json = serde_json::to_string(&val).unwrap();
        let back: Val = serde_json::from_str(&json).unwrap();
        assert_eq!(val, back);
    }
}
