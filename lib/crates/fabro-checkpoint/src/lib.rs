pub mod author;
pub mod branch;
pub mod error;
pub mod git;
pub mod metadata;
pub mod trailer;

pub const META_BRANCH_PREFIX: &str = "fabro/meta/";

pub use error::{Error, MetadataError, Result};
