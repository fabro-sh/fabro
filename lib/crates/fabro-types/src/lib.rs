extern crate self as fabro_types;

pub mod checkpoint;
pub mod combine;
pub mod conclusion;
pub mod failure_signature;
pub mod graph;
pub mod node_status;
pub mod outcome;
pub mod pull_request;
pub mod retro;
pub mod run;
pub mod run_id;
pub mod sandbox_record;
pub mod settings;
pub mod start;
pub mod status;
pub mod usage;

pub use checkpoint::Checkpoint;
pub use conclusion::{Conclusion, StageSummary};
pub use failure_signature::FailureSignature;
pub use graph::{AttrValue, Edge, Graph, Node, is_llm_handler_type, shape_to_handler_type};
pub use node_status::NodeStatusRecord;
pub use outcome::{FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageStatus};
pub use pull_request::PullRequestRecord;
pub use retro::{
    AggregateStats, FrictionKind, FrictionPoint, Learning, LearningCategory, OpenItem,
    OpenItemKind, Retro, RetroNarrative, SmoothnessRating, StageRetro,
};
pub use run::RunRecord;
pub use run_id::RunId;
pub use run_id::fixtures;
pub use sandbox_record::SandboxRecord;
pub use settings::Settings;
pub use start::StartRecord;
pub use status::{
    InvalidTransition, ParseRunStatusError, RunStatus, RunStatusRecord, StatusReason,
};
pub use usage::StageUsage;

pub use fabro_macros::Combine;
