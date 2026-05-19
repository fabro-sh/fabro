macro_rules! ulid_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(::ulid::Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(::ulid::Ulid::new())
            }

            pub fn created_at(&self) -> ::chrono::DateTime<::chrono::Utc> {
                self.0.datetime().into()
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = ::ulid::DecodeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(<::ulid::Ulid as ::std::str::FromStr>::from_str(s)?))
            }
        }

        impl From<::ulid::Ulid> for $name {
            fn from(value: ::ulid::Ulid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for ::ulid::Ulid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.to_string()
            }
        }

        impl From<&$name> for String {
            fn from(value: &$name) -> Self {
                value.to_string()
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let value = <String as ::serde::Deserialize>::deserialize(deserializer)?;
                value.parse().map_err(::serde::de::Error::custom)
            }
        }
    };
}

pub(crate) use ulid_id;
