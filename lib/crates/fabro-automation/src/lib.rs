mod error;
mod id;
mod model;
mod store;

pub use error::{AutomationStoreError, AutomationValidationError};
pub use id::{AutomationId, AutomationRevision, AutomationRevisionParseError, AutomationTriggerId};
pub use model::{
    ApiTrigger, Automation, AutomationDraft, AutomationReplace, AutomationTarget,
    AutomationTrigger, ScheduleTrigger,
};
pub use store::AutomationStore;
