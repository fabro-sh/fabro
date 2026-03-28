extern crate self as fabro_types;

pub mod checkpoint;
pub mod combine;
pub mod conclusion;
pub mod failure_signature;
pub mod graph;
pub mod outcome;
pub mod retro;
pub mod run;
pub mod sandbox_record;
pub mod settings;
pub mod start;
pub mod status;
pub mod usage;

pub use checkpoint::Checkpoint;
pub use conclusion::{Conclusion, StageSummary};
pub use failure_signature::FailureSignature;
pub use graph::{is_llm_handler_type, shape_to_handler_type, AttrValue, Edge, Graph, Node};
pub use outcome::{FailureCategory, FailureDetail, NodeResult, Outcome, OutcomeMeta, StageStatus};
pub use retro::{
    AggregateStats, FrictionKind, FrictionPoint, Learning, LearningCategory, OpenItem,
    OpenItemKind, Retro, RetroNarrative, SmoothnessRating, StageRetro,
};
pub use run::RunRecord;
pub use sandbox_record::SandboxRecord;
pub use settings::FabroSettings;
pub use start::StartRecord;
pub use status::{
    InvalidTransition, ParseRunStatusError, RunStatus, RunStatusRecord, StatusReason,
};
pub use usage::StageUsage;

pub use fabro_types_derive::Combine;
