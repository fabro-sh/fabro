use fabro_checkpoint::git::Store as GitStore;
use fabro_store::Database;
use fabro_types::{RunId, RunProjection};
use git2::Repository;
use tokio::task::spawn_blocking;

use super::run_store::map_open_run_error;
use crate::error::Error;

pub(crate) async fn load_projection(
    store: &Database,
    run_id: &RunId,
) -> Result<RunProjection, Error> {
    let run_store = store
        .open_run_reader(run_id)
        .await
        .map_err(|err| map_open_run_error(run_id, err))?;
    run_store
        .state()
        .await
        .map_err(|err| Error::engine(err.to_string()))
}

pub(crate) async fn with_run_git_store<T>(
    store: &Database,
    run_id: RunId,
    operation: impl FnOnce(GitStore) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error>
where
    T: Send + 'static,
{
    let projection = load_projection(store, &run_id).await?;
    let spec = projection
        .spec
        .ok_or_else(|| Error::Precondition(format!("run {run_id} has no spec")))?;
    let working_directory = spec.working_directory;

    spawn_blocking(move || {
        let repo = Repository::discover(&working_directory).map_err(|err| {
            Error::Unsupported(format!(
                "server cannot access run {run_id}'s working_directory {}: {err}",
                working_directory.display()
            ))
        })?;
        operation(GitStore::new(repo))
    })
    .await
    .map_err(|err| Error::engine(format!("git operation task failed: {err}")))?
}
