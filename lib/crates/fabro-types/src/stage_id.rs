use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StageId {
    node_id: String,
    visit:   u32,
}

impl StageId {
    #[must_use]
    pub fn new(node_id: impl Into<String>, visit: u32) -> Self {
        Self {
            node_id: node_id.into(),
            visit,
        }
    }

    #[must_use]
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    #[must_use]
    pub fn visit(&self) -> u32 {
        self.visit
    }
}

impl fmt::Display for StageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.node_id, self.visit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStageIdError(String);

impl fmt::Display for ParseStageIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseStageIdError {}

impl FromStr for StageId {
    type Err = ParseStageIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (node_id, visit) = s
            .rsplit_once('@')
            .ok_or_else(|| ParseStageIdError("stage id must contain '@'".to_string()))?;
        if node_id.is_empty() {
            return Err(ParseStageIdError(
                "stage id node_id must not be empty".to_string(),
            ));
        }
        if visit.is_empty() {
            return Err(ParseStageIdError(
                "stage id visit suffix must not be empty".to_string(),
            ));
        }
        let visit = visit
            .parse()
            .map_err(|err| ParseStageIdError(format!("invalid stage id visit: {err}")))?;
        Ok(Self::new(node_id, visit))
    }
}

impl Serialize for StageId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StageId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParallelBranchId {
    group: StageId,
    index: u32,
}

impl ParallelBranchId {
    #[must_use]
    pub fn new(group: StageId, index: u32) -> Self {
        Self { group, index }
    }

    #[must_use]
    pub fn group(&self) -> &StageId {
        &self.group
    }

    #[must_use]
    pub fn index(&self) -> u32 {
        self.index
    }
}

impl fmt::Display for ParallelBranchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.group, self.index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseParallelBranchIdError(String);

impl fmt::Display for ParseParallelBranchIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseParallelBranchIdError {}

impl FromStr for ParallelBranchId {
    type Err = ParseParallelBranchIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (group, index) = s.rsplit_once(':').ok_or_else(|| {
            ParseParallelBranchIdError("parallel branch id must contain ':'".to_string())
        })?;
        let group = group.parse::<StageId>().map_err(|err| {
            ParseParallelBranchIdError(format!("invalid parallel group id: {err}"))
        })?;
        if index.is_empty() {
            return Err(ParseParallelBranchIdError(
                "parallel branch id index must not be empty".to_string(),
            ));
        }
        let index = index.parse().map_err(|err| {
            ParseParallelBranchIdError(format!("invalid parallel branch index: {err}"))
        })?;
        Ok(Self::new(group, index))
    }
}

impl Serialize for ParallelBranchId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ParallelBranchId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{ParallelBranchId, StageId};

    #[test]
    fn display_and_parse_round_trip() {
        let stage = StageId::new("code", 2);
        assert_eq!(stage.to_string(), "code@2");
        assert_eq!("code@2".parse::<StageId>().unwrap(), stage);
    }

    #[test]
    fn ordering_is_node_id_then_visit() {
        let mut stages = vec![
            StageId::new("code", 2),
            StageId::new("build", 1),
            StageId::new("code", 1),
        ];
        stages.sort();
        assert_eq!(stages, vec![
            StageId::new("build", 1),
            StageId::new("code", 1),
            StageId::new("code", 2),
        ]);
    }

    #[test]
    fn serde_round_trip_uses_string_form() {
        let stage = StageId::new("code", 2);
        let value = serde_json::to_value(&stage).unwrap();
        assert_eq!(value, serde_json::json!("code@2"));
        let decoded: StageId = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, stage);
    }

    #[test]
    fn parse_rejects_missing_at_sign() {
        let err = "code".parse::<StageId>().unwrap_err();
        assert_eq!(err.to_string(), "stage id must contain '@'");
    }

    #[test]
    fn parse_rejects_empty_suffix() {
        let err = "code@".parse::<StageId>().unwrap_err();
        assert_eq!(err.to_string(), "stage id visit suffix must not be empty");
    }

    #[test]
    fn parse_rejects_non_numeric_visit() {
        let err = "code@two".parse::<StageId>().unwrap_err();
        assert!(err.to_string().starts_with("invalid stage id visit:"));
    }

    #[test]
    fn parse_rejects_empty_node_id() {
        let err = "@3".parse::<StageId>().unwrap_err();
        assert_eq!(err.to_string(), "stage id node_id must not be empty");
    }

    #[test]
    fn parallel_branch_id_display_and_parse_round_trip() {
        let branch = ParallelBranchId::new(StageId::new("fanout", 2), 3);
        assert_eq!(branch.to_string(), "fanout@2:3");
        assert_eq!("fanout@2:3".parse::<ParallelBranchId>().unwrap(), branch);
    }

    #[test]
    fn parallel_branch_id_serde_round_trip_uses_string_form() {
        let branch = ParallelBranchId::new(StageId::new("fanout", 2), 0);
        let value = serde_json::to_value(&branch).unwrap();
        assert_eq!(value, serde_json::json!("fanout@2:0"));
        let decoded: ParallelBranchId = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, branch);
    }

    #[test]
    fn parallel_branch_id_rejects_missing_colon() {
        let err = "fanout@2".parse::<ParallelBranchId>().unwrap_err();
        assert_eq!(err.to_string(), "parallel branch id must contain ':'");
    }

    #[test]
    fn parallel_branch_id_rejects_bad_group() {
        let err = "fanout:0".parse::<ParallelBranchId>().unwrap_err();
        assert!(err.to_string().starts_with("invalid parallel group id:"));
    }

    #[test]
    fn parallel_branch_id_rejects_non_numeric_index() {
        let err = "fanout@2:zero".parse::<ParallelBranchId>().unwrap_err();
        assert!(
            err.to_string()
                .starts_with("invalid parallel branch index:")
        );
    }
}
