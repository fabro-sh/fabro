use ::fabro_types::{
    BilledTokenCounts, EventBody, RunControlAction, RunEvent, RunId, StageOutcome,
    run_event as fabro_types,
};
use chrono::Utc;
use fabro_agent::{AgentEvent, SandboxEvent};
use fabro_llm::types::TokenCounts as LlmTokenCounts;
use uuid::Uuid;

use super::Event;
use super::stored_fields::stored_event_fields;
use crate::stage_scope::StageScope;

fn billed_token_counts_from_llm(usage: &LlmTokenCounts) -> BilledTokenCounts {
    BilledTokenCounts {
        input_tokens:       usage.input_tokens,
        output_tokens:      usage.output_tokens,
        total_tokens:       usage.total_tokens(),
        reasoning_tokens:   usage.reasoning_tokens,
        cache_read_tokens:  usage.cache_read_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        total_usd_micros:   None,
    }
}

fn stage_status_from_string(status: &str) -> StageOutcome {
    status.parse().unwrap_or_else(|_| {
        tracing::warn!(
            status,
            "unknown stage status in StageCompleted event; using Fail"
        );
        StageOutcome::Failed {
            retry_requested: false,
        }
    })
}

fn event_body_from_event(event: &Event) -> EventBody {
    match event {
        Event::RunCreated {
            settings,
            graph,
            workflow_source,
            workflow_config,
            labels,
            run_dir,
            source_directory,
            workflow_slug,
            db_prefix,
            provenance,
            manifest_blob,
            git,
            fork_source_ref,
            in_place,
            web_url,
            ..
        } => EventBody::RunCreated(fabro_types::RunCreatedProps {
            settings:         serde_json::from_value(settings.clone())
                .expect("run.created settings"),
            graph:            serde_json::from_value(graph.clone()).expect("run.created graph"),
            workflow_source:  workflow_source.clone(),
            workflow_config:  workflow_config.clone(),
            labels:           labels.clone(),
            run_dir:          run_dir.clone(),
            source_directory: source_directory.clone(),
            workflow_slug:    workflow_slug.clone(),
            db_prefix:        db_prefix.clone(),
            provenance:       provenance.clone(),
            manifest_blob:    *manifest_blob,
            git:              git.clone(),
            fork_source_ref:  fork_source_ref.clone(),
            in_place:         *in_place,
            web_url:          web_url.clone(),
        }),
        Event::WorkflowRunStarted {
            name,
            base_branch,
            base_sha,
            run_branch,
            worktree_dir,
            goal,
            ..
        } => EventBody::RunStarted(fabro_types::RunStartedProps {
            name:         name.clone(),
            base_branch:  base_branch.clone(),
            base_sha:     base_sha.clone(),
            run_branch:   run_branch.clone(),
            worktree_dir: worktree_dir.clone(),
            goal:         goal.clone(),
        }),
        Event::RunSubmitted { definition_blob } => {
            EventBody::RunSubmitted(fabro_types::RunSubmittedProps {
                definition_blob: *definition_blob,
            })
        }
        Event::RunQueued => EventBody::RunQueued(fabro_types::RunStatusEffectProps::default()),
        Event::RunStarting => {
            EventBody::RunStarting(fabro_types::RunStatusTransitionProps::default())
        }
        Event::RunRunning => {
            EventBody::RunRunning(fabro_types::RunStatusTransitionProps::default())
        }
        Event::RunBlocked { blocked_reason } => {
            EventBody::RunBlocked(fabro_types::RunBlockedProps {
                blocked_reason: *blocked_reason,
            })
        }
        Event::RunUnblocked => {
            EventBody::RunUnblocked(fabro_types::RunStatusEffectProps::default())
        }
        Event::RunRemoving => {
            EventBody::RunRemoving(fabro_types::RunStatusTransitionProps::default())
        }
        Event::RunCancelRequested { .. } => {
            EventBody::RunCancelRequested(fabro_types::RunControlRequestedProps {
                action: RunControlAction::Cancel,
            })
        }
        Event::RunPauseRequested { .. } => {
            EventBody::RunPauseRequested(fabro_types::RunControlRequestedProps {
                action: RunControlAction::Pause,
            })
        }
        Event::RunUnpauseRequested { .. } => {
            EventBody::RunUnpauseRequested(fabro_types::RunControlRequestedProps {
                action: RunControlAction::Unpause,
            })
        }
        Event::RunPaused => EventBody::RunPaused(fabro_types::RunControlEffectProps::default()),
        Event::RunUnpaused => EventBody::RunUnpaused(fabro_types::RunControlEffectProps::default()),
        Event::RunSupersededBy {
            new_run_id,
            target_checkpoint_ordinal,
            target_node_id,
            target_visit,
        } => EventBody::RunSupersededBy(fabro_types::RunSupersededByProps {
            new_run_id:                *new_run_id,
            target_checkpoint_ordinal: *target_checkpoint_ordinal,
            target_node_id:            target_node_id.clone(),
            target_visit:              *target_visit,
        }),
        Event::RunArchived { .. } => {
            EventBody::RunArchived(fabro_types::RunArchivedProps::default())
        }
        Event::RunUnarchived { .. } => {
            EventBody::RunUnarchived(fabro_types::RunUnarchivedProps::default())
        }
        Event::WorkflowRunCompleted {
            duration_ms,
            artifact_count,
            status,
            reason,
            total_usd_micros,
            final_git_commit_sha,
            final_patch,
            billing,
        } => EventBody::RunCompleted(fabro_types::RunCompletedProps {
            duration_ms:          *duration_ms,
            artifact_count:       *artifact_count,
            status:               status.clone(),
            reason:               *reason,
            total_usd_micros:     *total_usd_micros,
            final_git_commit_sha: final_git_commit_sha.clone(),
            final_patch:          final_patch.clone(),
            billing:              billing.clone(),
        }),
        Event::WorkflowRunFailed {
            error,
            duration_ms,
            reason,
            git_commit_sha,
            final_patch,
        } => EventBody::RunFailed(fabro_types::RunFailedProps {
            error:          error.to_string(),
            causes:         error.causes(),
            duration_ms:    *duration_ms,
            reason:         *reason,
            git_commit_sha: git_commit_sha.clone(),
            final_patch:    final_patch.clone(),
        }),
        Event::RunNotice {
            level,
            code,
            message,
            exec_output_tail,
        } => EventBody::RunNotice(fabro_types::RunNoticeProps {
            level:            *level,
            code:             code.clone(),
            message:          message.clone(),
            exec_output_tail: exec_output_tail.clone(),
        }),
        Event::MetadataSnapshotStarted { phase, branch } => {
            EventBody::MetadataSnapshotStarted(fabro_types::MetadataSnapshotStartedProps {
                phase:  *phase,
                branch: branch.clone(),
            })
        }
        Event::MetadataSnapshotCompleted {
            phase,
            branch,
            duration_ms,
            entry_count,
            bytes,
            commit_sha,
        } => EventBody::MetadataSnapshotCompleted(fabro_types::MetadataSnapshotCompletedProps {
            phase:       *phase,
            branch:      branch.clone(),
            duration_ms: *duration_ms,
            entry_count: *entry_count,
            bytes:       *bytes,
            commit_sha:  commit_sha.clone(),
        }),
        Event::MetadataSnapshotFailed {
            phase,
            branch,
            duration_ms,
            failure_kind,
            error,
            causes,
            commit_sha,
            entry_count,
            bytes,
            exec_output_tail,
        } => EventBody::MetadataSnapshotFailed(fabro_types::MetadataSnapshotFailedProps {
            phase:            *phase,
            branch:           branch.clone(),
            duration_ms:      *duration_ms,
            failure_kind:     *failure_kind,
            error:            error.clone(),
            causes:           causes.clone(),
            commit_sha:       commit_sha.clone(),
            entry_count:      *entry_count,
            bytes:            *bytes,
            exec_output_tail: exec_output_tail.clone(),
        }),
        Event::StageStarted {
            index,
            handler_type,
            attempt,
            max_attempts,
            ..
        } => EventBody::StageStarted(fabro_types::StageStartedProps {
            index:        *index,
            handler_type: handler_type.clone(),
            attempt:      *attempt,
            max_attempts: *max_attempts,
        }),
        Event::StageCompleted {
            index,
            duration_ms,
            status,
            preferred_label,
            suggested_next_ids,
            billing,
            failure,
            notes,
            files_touched,
            context_updates,
            jump_to_node,
            context_values,
            node_visits,
            loop_failure_signatures,
            restart_failure_signatures,
            response,
            attempt,
            max_attempts,
            ..
        } => EventBody::StageCompleted(fabro_types::StageCompletedProps {
            index: *index,
            duration_ms: *duration_ms,
            status: stage_status_from_string(status),
            preferred_label: preferred_label.clone(),
            suggested_next_ids: suggested_next_ids.clone(),
            billing: billing.clone(),
            failure: failure.clone(),
            notes: notes.clone(),
            files_touched: files_touched.clone(),
            context_updates: context_updates.clone(),
            jump_to_node: jump_to_node.clone(),
            context_values: context_values.clone(),
            node_visits: node_visits.clone(),
            loop_failure_signatures: loop_failure_signatures.clone(),
            restart_failure_signatures: restart_failure_signatures.clone(),
            response: response.clone(),
            attempt: *attempt,
            max_attempts: *max_attempts,
        }),
        Event::StageFailed {
            index,
            failure,
            will_retry,
            duration_ms,
            billing,
            ..
        } => EventBody::StageFailed(fabro_types::StageFailedProps {
            index:       *index,
            failure:     Some(failure.clone()),
            will_retry:  *will_retry,
            duration_ms: *duration_ms,
            billing:     billing.clone(),
        }),
        Event::StageRetrying {
            index,
            attempt,
            max_attempts,
            delay_ms,
            ..
        } => EventBody::StageRetrying(fabro_types::StageRetryingProps {
            index:        *index,
            attempt:      *attempt,
            max_attempts: *max_attempts,
            delay_ms:     *delay_ms,
        }),
        Event::ParallelStarted {
            visit,
            branch_count,
            join_policy,
            ..
        } => EventBody::ParallelStarted(fabro_types::ParallelStartedProps {
            visit:        *visit,
            branch_count: *branch_count,
            join_policy:  join_policy.clone(),
        }),
        Event::ParallelBranchStarted { index, .. } => {
            EventBody::ParallelBranchStarted(fabro_types::ParallelBranchStartedProps {
                index: *index,
            })
        }
        Event::ParallelBranchCompleted {
            index,
            duration_ms,
            status,
            head_sha,
            ..
        } => EventBody::ParallelBranchCompleted(fabro_types::ParallelBranchCompletedProps {
            index:       *index,
            duration_ms: *duration_ms,
            status:      status.clone(),
            head_sha:    head_sha.clone(),
        }),
        Event::ParallelCompleted {
            visit,
            duration_ms,
            success_count,
            failure_count,
            results,
            ..
        } => EventBody::ParallelCompleted(fabro_types::ParallelCompletedProps {
            visit:         *visit,
            duration_ms:   *duration_ms,
            success_count: *success_count,
            failure_count: *failure_count,
            results:       results.clone(),
        }),
        Event::InterviewStarted {
            question_id,
            question,
            stage,
            question_type,
            options,
            allow_freeform,
            timeout_seconds,
            context_display,
        } => EventBody::InterviewStarted(fabro_types::InterviewStartedProps {
            question_id:     question_id.clone(),
            question:        question.clone(),
            stage:           stage.clone(),
            question_type:   question_type.clone(),
            options:         options.clone(),
            allow_freeform:  *allow_freeform,
            timeout_seconds: *timeout_seconds,
            context_display: context_display.clone(),
        }),
        Event::InterviewCompleted {
            actor: _,
            question_id,
            question,
            answer,
            duration_ms,
        } => EventBody::InterviewCompleted(fabro_types::InterviewCompletedProps {
            question_id: question_id.clone(),
            question:    question.clone(),
            answer:      answer.clone(),
            duration_ms: *duration_ms,
        }),
        Event::InterviewTimeout {
            actor: _,
            question_id,
            question,
            stage,
            duration_ms,
        } => EventBody::InterviewTimeout(fabro_types::InterviewTimeoutProps {
            question_id: question_id.clone(),
            question:    question.clone(),
            stage:       stage.clone(),
            duration_ms: *duration_ms,
        }),
        Event::InterviewInterrupted {
            actor: _,
            question_id,
            question,
            stage,
            reason,
            duration_ms,
        } => EventBody::InterviewInterrupted(fabro_types::InterviewInterruptedProps {
            question_id: question_id.clone(),
            question:    question.clone(),
            stage:       stage.clone(),
            reason:      reason.clone(),
            duration_ms: *duration_ms,
        }),
        Event::CheckpointCompleted {
            status,
            current_node,
            completed_nodes,
            node_retries,
            context_values,
            node_outcomes,
            next_node_id,
            git_commit_sha,
            loop_failure_signatures,
            restart_failure_signatures,
            node_visits,
            diff,
            ..
        } => EventBody::CheckpointCompleted(fabro_types::CheckpointCompletedProps {
            status: status.clone(),
            current_node: current_node.clone(),
            completed_nodes: completed_nodes.clone(),
            node_retries: node_retries.clone(),
            context_values: context_values.clone(),
            node_outcomes: node_outcomes.clone(),
            next_node_id: next_node_id.clone(),
            git_commit_sha: git_commit_sha.clone(),
            loop_failure_signatures: loop_failure_signatures.clone(),
            restart_failure_signatures: restart_failure_signatures.clone(),
            node_visits: node_visits.clone(),
            diff: diff.clone(),
        }),
        Event::CheckpointFailed {
            error,
            exec_output_tail,
            ..
        } => EventBody::CheckpointFailed(fabro_types::CheckpointFailedProps {
            error:            error.clone(),
            exec_output_tail: exec_output_tail.clone(),
        }),
        Event::GitCommit { sha, .. } => {
            EventBody::GitCommit(fabro_types::GitCommitProps { sha: sha.clone() })
        }
        Event::GitPush {
            branch,
            success,
            exec_output_tail,
        } => EventBody::GitPush(fabro_types::GitPushProps {
            branch:           branch.clone(),
            success:          *success,
            exec_output_tail: exec_output_tail.clone(),
        }),
        Event::GitBranch { branch, sha } => EventBody::GitBranch(fabro_types::GitBranchProps {
            branch: branch.clone(),
            sha:    sha.clone(),
        }),
        Event::GitWorktreeAdd { path, branch } => {
            EventBody::GitWorktreeAdd(fabro_types::GitWorktreeAddProps {
                path:   path.clone(),
                branch: branch.clone(),
            })
        }
        Event::GitWorktreeRemove { path } => {
            EventBody::GitWorktreeRemove(fabro_types::GitWorktreeRemoveProps { path: path.clone() })
        }
        Event::GitFetch { branch, success } => EventBody::GitFetch(fabro_types::GitFetchProps {
            branch:  branch.clone(),
            success: *success,
        }),
        Event::GitReset { sha } => {
            EventBody::GitReset(fabro_types::GitResetProps { sha: sha.clone() })
        }
        Event::EdgeSelected {
            from_node,
            to_node,
            label,
            condition,
            reason,
            preferred_label,
            suggested_next_ids,
            stage_status,
            is_jump,
        } => EventBody::EdgeSelected(fabro_types::EdgeSelectedProps {
            from_node:          from_node.clone(),
            to_node:            to_node.clone(),
            label:              label.clone(),
            condition:          condition.clone(),
            reason:             reason.clone(),
            preferred_label:    preferred_label.clone(),
            suggested_next_ids: suggested_next_ids.clone(),
            stage_status:       stage_status.clone(),
            is_jump:            *is_jump,
        }),
        Event::LoopRestart { from_node, to_node } => {
            EventBody::LoopRestart(fabro_types::LoopRestartProps {
                from_node: from_node.clone(),
                to_node:   to_node.clone(),
            })
        }
        Event::Prompt {
            visit,
            text,
            mode,
            provider,
            model,
            ..
        } => EventBody::StagePrompt(fabro_types::StagePromptProps {
            visit:    *visit,
            text:     text.clone(),
            mode:     mode.clone(),
            provider: provider.clone(),
            model:    model.clone(),
        }),
        Event::PromptCompleted {
            response,
            model,
            provider,
            billing,
            ..
        } => EventBody::PromptCompleted(fabro_types::PromptCompletedProps {
            response: response.clone(),
            model:    model.clone(),
            provider: provider.clone(),
            billing:  billing.clone(),
        }),
        Event::Agent { visit, event, .. } => match event {
            AgentEvent::SessionStarted { provider, model } => {
                EventBody::AgentSessionStarted(fabro_types::AgentSessionStartedProps {
                    provider: provider.clone(),
                    model:    model.clone(),
                    visit:    *visit,
                })
            }
            AgentEvent::SessionEnded => {
                EventBody::AgentSessionEnded(fabro_types::AgentSessionEndedProps { visit: *visit })
            }
            AgentEvent::ProcessingEnd => {
                EventBody::AgentProcessingEnd(fabro_types::AgentProcessingEndProps {
                    visit: *visit,
                })
            }
            AgentEvent::UserInput { text } => EventBody::AgentInput(fabro_types::AgentInputProps {
                text:  text.clone(),
                visit: *visit,
            }),
            AgentEvent::AssistantMessage {
                text,
                model,
                usage,
                tool_call_count,
            } => EventBody::AgentMessage(fabro_types::AgentMessageProps {
                text:            text.clone(),
                model:           model.clone(),
                billing:         billed_token_counts_from_llm(usage),
                tool_call_count: *tool_call_count,
                visit:           *visit,
            }),
            AgentEvent::ToolCallStarted {
                tool_name,
                tool_call_id,
                arguments,
            } => EventBody::AgentToolStarted(fabro_types::AgentToolStartedProps {
                tool_name:    tool_name.clone(),
                tool_call_id: tool_call_id.clone(),
                arguments:    arguments.clone(),
                visit:        *visit,
            }),
            AgentEvent::ToolCallCompleted {
                tool_name,
                tool_call_id,
                output,
                is_error,
            } => EventBody::AgentToolCompleted(fabro_types::AgentToolCompletedProps {
                tool_name:    tool_name.clone(),
                tool_call_id: tool_call_id.clone(),
                output:       output.clone(),
                is_error:     *is_error,
                visit:        *visit,
            }),
            AgentEvent::Error { error } => EventBody::AgentError(fabro_types::AgentErrorProps {
                error: serde_json::to_value(error).expect("serializable agent error"),
                visit: *visit,
            }),
            AgentEvent::Warning {
                kind,
                message,
                details,
            } => EventBody::AgentWarning(fabro_types::AgentWarningProps {
                kind:    kind.clone(),
                message: message.clone(),
                details: details.clone(),
                visit:   *visit,
            }),
            AgentEvent::LoopDetected => {
                EventBody::AgentLoopDetected(fabro_types::AgentLoopDetectedProps { visit: *visit })
            }
            AgentEvent::TurnLimitReached { max_turns } => {
                EventBody::AgentTurnLimitReached(fabro_types::AgentTurnLimitReachedProps {
                    max_turns: *max_turns,
                    visit:     *visit,
                })
            }
            AgentEvent::SteeringInjected { text } => {
                EventBody::AgentSteeringInjected(fabro_types::AgentSteeringInjectedProps {
                    text:  text.clone(),
                    visit: *visit,
                })
            }
            AgentEvent::CompactionStarted {
                estimated_tokens,
                context_window_size,
            } => EventBody::AgentCompactionStarted(fabro_types::AgentCompactionStartedProps {
                estimated_tokens:    *estimated_tokens,
                context_window_size: *context_window_size,
                visit:               *visit,
            }),
            AgentEvent::CompactionCompleted {
                original_turn_count,
                preserved_turn_count,
                summary_token_estimate,
                tracked_file_count,
            } => EventBody::AgentCompactionCompleted(fabro_types::AgentCompactionCompletedProps {
                original_turn_count:    *original_turn_count,
                preserved_turn_count:   *preserved_turn_count,
                summary_token_estimate: *summary_token_estimate,
                tracked_file_count:     *tracked_file_count,
                visit:                  *visit,
            }),
            AgentEvent::LlmRetry {
                provider,
                model,
                attempt,
                delay_secs,
                error,
            } => EventBody::AgentLlmRetry(fabro_types::AgentLlmRetryProps {
                provider:   provider.clone(),
                model:      model.clone(),
                attempt:    *attempt,
                delay_secs: *delay_secs,
                error:      serde_json::to_value(error).expect("serializable sdk error"),
                visit:      *visit,
            }),
            AgentEvent::SubAgentSpawned {
                agent_id,
                depth,
                task,
            } => EventBody::AgentSubSpawned(fabro_types::AgentSubSpawnedProps {
                agent_id: agent_id.clone(),
                depth:    *depth,
                task:     task.clone(),
                visit:    *visit,
            }),
            AgentEvent::SubAgentCompleted {
                agent_id,
                depth,
                success,
                turns_used,
            } => EventBody::AgentSubCompleted(fabro_types::AgentSubCompletedProps {
                agent_id:   agent_id.clone(),
                depth:      *depth,
                success:    *success,
                turns_used: *turns_used,
                visit:      *visit,
            }),
            AgentEvent::SubAgentFailed {
                agent_id,
                depth,
                error,
            } => EventBody::AgentSubFailed(fabro_types::AgentSubFailedProps {
                agent_id: agent_id.clone(),
                depth:    *depth,
                error:    serde_json::to_value(error).expect("serializable agent error"),
                visit:    *visit,
            }),
            AgentEvent::SubAgentClosed { agent_id, depth } => {
                EventBody::AgentSubClosed(fabro_types::AgentSubClosedProps {
                    agent_id: agent_id.clone(),
                    depth:    *depth,
                    visit:    *visit,
                })
            }
            AgentEvent::McpServerReady {
                server_name,
                tool_count,
            } => EventBody::AgentMcpReady(fabro_types::AgentMcpReadyProps {
                server_name: server_name.clone(),
                tool_count:  *tool_count,
                visit:       *visit,
            }),
            AgentEvent::McpServerFailed { server_name, error } => {
                EventBody::AgentMcpFailed(fabro_types::AgentMcpFailedProps {
                    server_name: server_name.clone(),
                    error:       error.clone(),
                    visit:       *visit,
                })
            }
            AgentEvent::AssistantTextStart
            | AgentEvent::AssistantOutputReplace { .. }
            | AgentEvent::TextDelta { .. }
            | AgentEvent::ReasoningDelta { .. }
            | AgentEvent::ToolCallOutputDelta { .. }
            | AgentEvent::SkillExpanded { .. } => {
                panic!("streaming-noise agent event should not be converted to RunEvent")
            }
        },
        Event::SubgraphStarted { start_node, .. } => {
            EventBody::SubgraphStarted(fabro_types::SubgraphStartedProps {
                start_node: start_node.clone(),
            })
        }
        Event::SubgraphCompleted {
            steps_executed,
            status,
            duration_ms,
            ..
        } => EventBody::SubgraphCompleted(fabro_types::SubgraphCompletedProps {
            steps_executed: *steps_executed,
            status:         status.clone(),
            duration_ms:    *duration_ms,
        }),
        Event::Sandbox { event } => match event {
            SandboxEvent::Initializing { provider } => {
                EventBody::SandboxInitializing(fabro_types::SandboxInitializingProps {
                    provider: provider.clone(),
                })
            }
            SandboxEvent::Ready {
                provider,
                duration_ms,
                name,
                cpu,
                memory,
                url,
            } => EventBody::SandboxReady(fabro_types::SandboxReadyProps {
                provider:    provider.clone(),
                duration_ms: *duration_ms,
                name:        name.clone(),
                cpu:         *cpu,
                memory:      *memory,
                url:         url.clone(),
            }),
            SandboxEvent::InitializeFailed {
                provider,
                error,
                causes,
                duration_ms,
            } => EventBody::SandboxFailed(fabro_types::SandboxFailedProps {
                provider:    provider.clone(),
                error:       error.clone(),
                causes:      causes.clone(),
                duration_ms: *duration_ms,
            }),
            SandboxEvent::CleanupStarted { provider } => {
                EventBody::SandboxCleanupStarted(fabro_types::SandboxCleanupStartedProps {
                    provider: provider.clone(),
                })
            }
            SandboxEvent::CleanupCompleted {
                provider,
                duration_ms,
            } => EventBody::SandboxCleanupCompleted(fabro_types::SandboxCleanupCompletedProps {
                provider:    provider.clone(),
                duration_ms: *duration_ms,
            }),
            SandboxEvent::CleanupFailed {
                provider,
                error,
                causes,
            } => EventBody::SandboxCleanupFailed(fabro_types::SandboxCleanupFailedProps {
                provider: provider.clone(),
                error:    error.clone(),
                causes:   causes.clone(),
            }),
            SandboxEvent::SnapshotPulling { name } => {
                EventBody::SnapshotPulling(fabro_types::SnapshotNameProps { name: name.clone() })
            }
            SandboxEvent::SnapshotCreating { name } => {
                EventBody::SnapshotCreating(fabro_types::SnapshotNameProps { name: name.clone() })
            }
            SandboxEvent::SnapshotReady { name, duration_ms } => {
                EventBody::SnapshotReady(fabro_types::SnapshotCompletedProps {
                    name:        name.clone(),
                    duration_ms: *duration_ms,
                })
            }
            SandboxEvent::SnapshotFailed {
                name,
                error,
                causes,
            } => EventBody::SnapshotFailed(fabro_types::SnapshotFailedProps {
                name:   name.clone(),
                error:  error.clone(),
                causes: causes.clone(),
            }),
            SandboxEvent::GitCloneStarted { url, branch } => {
                EventBody::GitCloneStarted(fabro_types::GitCloneStartedProps {
                    url:    url.clone(),
                    branch: branch.clone(),
                })
            }
            SandboxEvent::GitCloneCompleted { url, duration_ms } => {
                EventBody::GitCloneCompleted(fabro_types::GitCloneCompletedProps {
                    url:         url.clone(),
                    duration_ms: *duration_ms,
                })
            }
            SandboxEvent::GitCloneFailed { url, error, causes } => {
                EventBody::GitCloneFailed(fabro_types::GitCloneFailedProps {
                    url:    url.clone(),
                    error:  error.clone(),
                    causes: causes.clone(),
                })
            }
        },
        Event::SandboxInitialized {
            working_directory,
            provider,
            identifier,
            repo_cloned,
            clone_origin_url,
            clone_branch,
        } => EventBody::SandboxInitialized(fabro_types::SandboxInitializedProps {
            working_directory: working_directory.clone(),
            provider:          provider.clone(),
            identifier:        identifier.clone(),
            repo_cloned:       *repo_cloned,
            clone_origin_url:  clone_origin_url.clone(),
            clone_branch:      clone_branch.clone(),
        }),
        Event::SetupStarted { command_count } => {
            EventBody::SetupStarted(fabro_types::SetupStartedProps {
                command_count: *command_count,
            })
        }
        Event::SetupCommandStarted { command, index } => {
            EventBody::SetupCommandStarted(fabro_types::SetupCommandStartedProps {
                command: command.clone(),
                index:   *index,
            })
        }
        Event::SetupCommandCompleted {
            command,
            index,
            exit_code,
            duration_ms,
        } => EventBody::SetupCommandCompleted(fabro_types::SetupCommandCompletedProps {
            command:     command.clone(),
            index:       *index,
            exit_code:   *exit_code,
            duration_ms: *duration_ms,
        }),
        Event::SetupCompleted { duration_ms } => {
            EventBody::SetupCompleted(fabro_types::SetupCompletedProps {
                duration_ms: *duration_ms,
            })
        }
        Event::SetupFailed {
            command,
            index,
            exit_code,
            stderr,
            exec_output_tail,
        } => EventBody::SetupFailed(fabro_types::SetupFailedProps {
            command:          command.clone(),
            index:            *index,
            exit_code:        *exit_code,
            stderr:           stderr.clone(),
            exec_output_tail: exec_output_tail.clone(),
        }),
        Event::StallWatchdogTimeout { idle_seconds, .. } => {
            EventBody::StallWatchdogTimeout(fabro_types::StallWatchdogTimeoutProps {
                idle_seconds: *idle_seconds,
            })
        }
        Event::ArtifactCaptured {
            attempt,
            node_slug,
            path,
            mime,
            content_md5,
            content_sha256,
            bytes,
            ..
        } => EventBody::ArtifactCaptured(fabro_types::ArtifactCapturedProps {
            attempt:        *attempt,
            node_slug:      node_slug.clone(),
            path:           path.clone(),
            mime:           mime.clone(),
            content_md5:    content_md5.clone(),
            content_sha256: content_sha256.clone(),
            bytes:          *bytes,
        }),
        Event::SshAccessReady { ssh_command } => {
            EventBody::SshAccessReady(fabro_types::SshAccessReadyProps {
                ssh_command: ssh_command.clone(),
            })
        }
        Event::Failover {
            from_provider,
            from_model,
            to_provider,
            to_model,
            error,
            ..
        } => EventBody::Failover(fabro_types::FailoverProps {
            from_provider: from_provider.clone(),
            from_model:    from_model.clone(),
            to_provider:   to_provider.clone(),
            to_model:      to_model.clone(),
            error:         error.clone(),
        }),
        Event::CliEnsureStarted { cli_name, provider } => {
            EventBody::CliEnsureStarted(fabro_types::CliEnsureStartedProps {
                cli_name: cli_name.clone(),
                provider: provider.clone(),
            })
        }
        Event::CliEnsureCompleted {
            cli_name,
            provider,
            already_installed,
            node_installed,
            duration_ms,
        } => EventBody::CliEnsureCompleted(fabro_types::CliEnsureCompletedProps {
            cli_name:          cli_name.clone(),
            provider:          provider.clone(),
            already_installed: *already_installed,
            node_installed:    *node_installed,
            duration_ms:       *duration_ms,
        }),
        Event::CliEnsureFailed {
            cli_name,
            provider,
            error,
            duration_ms,
            exec_output_tail,
        } => EventBody::CliEnsureFailed(fabro_types::CliEnsureFailedProps {
            cli_name:         cli_name.clone(),
            provider:         provider.clone(),
            error:            error.clone(),
            duration_ms:      *duration_ms,
            exec_output_tail: exec_output_tail.clone(),
        }),
        Event::CommandStarted {
            script,
            command,
            language,
            timeout_ms,
            ..
        } => EventBody::CommandStarted(fabro_types::CommandStartedProps {
            script:     script.clone(),
            command:    command.clone(),
            language:   language.clone(),
            timeout_ms: *timeout_ms,
        }),
        Event::CommandCompleted {
            stdout,
            stderr,
            exit_code,
            duration_ms,
            termination,
            stdout_bytes,
            stderr_bytes,
            streams_separated,
            live_streaming,
            ..
        } => EventBody::CommandCompleted(fabro_types::CommandCompletedProps {
            stdout:            stdout.clone(),
            stderr:            stderr.clone(),
            exit_code:         *exit_code,
            duration_ms:       *duration_ms,
            termination:       *termination,
            stdout_bytes:      *stdout_bytes,
            stderr_bytes:      *stderr_bytes,
            streams_separated: *streams_separated,
            live_streaming:    *live_streaming,
        }),
        Event::AgentCliStarted {
            visit,
            mode,
            provider,
            model,
            command,
            ..
        } => EventBody::AgentCliStarted(fabro_types::AgentCliStartedProps {
            visit:    *visit,
            mode:     mode.clone(),
            provider: provider.clone(),
            model:    model.clone(),
            command:  command.clone(),
        }),
        Event::AgentCliCompleted {
            stdout,
            stderr,
            exit_code,
            duration_ms,
            ..
        } => EventBody::AgentCliCompleted(fabro_types::AgentCliCompletedProps {
            stdout:      stdout.clone(),
            stderr:      stderr.clone(),
            exit_code:   *exit_code,
            duration_ms: *duration_ms,
        }),
        Event::AgentCliCancelled {
            stdout,
            stderr,
            duration_ms,
            ..
        } => EventBody::AgentCliCancelled(fabro_types::AgentCliCancelledProps {
            stdout:      stdout.clone(),
            stderr:      stderr.clone(),
            duration_ms: *duration_ms,
        }),
        Event::AgentCliTimedOut {
            stdout,
            stderr,
            duration_ms,
            ..
        } => EventBody::AgentCliTimedOut(fabro_types::AgentCliTimedOutProps {
            stdout:      stdout.clone(),
            stderr:      stderr.clone(),
            duration_ms: *duration_ms,
        }),
        Event::PullRequestCreated {
            pr_url,
            pr_number,
            owner,
            repo,
            base_branch,
            head_branch,
            title,
            draft,
        } => EventBody::PullRequestCreated(fabro_types::PullRequestCreatedProps {
            pr_url:      pr_url.clone(),
            pr_number:   *pr_number,
            owner:       owner.clone(),
            repo:        repo.clone(),
            base_branch: base_branch.clone(),
            head_branch: head_branch.clone(),
            title:       title.clone(),
            draft:       *draft,
        }),
        Event::PullRequestFailed { error } => {
            EventBody::PullRequestFailed(fabro_types::PullRequestFailedProps {
                error: error.clone(),
            })
        }
        Event::DevcontainerResolved {
            dockerfile_lines,
            environment_count,
            lifecycle_command_count,
            workspace_folder,
        } => EventBody::DevcontainerResolved(fabro_types::DevcontainerResolvedProps {
            dockerfile_lines:        *dockerfile_lines,
            environment_count:       *environment_count,
            lifecycle_command_count: *lifecycle_command_count,
            workspace_folder:        workspace_folder.clone(),
        }),
        Event::DevcontainerLifecycleStarted {
            phase,
            command_count,
        } => EventBody::DevcontainerLifecycleStarted(
            fabro_types::DevcontainerLifecycleStartedProps {
                phase:         phase.clone(),
                command_count: *command_count,
            },
        ),
        Event::DevcontainerLifecycleCommandStarted {
            phase,
            command,
            index,
        } => EventBody::DevcontainerLifecycleCommandStarted(
            fabro_types::DevcontainerLifecycleCommandStartedProps {
                phase:   phase.clone(),
                command: command.clone(),
                index:   *index,
            },
        ),
        Event::DevcontainerLifecycleCommandCompleted {
            phase,
            command,
            index,
            exit_code,
            duration_ms,
        } => EventBody::DevcontainerLifecycleCommandCompleted(
            fabro_types::DevcontainerLifecycleCommandCompletedProps {
                phase:       phase.clone(),
                command:     command.clone(),
                index:       *index,
                exit_code:   *exit_code,
                duration_ms: *duration_ms,
            },
        ),
        Event::DevcontainerLifecycleCompleted { phase, duration_ms } => {
            EventBody::DevcontainerLifecycleCompleted(
                fabro_types::DevcontainerLifecycleCompletedProps {
                    phase:       phase.clone(),
                    duration_ms: *duration_ms,
                },
            )
        }
        Event::DevcontainerLifecycleFailed {
            phase,
            command,
            index,
            exit_code,
            stderr,
            exec_output_tail,
        } => {
            EventBody::DevcontainerLifecycleFailed(fabro_types::DevcontainerLifecycleFailedProps {
                phase:            phase.clone(),
                command:          command.clone(),
                index:            *index,
                exit_code:        *exit_code,
                stderr:           stderr.clone(),
                exec_output_tail: exec_output_tail.clone(),
            })
        }
        Event::RetroStarted {
            prompt,
            provider,
            model,
        } => EventBody::RetroStarted(fabro_types::RetroStartedProps {
            prompt:   prompt.clone(),
            provider: provider.clone(),
            model:    model.clone(),
        }),
        Event::RetroCompleted {
            duration_ms,
            response,
            retro,
        } => EventBody::RetroCompleted(fabro_types::RetroCompletedProps {
            duration_ms: *duration_ms,
            response:    response.clone(),
            retro:       retro.clone(),
        }),
        Event::RetroFailed {
            error,
            duration_ms,
            exec_output_tail,
        } => EventBody::RetroFailed(fabro_types::RetroFailedProps {
            error:            error.clone(),
            duration_ms:      *duration_ms,
            exec_output_tail: exec_output_tail.clone(),
        }),
    }
}

#[must_use]
pub fn to_run_event(run_id: &RunId, event: &Event) -> RunEvent {
    to_run_event_at(run_id, event, Utc::now(), None)
}

#[must_use]
pub fn to_run_event_at(
    run_id: &RunId,
    event: &Event,
    ts: chrono::DateTime<Utc>,
    scope: Option<&StageScope>,
) -> RunEvent {
    let fields = stored_event_fields(event, scope);
    let body = event_body_from_event(event);
    RunEvent {
        id: Uuid::now_v7().to_string(),
        ts,
        run_id: *run_id,
        node_id: fields.node_id,
        node_label: fields.node_label,
        stage_id: fields.stage_id,
        parallel_group_id: fields.parallel_group_id,
        parallel_branch_id: fields.parallel_branch_id,
        session_id: fields.session_id,
        parent_session_id: fields.parent_session_id,
        tool_call_id: fields.tool_call_id,
        actor: fields.actor,
        body,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ::fabro_types::{
        EventBody, FailureReason, ParallelBranchId, Principal, RunNoticeCode, RunNoticeLevel,
        RunProvenance, StageId, SystemActorKind, fixtures, run_event as fabro_types,
    };
    use chrono::Utc;
    use fabro_agent::{AgentEvent, SandboxEvent};
    use fabro_llm::types::TokenCounts as LlmTokenCounts;

    use super::*;
    use crate::error::Error;
    use crate::event::test_support::user_principal;
    use crate::event::{Event, StageScope};
    use crate::outcome::{BilledModelUsage, FailureDetail};

    #[derive(Debug)]
    struct EventTestCause;

    impl std::fmt::Display for EventTestCause {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("connection refused")
        }
    }

    impl std::error::Error for EventTestCause {}

    fn exec_tail() -> fabro_types::ExecOutputTail {
        fabro_types::ExecOutputTail {
            stdout:           Some("last stdout line".to_string()),
            stderr:           Some("last stderr line".to_string()),
            stdout_truncated: false,
            stderr_truncated: true,
        }
    }

    fn test_usage(model_id: &str, input_tokens: i64, output_tokens: i64) -> BilledModelUsage {
        serde_json::from_value(serde_json::json!({
            "input": {
                "usage": {
                    "model": {
                        "provider": "openai",
                        "model_id": model_id
                    },
                    "tokens": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens
                    }
                },
                "facts": {
                    "provider": "open_ai"
                }
            },
            "total_usd_micros": input_tokens + output_tokens
        }))
        .unwrap()
    }

    #[test]
    fn run_event_stage_completed_places_node_fields_in_header() {
        let stored = to_run_event_at(
            &fixtures::RUN_2,
            &Event::StageCompleted {
                node_id: "plan".to_string(),
                name: "Plan".to_string(),
                index: 0,
                duration_ms: 5000,
                status: "succeeded".to_string(),
                preferred_label: None,
                suggested_next_ids: Vec::new(),
                billing: None,
                failure: None,
                notes: None,
                files_touched: Vec::new(),
                context_updates: None,
                jump_to_node: None,
                context_values: None,
                node_visits: None,
                loop_failure_signatures: None,
                restart_failure_signatures: None,
                response: None,
                attempt: 1,
                max_attempts: 1,
            },
            Utc::now(),
            Some(&StageScope {
                node_id:            "plan".to_string(),
                visit:              1,
                parallel_group_id:  None,
                parallel_branch_id: None,
            }),
        );

        assert_eq!(stored.event_name(), "stage.completed");
        assert_eq!(stored.run_id, fixtures::RUN_2);
        assert_eq!(stored.node_id.as_deref(), Some("plan"));
        assert_eq!(stored.node_label.as_deref(), Some("Plan"));
        assert_eq!(stored.stage_id, Some(StageId::new("plan", 1)));
        let properties = stored.properties().unwrap();
        assert_eq!(properties["duration_ms"], 5000);
        assert_eq!(properties["status"], "succeeded");
        assert!(stored.session_id.is_none());
    }

    #[test]
    fn run_event_stage_completed_keeps_response_and_signature_snapshots() {
        let stored = to_run_event(&fixtures::RUN_2, &Event::StageCompleted {
            node_id: "plan".to_string(),
            name: "Plan".to_string(),
            index: 0,
            duration_ms: 5000,
            status: "succeeded".to_string(),
            preferred_label: None,
            suggested_next_ids: Vec::new(),
            billing: None,
            failure: None,
            notes: None,
            files_touched: Vec::new(),
            context_updates: None,
            jump_to_node: None,
            context_values: None,
            node_visits: None,
            loop_failure_signatures: Some(BTreeMap::from([("sig-a".to_string(), 2usize)])),
            restart_failure_signatures: Some(BTreeMap::from([("sig-b".to_string(), 1usize)])),
            response: Some("done".to_string()),
            attempt: 1,
            max_attempts: 1,
        });

        let properties = stored.properties().unwrap();
        assert_eq!(properties["response"], "done");
        assert_eq!(properties["loop_failure_signatures"]["sig-a"], 2);
        assert_eq!(properties["restart_failure_signatures"]["sig-b"], 1);
    }

    #[test]
    fn run_event_stage_failure_keeps_failure_detail() {
        let usage = test_usage("gpt-5.2", 321, 54);
        let stored = to_run_event(&fixtures::RUN_3, &Event::StageFailed {
            node_id:     "code".to_string(),
            name:        "Code".to_string(),
            index:       1,
            failure:     FailureDetail::new(
                "lint failed",
                crate::outcome::FailureCategory::Deterministic,
            ),
            will_retry:  true,
            duration_ms: 5000,
            billing:     Some(usage.clone()),
            actor:       None,
        });

        assert_eq!(stored.event_name(), "stage.failed");
        let properties = stored.properties().unwrap();
        assert_eq!(properties["failure"]["message"], "lint failed");
        assert_eq!(properties["failure"]["failure_class"], "deterministic");
        assert_eq!(properties["will_retry"], true);
        assert_eq!(properties["billing"], serde_json::to_value(&usage).unwrap());
    }

    #[test]
    fn run_event_agent_tool_started_moves_session_metadata_to_header() {
        let stored = to_run_event(&fixtures::RUN_4, &Event::Agent {
            stage:             "code".to_string(),
            visit:             2,
            event:             AgentEvent::ToolCallStarted {
                tool_name:    "read_file".to_string(),
                tool_call_id: "call_1".to_string(),
                arguments:    serde_json::json!({"path": "src/main.rs"}),
            },
            session_id:        Some("ses_child".to_string()),
            parent_session_id: Some("ses_parent".to_string()),
        });

        assert_eq!(stored.event_name(), "agent.tool.started");
        assert_eq!(stored.node_id.as_deref(), Some("code"));
        assert_eq!(stored.node_label.as_deref(), Some("code"));
        assert_eq!(stored.session_id.as_deref(), Some("ses_child"));
        assert_eq!(stored.parent_session_id.as_deref(), Some("ses_parent"));
        let properties = stored.properties().unwrap();
        assert_eq!(properties["tool_name"], "read_file");
        assert_eq!(properties["tool_call_id"], "call_1");
        assert_eq!(properties["visit"], 2);
    }

    #[test]
    fn run_event_sandbox_event_keeps_properties_nested() {
        let stored = to_run_event(&fixtures::RUN_5, &Event::Sandbox {
            event: SandboxEvent::Ready {
                provider:    "daytona".to_string(),
                duration_ms: 2500,
                name:        Some("sandbox-1".to_string()),
                cpu:         Some(4.0),
                memory:      Some(8.0),
                url:         Some("https://example.test".to_string()),
            },
        });

        assert_eq!(stored.event_name(), "sandbox.ready");
        assert!(stored.node_id.is_none());
        let properties = stored.properties().unwrap();
        assert_eq!(properties["provider"], "daytona");
        assert_eq!(properties["duration_ms"], 2500);
    }

    #[test]
    fn run_event_sandbox_failure_serializes_causes() {
        let stored = to_run_event(&fixtures::RUN_5, &Event::Sandbox {
            event: SandboxEvent::InitializeFailed {
                provider:    "docker".to_string(),
                error:       "Failed to pull Docker image buildpack-deps:noble".to_string(),
                causes:      vec!["connection refused".to_string()],
                duration_ms: 42,
            },
        });

        assert_eq!(stored.event_name(), "sandbox.failed");
        let properties = stored.properties().unwrap();
        assert_eq!(properties["provider"], "docker");
        assert_eq!(
            properties["error"],
            "Failed to pull Docker image buildpack-deps:noble"
        );
        assert_eq!(
            properties["causes"],
            serde_json::json!(["connection refused"])
        );
    }

    #[test]
    fn run_event_workflow_failure_uses_display_error() {
        let stored = to_run_event(&fixtures::RUN_6, &Event::WorkflowRunFailed {
            error:          Error::handler("boom"),
            duration_ms:    900,
            reason:         FailureReason::WorkflowError,
            git_commit_sha: Some("abc123".to_string()),
            final_patch:    None,
        });

        assert_eq!(stored.event_name(), "run.failed");
        let properties = stored.properties().unwrap();
        assert_eq!(properties["error"], "Handler error: boom");
        assert_eq!(properties["duration_ms"], 900);
    }

    #[test]
    fn run_event_workflow_failure_serializes_causes() {
        let source = EventTestCause;
        let stored = to_run_event(&fixtures::RUN_6, &Event::WorkflowRunFailed {
            error:          Error::engine_with_source("Failed to initialize sandbox", &source),
            duration_ms:    900,
            reason:         FailureReason::WorkflowError,
            git_commit_sha: None,
            final_patch:    None,
        });

        let properties = stored.properties().unwrap();
        assert_eq!(
            properties["error"],
            "Engine error: Failed to initialize sandbox"
        );
        assert_eq!(
            properties["causes"],
            serde_json::json!(["connection refused"])
        );
    }

    #[test]
    fn stage_started_populates_parallel_ids_when_present() {
        let stored = to_run_event_at(
            &fixtures::RUN_1,
            &Event::StageStarted {
                node_id:      "review".to_string(),
                name:         "review".to_string(),
                index:        1,
                handler_type: "agent".to_string(),
                attempt:      1,
                max_attempts: 1,
            },
            Utc::now(),
            Some(&StageScope {
                node_id:            "review".to_string(),
                visit:              1,
                parallel_group_id:  Some(StageId::new("fanout", 2)),
                parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 2), 1)),
            }),
        );
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert_eq!(
            stored.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("fanout", 2), 1))
        );
    }

    #[test]
    fn parallel_started_populates_parallel_group_id() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::ParallelStarted {
            node_id:      "fanout".to_string(),
            visit:        2,
            branch_count: 3,
            join_policy:  "wait_all".to_string(),
        });
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert!(stored.parallel_branch_id.is_none());
    }

    #[test]
    fn parallel_branch_started_populates_group_and_branch_ids() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::ParallelBranchStarted {
            parallel_group_id:  StageId::new("fanout", 2),
            parallel_branch_id: ParallelBranchId::new(StageId::new("fanout", 2), 1),
            branch:             "review".to_string(),
            index:              1,
        });
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert_eq!(
            stored.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("fanout", 2), 1))
        );
    }

    #[test]
    fn agent_tool_started_populates_tool_call_id_and_stage_id() {
        let stored = to_run_event_at(
            &fixtures::RUN_1,
            &Event::Agent {
                stage:             "code".to_string(),
                visit:             3,
                event:             AgentEvent::ToolCallStarted {
                    tool_name:    "read_file".to_string(),
                    tool_call_id: "call_abc".to_string(),
                    arguments:    serde_json::json!({"path": "src/main.rs"}),
                },
                session_id:        Some("ses_1".to_string()),
                parent_session_id: None,
            },
            Utc::now(),
            Some(&StageScope {
                node_id:            "code".to_string(),
                visit:              3,
                parallel_group_id:  Some(StageId::new("fanout", 2)),
                parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 2), 0)),
            }),
        );
        assert_eq!(stored.stage_id, Some(StageId::new("code", 3)));
        assert_eq!(stored.tool_call_id.as_deref(), Some("call_abc"));
        assert_eq!(
            stored.actor,
            Some(Principal::Agent {
                session_id:        Some("ses_1".to_string()),
                parent_session_id: None,
                model:             None,
            })
        );
        assert_eq!(stored.parallel_group_id, Some(StageId::new("fanout", 2)));
        assert_eq!(
            stored.parallel_branch_id,
            Some(ParallelBranchId::new(StageId::new("fanout", 2), 0))
        );
    }

    #[test]
    fn stage_scope_populates_stage_id_on_non_stage_events() {
        // Events tied to a concrete stage execution but lacking scope in their
        // own variant fields (CheckpointCompleted, CommandStarted, PromptCompleted,
        // Prompt, InterviewStarted, Failover, GitCommit) should pick up stage_id
        // / parallel_group_id / parallel_branch_id from the scope argument.
        let scope = StageScope {
            node_id:            "build".to_string(),
            visit:              2,
            parallel_group_id:  Some(StageId::new("fanout", 1)),
            parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 1), 0)),
        };

        let command_started = to_run_event_at(
            &fixtures::RUN_1,
            &Event::CommandStarted {
                node_id:    "build".to_string(),
                script:     "echo".to_string(),
                command:    "echo".to_string(),
                language:   "shell".to_string(),
                timeout_ms: None,
            },
            Utc::now(),
            Some(&scope),
        );
        assert_eq!(command_started.stage_id, Some(StageId::new("build", 2)));
        assert_eq!(command_started.parallel_group_id, scope.parallel_group_id);
        assert_eq!(command_started.parallel_branch_id, scope.parallel_branch_id);

        let prompt = to_run_event_at(
            &fixtures::RUN_1,
            &Event::Prompt {
                stage:    "build".to_string(),
                visit:    2,
                text:     "do it".to_string(),
                mode:     None,
                provider: None,
                model:    None,
            },
            Utc::now(),
            Some(&scope),
        );
        assert_eq!(prompt.stage_id, Some(StageId::new("build", 2)));

        let git_commit = to_run_event_at(
            &fixtures::RUN_1,
            &Event::GitCommit {
                node_id: Some("build".to_string()),
                sha:     "deadbeef".to_string(),
            },
            Utc::now(),
            Some(&scope),
        );
        assert_eq!(git_commit.stage_id, Some(StageId::new("build", 2)));
    }

    #[test]
    fn run_level_events_without_scope_leave_stage_id_absent() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::RunRunning);
        assert!(stored.stage_id.is_none());
        assert!(stored.parallel_group_id.is_none());
        assert!(stored.parallel_branch_id.is_none());
    }

    #[test]
    fn control_action_events_carry_actor_in_envelope() {
        let actor = user_principal("alice");

        let cancel = to_run_event(&fixtures::RUN_1, &Event::RunCancelRequested {
            actor: Some(actor.clone()),
        });
        assert_eq!(cancel.event_name(), "run.cancel.requested");
        assert_eq!(cancel.actor.as_ref().expect("actor set"), &actor);

        let pause = to_run_event(&fixtures::RUN_1, &Event::RunPauseRequested {
            actor: Some(actor.clone()),
        });
        assert_eq!(pause.actor.as_ref().expect("actor set"), &actor);

        let unpause = to_run_event(&fixtures::RUN_1, &Event::RunUnpauseRequested {
            actor: None,
        });
        assert!(unpause.actor.is_none());
    }

    #[test]
    fn run_archived_round_trips_actor_in_envelope() {
        let actor = user_principal("alice");

        let archived = to_run_event(&fixtures::RUN_1, &Event::RunArchived {
            actor: Some(actor.clone()),
        });
        assert_eq!(archived.event_name(), "run.archived");
        assert_eq!(archived.actor.as_ref().expect("actor set"), &actor);
        assert!(matches!(archived.body, EventBody::RunArchived(_)));
    }

    #[test]
    fn run_unarchived_round_trips_actor_in_envelope() {
        let actor = user_principal("bob");

        let unarchived = to_run_event(&fixtures::RUN_1, &Event::RunUnarchived {
            actor: Some(actor.clone()),
        });
        assert_eq!(unarchived.event_name(), "run.unarchived");
        assert_eq!(unarchived.actor.as_ref().expect("actor set"), &actor);
        match &unarchived.body {
            EventBody::RunUnarchived(_) => {}
            other => panic!("expected RunUnarchived body, got {other:?}"),
        }
    }

    #[test]
    fn run_notice_maps_exec_output_tail_to_props() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::RunNotice {
            level:            RunNoticeLevel::Warn,
            code:             RunNoticeCode::GitDiffFailed.to_string(),
            message:          "git diff failed".to_string(),
            exec_output_tail: Some(exec_tail()),
        });

        match stored.body {
            EventBody::RunNotice(props) => {
                let tail = props.exec_output_tail.expect("exec output tail");
                assert_eq!(tail.stderr.as_deref(), Some("last stderr line"));
                assert!(tail.stderr_truncated);
            }
            other => panic!("expected RunNotice body, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_failed_maps_exec_output_tail_to_props() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::CheckpointFailed {
            node_id:          "build".to_string(),
            error:            "git commit failed".to_string(),
            exec_output_tail: Some(exec_tail()),
        });

        match stored.body {
            EventBody::CheckpointFailed(props) => {
                let tail = props.exec_output_tail.expect("exec output tail");
                assert_eq!(tail.stdout.as_deref(), Some("last stdout line"));
                assert!(!tail.stdout_truncated);
            }
            other => panic!("expected CheckpointFailed body, got {other:?}"),
        }
    }

    #[test]
    fn git_push_maps_exec_output_tail_to_props() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::GitPush {
            branch:           "refs/heads/run:refs/heads/run".to_string(),
            success:          false,
            exec_output_tail: Some(exec_tail()),
        });

        match stored.body {
            EventBody::GitPush(props) => {
                assert!(!props.success);
                let tail = props.exec_output_tail.expect("exec output tail");
                assert_eq!(tail.stderr.as_deref(), Some("last stderr line"));
            }
            other => panic!("expected GitPush body, got {other:?}"),
        }
    }

    #[test]
    fn retro_failed_maps_exec_output_tail_to_props() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::RetroFailed {
            error:            "state load failed".to_string(),
            duration_ms:      12,
            exec_output_tail: Some(exec_tail()),
        });

        match stored.body {
            EventBody::RetroFailed(props) => {
                assert_eq!(props.duration_ms, 12);
                let tail = props.exec_output_tail.expect("exec output tail");
                assert_eq!(tail.stdout.as_deref(), Some("last stdout line"));
            }
            other => panic!("expected RetroFailed body, got {other:?}"),
        }
    }

    #[test]
    fn metadata_snapshot_events_map_to_typed_bodies() {
        let started = to_run_event(&fixtures::RUN_1, &Event::MetadataSnapshotStarted {
            phase:  fabro_types::MetadataSnapshotPhase::Init,
            branch: "fabro/metadata/run".to_string(),
        });

        assert_eq!(started.event_name(), "metadata.snapshot.started");
        assert!(started.node_id.is_none());
        assert!(started.stage_id.is_none());
        match started.body {
            EventBody::MetadataSnapshotStarted(props) => {
                assert_eq!(props.phase, fabro_types::MetadataSnapshotPhase::Init);
                assert_eq!(props.branch, "fabro/metadata/run");
            }
            other => panic!("expected MetadataSnapshotStarted body, got {other:?}"),
        }

        let completed = to_run_event(&fixtures::RUN_1, &Event::MetadataSnapshotCompleted {
            phase:       fabro_types::MetadataSnapshotPhase::Finalize,
            branch:      "fabro/metadata/run".to_string(),
            duration_ms: 2400,
            entry_count: 4,
            bytes:       512,
            commit_sha:  "abc123".to_string(),
        });

        assert_eq!(completed.event_name(), "metadata.snapshot.completed");
        match completed.body {
            EventBody::MetadataSnapshotCompleted(props) => {
                assert_eq!(props.phase, fabro_types::MetadataSnapshotPhase::Finalize);
                assert_eq!(props.duration_ms, 2400);
                assert_eq!(props.entry_count, 4);
                assert_eq!(props.bytes, 512);
                assert_eq!(props.commit_sha, "abc123");
            }
            other => panic!("expected MetadataSnapshotCompleted body, got {other:?}"),
        }

        let failed = to_run_event(&fixtures::RUN_1, &Event::MetadataSnapshotFailed {
            phase:            fabro_types::MetadataSnapshotPhase::Checkpoint,
            branch:           "fabro/metadata/run".to_string(),
            duration_ms:      120,
            failure_kind:     fabro_types::MetadataSnapshotFailureKind::Push,
            error:            "push rejected".to_string(),
            causes:           vec!["permission denied".to_string()],
            commit_sha:       Some("def456".to_string()),
            entry_count:      Some(4),
            bytes:            Some(512),
            exec_output_tail: Some(fabro_types::ExecOutputTail {
                stdout:           Some("last stdout line".to_string()),
                stderr:           Some("last stderr line".to_string()),
                stdout_truncated: false,
                stderr_truncated: true,
            }),
        });

        assert_eq!(failed.event_name(), "metadata.snapshot.failed");
        match failed.body {
            EventBody::MetadataSnapshotFailed(props) => {
                assert_eq!(
                    props.failure_kind,
                    fabro_types::MetadataSnapshotFailureKind::Push
                );
                assert_eq!(props.commit_sha.as_deref(), Some("def456"));
                assert_eq!(props.entry_count, Some(4));
                assert_eq!(props.bytes, Some(512));
                let tail = props.exec_output_tail.expect("exec output tail");
                assert_eq!(tail.stdout.as_deref(), Some("last stdout line"));
                assert_eq!(tail.stderr.as_deref(), Some("last stderr line"));
                assert!(tail.stderr_truncated);
                assert!(!tail.stdout_truncated);
            }
            other => panic!("expected MetadataSnapshotFailed body, got {other:?}"),
        }
    }

    #[test]
    fn checkpoint_metadata_snapshot_events_can_be_stage_scoped() {
        let scope = StageScope {
            node_id:            "build".to_string(),
            visit:              2,
            parallel_group_id:  Some(StageId::new("fanout", 1)),
            parallel_branch_id: Some(ParallelBranchId::new(StageId::new("fanout", 1), 0)),
        };
        let stored = to_run_event_at(
            &fixtures::RUN_1,
            &Event::MetadataSnapshotStarted {
                phase:  fabro_types::MetadataSnapshotPhase::Checkpoint,
                branch: "fabro/metadata/run".to_string(),
            },
            Utc::now(),
            Some(&scope),
        );

        assert_eq!(stored.node_id.as_deref(), Some("build"));
        assert_eq!(stored.node_label.as_deref(), Some("build"));
        assert_eq!(stored.stage_id, Some(StageId::new("build", 2)));
        assert_eq!(stored.parallel_group_id, scope.parallel_group_id);
        assert_eq!(stored.parallel_branch_id, scope.parallel_branch_id);
    }

    #[test]
    fn agent_assistant_message_populates_agent_actor() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::Agent {
            stage:             "code".to_string(),
            visit:             1,
            event:             AgentEvent::AssistantMessage {
                text:            "ok".to_string(),
                model:           "claude-sonnet".to_string(),
                usage:           LlmTokenCounts::default(),
                tool_call_count: 0,
            },
            session_id:        Some("ses_agent".to_string()),
            parent_session_id: None,
        });
        let actor = stored.actor.as_ref().expect("actor set");
        assert_eq!(actor, &Principal::Agent {
            session_id:        Some("ses_agent".to_string()),
            parent_session_id: None,
            model:             Some("claude-sonnet".to_string()),
        });
    }

    #[test]
    fn agent_cli_cancelled_maps_to_event_body_with_node_id() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::AgentCliCancelled {
            node_id:     "code".to_string(),
            stdout:      "out".to_string(),
            stderr:      "err".to_string(),
            duration_ms: 42,
        });

        assert_eq!(stored.event_name(), "agent.cli.cancelled");
        assert_eq!(stored.node_id.as_deref(), Some("code"));
        match &stored.body {
            EventBody::AgentCliCancelled(props) => {
                assert_eq!(props.stdout, "out");
                assert_eq!(props.stderr, "err");
                assert_eq!(props.duration_ms, 42);
            }
            other => panic!("expected AgentCliCancelled, got {other:?}"),
        }
    }

    #[test]
    fn agent_cli_timed_out_maps_to_event_body_with_node_id() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::AgentCliTimedOut {
            node_id:     "code".to_string(),
            stdout:      "out".to_string(),
            stderr:      "err".to_string(),
            duration_ms: 99,
        });

        assert_eq!(stored.event_name(), "agent.cli.timed_out");
        assert_eq!(stored.node_id.as_deref(), Some("code"));
        match &stored.body {
            EventBody::AgentCliTimedOut(props) => {
                assert_eq!(props.stdout, "out");
                assert_eq!(props.stderr, "err");
                assert_eq!(props.duration_ms, 99);
            }
            other => panic!("expected AgentCliTimedOut, got {other:?}"),
        }
    }

    #[test]
    fn stall_watchdog_timeout_populates_watchdog_actor() {
        let stored = to_run_event(&fixtures::RUN_1, &Event::StallWatchdogTimeout {
            node:         "code".to_string(),
            idle_seconds: 60,
        });

        assert_eq!(stored.event_name(), "watchdog.timeout");
        assert_eq!(stored.node_id.as_deref(), Some("code"));
        assert_eq!(
            stored.actor,
            Some(Principal::System {
                system_kind: SystemActorKind::Watchdog,
            })
        );
    }

    #[test]
    fn run_created_populates_user_actor_from_provenance() {
        use ::fabro_types::{Graph, WorkflowSettings, fixtures};

        let provenance = RunProvenance {
            server:  None,
            client:  None,
            subject: Some(user_principal("alice")),
        };

        let stored = to_run_event(&fixtures::RUN_1, &Event::RunCreated {
            run_id:           fixtures::RUN_1,
            settings:         serde_json::to_value(WorkflowSettings::default()).unwrap(),
            graph:            serde_json::to_value(Graph::new("test")).unwrap(),
            workflow_source:  None,
            workflow_config:  None,
            labels:           BTreeMap::default(),
            run_dir:          "/tmp/run".to_string(),
            source_directory: Some("/tmp/run".to_string()),
            workflow_slug:    None,
            db_prefix:        None,
            provenance:       Some(provenance),
            manifest_blob:    None,
            git:              None,
            fork_source_ref:  None,
            in_place:         false,
            web_url:          None,
        });
        let actor = stored.actor.as_ref().expect("actor set");
        assert_eq!(actor, &user_principal("alice"));
    }
}
