//! Wire protocol spoken on every service socket.
//!
//! One JSON object per line, in both directions. Three verbs cover a shell:
//!
//! ```text
//! → {"id":1,"op":"get","path":"battery"}             ← {"id":1,"result":{...}}
//! → {"id":2,"op":"call","method":"x","args":{...}}   ← {"id":2,"result":...} | {"id":2,"error":"..."}
//! → {"id":3,"op":"subscribe","path":"battery"}       ← {"id":3,"event":{...}}  (repeated)
//! → {"id":4,"op":"unsubscribe","sub":3}              ← {"id":4,"result":null}
//! → {"id":5,"op":"describe"}                         ← {"id":5,"result":{"name":..,"paths":[..],"methods":[..]}}
//! ```
//!
//! A service must send the current value as the first event of a subscription,
//! so a widget never renders "unknown" while waiting for the first change.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    Get {
        path: String,
    },
    Call {
        method: String,
        #[serde(default)]
        args: Value,
    },
    Subscribe {
        path: String,
    },
    Unsubscribe {
        sub: u64,
    },
    Describe,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// Exactly one of `result`, `error` or `event` is present.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<Value>,
}

impl Response {
    pub fn result(id: u64, v: Value) -> Self {
        Self {
            id,
            result: Some(v),
            error: None,
            event: None,
        }
    }
    pub fn error(id: u64, e: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(e.into()),
            event: None,
        }
    }
    pub fn event(id: u64, v: Value) -> Self {
        Self {
            id,
            result: None,
            error: None,
            event: Some(v),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Description {
    pub name: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub methods: Vec<String>,
}

pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).expect("protocol types always serialize");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_shape_is_flat() {
        let r = Request {
            id: 3,
            op: Op::Subscribe {
                path: "battery".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"id":3,"op":"subscribe","path":"battery"})
        );
        let back: Request = serde_json::from_str(r#"{"id":2,"op":"call","method":"m"}"#).unwrap();
        assert_eq!(
            back.op,
            Op::Call {
                method: "m".into(),
                args: Value::Null
            }
        );
    }

    #[test]
    fn response_omits_absent_fields() {
        assert_eq!(
            encode(&Response::event(1, json!(5))),
            "{\"id\":1,\"event\":5}\n"
        );
    }
}
