//! Journal operations (`docs/03` §4): one JSON line per op, per-device `seq`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Fields = BTreeMap<String, serde_json::Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Set,
    Delete,
}

/// A published operation. `fields` is a `BTreeMap`, so the encoding is canonical: the same
/// op always serialises to the same bytes (re-uploading a chunk is byte-identical).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub seq: u64,
    pub hlc: String,
    pub col: String,
    pub id: String,
    pub op: OpKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Fields>,
    pub by: String,
}

impl Op {
    /// One journal line, without the trailing newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("op serialises")
    }

    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_matches_the_spec_shape_and_is_canonical() {
        let mut f = Fields::new();
        f.insert("title".into(), "Docs".into());
        f.insert("parentId".into(), "a1f9".into());
        f.insert("pos".into(), "V".into());
        let op = Op {
            seq: 4121,
            hlc: "1790000000000-0003-6fba69b7".into(),
            col: "bookmarks".into(),
            id: "c0b4".into(),
            op: OpKind::Set,
            fields: Some(f),
            by: "6fba69b7".into(),
        };
        let line = op.to_line();
        assert_eq!(
            line,
            r#"{"seq":4121,"hlc":"1790000000000-0003-6fba69b7","col":"bookmarks","id":"c0b4","op":"set","fields":{"parentId":"a1f9","pos":"V","title":"Docs"},"by":"6fba69b7"}"#
        );
        assert_eq!(Op::from_line(&line).unwrap(), op);
        let del = Op {
            op: OpKind::Delete,
            fields: None,
            ..op
        };
        assert!(!del.to_line().contains("fields"));
    }
}
