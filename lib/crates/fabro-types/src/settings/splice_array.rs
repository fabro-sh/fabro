//! Splice-capable string arrays.
//!
//! In declared splice-capable array paths, the literal string value `"..."`
//! is reserved: it represents "splice in inherited values from lower-precedence
//! layers here." At most one `"..."` marker may appear per array. In the base
//! layer with no inherited parent, the marker resolves to an empty inherited
//! segment. In non-splice paths the same literal is a hard error — enforced
//! by using the plain `Vec<String>` type elsewhere and this type only where
//! splice semantics are explicitly allowed.

use std::fmt;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The reserved literal that marks the splice insertion point.
pub const SPLICE_MARKER: &str = "...";

/// A string array that may contain at most one splice marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpliceArray {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Value(String),
    Splice,
}

/// An error returned when a splice array fails validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpliceArrayError {
    /// The array contained more than one splice marker.
    MultipleMarkers,
}

impl fmt::Display for SpliceArrayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleMarkers => {
                f.write_str(r#"splice array must contain at most one "..." marker"#)
            }
        }
    }
}

impl std::error::Error for SpliceArrayError {}

impl SpliceArray {
    /// Build a splice array from a raw `Vec<String>`.
    pub fn from_raw(raw: Vec<String>) -> Result<Self, SpliceArrayError> {
        let mut entries = Vec::with_capacity(raw.len());
        let mut marker_count = 0;
        for item in raw {
            if item == SPLICE_MARKER {
                marker_count += 1;
                entries.push(Entry::Splice);
            } else {
                entries.push(Entry::Value(item));
            }
        }
        if marker_count > 1 {
            return Err(SpliceArrayError::MultipleMarkers);
        }
        Ok(Self { entries })
    }

    /// Build a splice array with no inherited splice marker.
    #[must_use]
    pub fn from_values(values: impl IntoIterator<Item = String>) -> Self {
        Self {
            entries: values.into_iter().map(Entry::Value).collect(),
        }
    }

    /// True when the array contains a splice marker.
    #[must_use]
    pub fn has_splice(&self) -> bool {
        self.entries.iter().any(|e| matches!(e, Entry::Splice))
    }

    /// The index of the splice marker, if present.
    #[must_use]
    pub fn splice_position(&self) -> Option<usize> {
        self.entries.iter().position(|e| matches!(e, Entry::Splice))
    }

    /// The non-splice values, in source order.
    #[must_use]
    pub fn values(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Value(v) => Some(v.as_str()),
                Entry::Splice => None,
            })
            .collect()
    }

    /// Resolve this array against an inherited lower-precedence value list.
    ///
    /// - If the array has a splice marker, the inherited list is spliced in
    ///   at the marker position.
    /// - If the array has no splice marker, it replaces the inherited list
    ///   wholesale.
    #[must_use]
    pub fn resolve(self, inherited: Vec<String>) -> Vec<String> {
        let Some(pos) = self.splice_position() else {
            return self
                .entries
                .into_iter()
                .filter_map(|e| match e {
                    Entry::Value(v) => Some(v),
                    Entry::Splice => None,
                })
                .collect();
        };

        let mut prefix = Vec::new();
        let mut suffix = Vec::new();
        for (i, entry) in self.entries.into_iter().enumerate() {
            match entry {
                Entry::Value(v) => {
                    if i < pos {
                        prefix.push(v);
                    } else {
                        suffix.push(v);
                    }
                }
                Entry::Splice => {}
            }
        }

        let mut out = prefix;
        out.extend(inherited);
        out.extend(suffix);
        out
    }
}

impl Serialize for SpliceArray {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.entries.len()))?;
        for entry in &self.entries {
            match entry {
                Entry::Value(v) => seq.serialize_element(v)?,
                Entry::Splice => seq.serialize_element(SPLICE_MARKER)?,
            }
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for SpliceArray {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SpliceArrayVisitor;

        impl<'de> Visitor<'de> for SpliceArrayVisitor {
            type Value = SpliceArray;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    r#"an array of strings, optionally containing a single "..." splice marker"#,
                )
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<SpliceArray, A::Error> {
                let mut raw: Vec<String> = Vec::new();
                while let Some(item) = seq.next_element::<String>()? {
                    raw.push(item);
                }
                SpliceArray::from_raw(raw).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_seq(SpliceArrayVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_with_no_marker() {
        let arr = SpliceArray::from_raw(vec!["a".into(), "b".into()]).unwrap();
        assert!(!arr.has_splice());
        assert_eq!(arr.values(), vec!["a", "b"]);
    }

    #[test]
    fn append_marker_at_front() {
        let arr = SpliceArray::from_raw(vec!["...".into(), "c".into()]).unwrap();
        assert_eq!(arr.splice_position(), Some(0));
        let resolved = arr.resolve(vec!["a".into(), "b".into()]);
        assert_eq!(resolved, vec!["a", "b", "c"]);
    }

    #[test]
    fn prepend_marker_at_back() {
        let arr = SpliceArray::from_raw(vec!["a".into(), "...".into()]).unwrap();
        assert_eq!(arr.splice_position(), Some(1));
        let resolved = arr.resolve(vec!["b".into(), "c".into()]);
        assert_eq!(resolved, vec!["a", "b", "c"]);
    }

    #[test]
    fn marker_in_middle() {
        let arr = SpliceArray::from_raw(vec!["pre".into(), "...".into(), "post".into()]).unwrap();
        let resolved = arr.resolve(vec!["mid".into()]);
        assert_eq!(resolved, vec!["pre", "mid", "post"]);
    }

    #[test]
    fn replace_semantics_without_marker() {
        let arr = SpliceArray::from_raw(vec!["only".into()]).unwrap();
        let resolved = arr.resolve(vec!["inherited".into()]);
        assert_eq!(resolved, vec!["only"]);
    }

    #[test]
    fn multiple_markers_rejected() {
        let err = SpliceArray::from_raw(vec!["...".into(), "...".into()]).unwrap_err();
        assert_eq!(err, SpliceArrayError::MultipleMarkers);
    }

    #[test]
    fn base_layer_with_splice_resolves_to_empty_inherited() {
        let arr = SpliceArray::from_raw(vec!["...".into(), "b".into()]).unwrap();
        let resolved = arr.resolve(vec![]);
        assert_eq!(resolved, vec!["b"]);
    }

    #[test]
    fn serde_round_trip_via_json() {
        #[derive(Debug, serde::Deserialize, serde::Serialize, PartialEq)]
        struct Wrap {
            a: SpliceArray,
        }

        let input = r#"{"a":["...","b"]}"#;
        let parsed: Wrap = serde_json::from_str(input).unwrap();
        assert!(parsed.a.has_splice());
        let rendered = serde_json::to_string(&parsed).unwrap();
        assert_eq!(rendered, input);
    }

    #[test]
    fn serde_rejects_multiple_markers() {
        #[derive(Debug, serde::Deserialize)]
        struct Wrap {
            _a: SpliceArray,
        }

        let input = r#"{"_a":["...","..."]}"#;
        let err = serde_json::from_str::<Wrap>(input).unwrap_err();
        assert!(err.to_string().contains("at most one"));
    }
}
