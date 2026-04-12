use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ulid::Ulid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(Ulid);

impl RunId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.0.datetime().into()
    }

    #[cfg(test)]
    pub fn from_datetime(dt: DateTime<Utc>) -> Self {
        Self(Ulid::from_datetime(dt.into()))
    }
}

impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for RunId {
    type Err = ulid::DecodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Ulid::from_str(s)?))
    }
}

impl From<Ulid> for RunId {
    fn from(value: Ulid) -> Self {
        Self(value)
    }
}

impl From<RunId> for Ulid {
    fn from(value: RunId) -> Self {
        value.0
    }
}

impl From<RunId> for String {
    fn from(value: RunId) -> Self {
        value.to_string()
    }
}

impl From<&RunId> for String {
    fn from(value: &RunId) -> Self {
        value.to_string()
    }
}

impl Serialize for RunId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for RunId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

pub mod fixtures {
    use ulid::Ulid;

    use super::RunId;

    macro_rules! define_run_ids {
        ($($name:ident = $value:expr),+ $(,)?) => {
            $(pub const $name: RunId = RunId(Ulid($value));)+
        };
    }

    define_run_ids!(
        RUN_1 = 1,
        RUN_2 = 2,
        RUN_3 = 3,
        RUN_4 = 4,
        RUN_5 = 5,
        RUN_6 = 6,
        RUN_7 = 7,
        RUN_8 = 8,
        RUN_9 = 9,
        RUN_10 = 10,
        RUN_11 = 11,
        RUN_12 = 12,
        RUN_13 = 13,
        RUN_14 = 14,
        RUN_15 = 15,
        RUN_16 = 16,
        RUN_17 = 17,
        RUN_18 = 18,
        RUN_19 = 19,
        RUN_20 = 20,
        RUN_21 = 21,
        RUN_22 = 22,
        RUN_23 = 23,
        RUN_24 = 24,
        RUN_25 = 25,
        RUN_26 = 26,
        RUN_27 = 27,
        RUN_28 = 28,
        RUN_29 = 29,
        RUN_30 = 30,
        RUN_31 = 31,
        RUN_32 = 32,
        RUN_33 = 33,
        RUN_34 = 34,
        RUN_35 = 35,
        RUN_36 = 36,
        RUN_37 = 37,
        RUN_38 = 38,
        RUN_39 = 39,
        RUN_40 = 40,
        RUN_41 = 41,
        RUN_42 = 42,
        RUN_43 = 43,
        RUN_44 = 44,
        RUN_45 = 45,
        RUN_46 = 46,
        RUN_47 = 47,
        RUN_48 = 48,
        RUN_49 = 49,
        RUN_50 = 50,
        RUN_51 = 51,
        RUN_52 = 52,
        RUN_53 = 53,
        RUN_54 = 54,
        RUN_55 = 55,
        RUN_56 = 56,
        RUN_57 = 57,
        RUN_58 = 58,
        RUN_59 = 59,
        RUN_60 = 60,
        RUN_61 = 61,
        RUN_62 = 62,
        RUN_63 = 63,
        RUN_64 = 64
    );
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::{RunId, fixtures};

    #[test]
    fn serializes_as_a_ulid_string() {
        let value = serde_json::to_value(fixtures::RUN_1).unwrap();
        assert_eq!(value, serde_json::json!("00000000000000000000000001"));
    }

    #[test]
    fn deserializes_from_a_ulid_string() {
        let value: RunId =
            serde_json::from_value(serde_json::json!("0000000000000000000000001A")).unwrap();

        assert_eq!(value, fixtures::RUN_42);
    }

    #[test]
    fn exposes_created_at_from_ulid_timestamp() {
        let dt = Utc.with_ymd_and_hms(2026, 3, 27, 12, 34, 56).unwrap();
        let run_id = RunId::from_datetime(dt);

        assert_eq!(run_id.created_at(), dt);
    }

    #[test]
    fn creates_run_id_from_datetime() {
        let dt = Utc.with_ymd_and_hms(2026, 3, 27, 12, 34, 56).unwrap();
        let run_id = RunId::from_datetime(dt);

        assert_eq!(run_id.created_at(), dt);
    }
}
