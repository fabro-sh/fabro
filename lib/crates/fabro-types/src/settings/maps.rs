use std::collections::HashMap;
use std::collections::hash_map::{IntoIter, Iter, IterMut};
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

use super::combine::Combine;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReplaceMap<V>(pub HashMap<String, V>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StickyMap<V>(pub HashMap<String, V>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MergeMap<V>(pub HashMap<String, V>);

macro_rules! impl_map_wrapper {
    ($name:ident) => {
        impl<V> $name<V> {
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            #[must_use]
            pub fn into_inner(self) -> HashMap<String, V> {
                self.0
            }

            pub fn iter(&self) -> Iter<'_, String, V> {
                self.0.iter()
            }

            pub fn iter_mut(&mut self) -> IterMut<'_, String, V> {
                self.0.iter_mut()
            }
        }

        impl<V> Deref for $name<V> {
            type Target = HashMap<String, V>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<V> DerefMut for $name<V> {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl<V> From<HashMap<String, V>> for $name<V> {
            fn from(value: HashMap<String, V>) -> Self {
                Self(value)
            }
        }

        impl<V> Default for $name<V> {
            fn default() -> Self {
                Self(HashMap::new())
            }
        }

        impl<V> AsRef<HashMap<String, V>> for $name<V> {
            fn as_ref(&self) -> &HashMap<String, V> {
                &self.0
            }
        }

        impl<V> AsMut<HashMap<String, V>> for $name<V> {
            fn as_mut(&mut self) -> &mut HashMap<String, V> {
                &mut self.0
            }
        }

        impl<V> IntoIterator for $name<V> {
            type IntoIter = IntoIter<String, V>;
            type Item = (String, V);

            fn into_iter(self) -> Self::IntoIter {
                self.0.into_iter()
            }
        }

        impl<'a, V> IntoIterator for &'a $name<V> {
            type IntoIter = Iter<'a, String, V>;
            type Item = (&'a String, &'a V);

            fn into_iter(self) -> Self::IntoIter {
                self.0.iter()
            }
        }

        impl<'a, V> IntoIterator for &'a mut $name<V> {
            type IntoIter = IterMut<'a, String, V>;
            type Item = (&'a String, &'a mut V);

            fn into_iter(self) -> Self::IntoIter {
                self.0.iter_mut()
            }
        }
    };
}

impl_map_wrapper!(ReplaceMap);
impl_map_wrapper!(StickyMap);
impl_map_wrapper!(MergeMap);

impl<V> Combine for ReplaceMap<V> {
    fn combine(self, other: Self) -> Self {
        if self.0.is_empty() { other } else { self }
    }
}

impl<V> Combine for StickyMap<V> {
    fn combine(self, other: Self) -> Self {
        let mut combined = other.0;
        for (key, value) in self.0 {
            combined.insert(key, value);
        }
        Self(combined)
    }
}

impl<V: Combine> Combine for MergeMap<V> {
    fn combine(self, other: Self) -> Self {
        let mut combined = other.0;
        for (key, value) in self.0 {
            let value = match combined.remove(&key) {
                Some(fallback) => value.combine(fallback),
                None => value,
            };
            combined.insert(key, value);
        }
        Self(combined)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, fabro_macros::Combine)]
    struct ValueLayer {
        a: Option<String>,
        b: Option<String>,
    }

    #[test]
    fn replace_map_self_wins_when_non_empty() {
        let this = ReplaceMap(HashMap::from([("a".to_string(), "this".to_string())]));
        let fallback = ReplaceMap(HashMap::from([
            ("a".to_string(), "fallback".to_string()),
            ("b".to_string(), "fallback".to_string()),
        ]));

        assert_eq!(
            this.combine(fallback),
            ReplaceMap(HashMap::from([("a".to_string(), "this".to_string())]))
        );
    }

    #[test]
    fn replace_map_empty_self_uses_fallback() {
        let this = ReplaceMap::<String>(HashMap::new());
        let fallback = ReplaceMap(HashMap::from([("a".to_string(), "fallback".to_string())]));

        assert_eq!(
            this.combine(fallback),
            ReplaceMap(HashMap::from([("a".to_string(), "fallback".to_string())]))
        );
    }

    #[test]
    fn replace_map_round_trips_as_toml_table() {
        let parsed: ReplaceMap<String> =
            toml::from_str(r#"a = "one""#).expect("fixture should deserialize");

        assert_eq!(
            parsed,
            ReplaceMap(HashMap::from([("a".to_string(), "one".to_string())]))
        );

        let serialized = toml::to_string(&parsed).expect("fixture should serialize");
        let reparsed: ReplaceMap<String> =
            toml::from_str(&serialized).expect("fixture should deserialize again");

        assert_eq!(reparsed, parsed);
    }

    #[test]
    fn sticky_map_merges_keys_with_self_winning_conflicts() {
        let this = StickyMap(HashMap::from([
            ("a".to_string(), "this".to_string()),
            ("c".to_string(), "this".to_string()),
        ]));
        let fallback = StickyMap(HashMap::from([
            ("a".to_string(), "fallback".to_string()),
            ("b".to_string(), "fallback".to_string()),
        ]));

        assert_eq!(
            this.combine(fallback),
            StickyMap(HashMap::from([
                ("a".to_string(), "this".to_string()),
                ("b".to_string(), "fallback".to_string()),
                ("c".to_string(), "this".to_string()),
            ]))
        );
    }

    #[test]
    fn merge_map_recursively_combines_values_for_matching_keys() {
        let this = MergeMap(HashMap::from([("ops".to_string(), ValueLayer {
            a: Some("this".to_string()),
            b: None,
        })]));
        let fallback = MergeMap(HashMap::from([("ops".to_string(), ValueLayer {
            a: Some("fallback".to_string()),
            b: Some("fallback".to_string()),
        })]));

        assert_eq!(
            this.combine(fallback),
            MergeMap(HashMap::from([("ops".to_string(), ValueLayer {
                a: Some("this".to_string()),
                b: Some("fallback".to_string()),
            },)]))
        );
    }
}
