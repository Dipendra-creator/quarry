use serde::{Deserialize, Serialize, Serializer};

/// A single cell. Serializes to plain JSON (`null`, `true`, `1`, `1.5`,
/// `"text"`), with bytes as a `0x…` hex string, so AI tools read rows as-is.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    #[serde(skip)]
    Bytes(Vec<u8>),
}

impl Serialize for Value {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Value::Null => s.serialize_none(),
            Value::Bool(b) => s.serialize_bool(*b),
            Value::Int(i) => s.serialize_i64(*i),
            Value::Float(f) => s.serialize_f64(*f),
            Value::Text(t) => s.serialize_str(t),
            Value::Bytes(b) => s.serialize_str(&hex(b)),
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(2 + bytes.len() * 2);
    out.push_str("0x");
    for b in bytes {
        out.push_str(&format!("{b:02X}"));
    }
    out
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Text shown in a grid cell / edited in the cell editor.
    pub fn display(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Text(t) => t.clone(),
            Value::Bytes(b) => hex(b),
        }
    }

    /// Same as `display`, but `None` for NULL so editors start empty.
    pub fn as_edit_text(&self) -> Option<String> {
        (!self.is_null()).then(|| self.display())
    }

    pub fn is_numeric(&self) -> bool {
        matches!(self, Value::Int(_) | Value::Float(_))
    }

    /// Converts JSON (from AI tool arguments) into a cell value.
    pub fn from_json(v: &serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64().unwrap_or_default()),
            },
            serde_json::Value::String(s) => Value::Text(s.clone()),
            other => Value::Text(other.to_string()),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Text(s.into())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Text(s)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape() {
        let row = vec![
            Value::Null,
            Value::Bool(true),
            Value::Int(3),
            Value::Text("a".into()),
            Value::Bytes(vec![0xde, 0xad]),
        ];
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"[null,true,3,"a","0xDEAD"]"#
        );
        let back: Vec<Value> = serde_json::from_str(r#"[null,true,3,1.5,"a"]"#).unwrap();
        assert_eq!(
            back,
            vec![Value::Null, Value::Bool(true), Value::Int(3), Value::Float(1.5), Value::Text("a".into())]
        );
    }
}
