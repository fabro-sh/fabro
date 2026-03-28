use std::collections::HashMap;
use std::hash::Hash;
use std::path::PathBuf;

pub trait Combine {
    #[must_use]
    fn combine(self, other: Self) -> Self;
}

impl<T: Combine> Combine for Option<T> {
    fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Some(this), Some(other)) => Some(this.combine(other)),
            (Some(this), None) => Some(this),
            (None, Some(other)) => Some(other),
            (None, None) => None,
        }
    }
}

impl<T> Combine for Vec<T> {
    fn combine(mut self, other: Self) -> Self {
        self.extend(other);
        self
    }
}

impl<K, V> Combine for HashMap<K, V>
where
    K: Eq + Hash,
    V: Combine,
{
    fn combine(mut self, other: Self) -> Self {
        for (key, value) in other {
            match self.remove(&key) {
                Some(existing) => {
                    self.insert(key, existing.combine(value));
                }
                None => {
                    self.insert(key, value);
                }
            }
        }

        self
    }
}

macro_rules! impl_left_wins {
    ($($ty:ty),* $(,)?) => {
        $(
            impl Combine for $ty {
                fn combine(self, _other: Self) -> Self {
                    self
                }
            }
        )*
    };
}

impl_left_wins!(bool, i32, u16, u32, u64, usize, String, PathBuf,);
