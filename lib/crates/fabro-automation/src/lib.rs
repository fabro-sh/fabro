mod error;
mod id;
mod model;
mod store;

pub use error::{AutomationStoreError, AutomationValidationError};
pub use id::{AutomationId, AutomationRevision, AutomationRevisionParseError, AutomationTriggerId};
pub use model::{
    ApiTrigger, Automation, AutomationDraft, AutomationReplace, AutomationTarget,
    AutomationTrigger, GitHubRepositorySlug, ScheduleTrigger, parse_github_repository_slug,
    parse_schedule_expression,
};
pub use store::AutomationStore;
