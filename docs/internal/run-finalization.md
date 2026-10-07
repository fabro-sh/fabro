# Required run finalization

Petri owns the durable run result. Fabro declares required finalization when
its hooks have a publisher and implements it in `FabroHooks::finalize_run`.
For successful workflow execution this prepares the final diff, then uses the
existing publisher to retry the push and reconcile or create the pull request.
A preparation or publication error rejects with `publish_failed` and a rendered
message. Missing branch, checkpoint or workspace evidence is a rejection.
Failed or cancelled execution skips publication; its diff remains best effort.
Local workflow run-end hooks remain observational.

The coordinator awaits finalization and cleanup before committing `run.finished`.
Until that record, the run stays active without a conclusion, even when every
stage has succeeded. The run stream, API projection, managed server status,
worker return and CLI verdict use the committed overall status and structured
finalization failure. Stage outcomes, metrics and invocation results keep their
execution meaning. Successful stages stay successful when publication fails.

Both HTTP and in-process worker transports store the finish before settling
managed status. A rejected append cannot settle the run. The later platform
terminal lifecycle record acknowledges the same outcome; it cannot replace a
terminal result or conclusion. Early worker failures without a Petri finish
still end through their committed platform lifecycle record. Run scope,
authorization and lease ownership checks remain required for every worker write.

## Recovery and stored history

This integration requires a Petri with required run finalization (coordinator
format 9) and event contract 6. Petri retains its strict stored-format policy: version-8 coordinator
logs cannot resume, inspect or replay with this engine. Fabro does not rewrite
source history or relax that policy. Existing materialized views and stream
rows remain stored; a projector replay failure holds Petri positions and reports
incomplete record health. A historical run without a materialized view cannot
recover its old Petri stage evidence through the new engine. Operators requiring
inspection or recovery of old execution logs must keep the prior compatible
binary and its storage backup.

Existing incorrect historical publication-success projections are not repaired
by this change. Consumed cursors and immutable conclusions remain unchanged;
historical-view repair requires a separate, bounded process over preserved
source records. No publication is invoked by projection or replay.

A committed resume returns the same overall result without publishing again.
A fork inherits the source run’s required-finalization declaration while seeding
its records; its worker restores the actual publication hooks before resume.
Unfinished recovery must restore the same finalization requirement. Petri may
call the restored finalizer again after interruption, including a crash after
the callback returns but before the terminal record commits. Existing GitHub
reconciliation is retained; this contract does not provide independent retry
orchestration or guarantee external-effect deduplication after every crash.
Lost terminal-execution workspaces are not recreated for finalization. Fabro
rejects publication when it cannot establish the required evidence. Push
attempts retain their existing five-minute timeout; this integration adds no
overall finalizer deadline. Cancellation arriving after
execution ends does not interrupt Petri's awaited finalizer.
