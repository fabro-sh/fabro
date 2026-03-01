use chrono::{DateTime, Utc};

#[derive(Debug, sqlx::FromRow)]
pub struct WorkflowRun {
    pub id: String,
    pub title: String,
    pub logs_dir: String,
    pub work_dir: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
