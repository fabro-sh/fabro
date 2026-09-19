//! Petri's `Interviewer` over Fabro's questions API and the worker's
//! control channel.
//!
//! A human gate in a Petri run asks through Petri's interview boundary: the
//! dispatcher hands this adapter one [`InterviewRequest`] per question, on
//! its own task, with the question's identity (invocation path, execution,
//! firing, attempt, node, occurrence, ask). The adapter surfaces the
//! question to Fabro as a `human` stage's question, waits for the answer
//! through the questions API, and hands Petri the reply.
//!
//! # One identity
//!
//! A question has one id in Fabro: Petri's [`Question::id`] (`gate#2`),
//! as the `question` record names it. The projection over Petri's records
//! keys `pending_interviews` by it, `GET /runs/{id}/questions` lists it,
//! `POST /runs/{id}/questions/{qid}/answer` validates the answer against
//! the projection's pending question under it, and this adapter waits on
//! the worker's [`ControlInterviewer`] under it. The rest of Petri's
//! identity rides on [`AskedQuestion::identity`] for the record of who
//! answered. The stage a question names is the label the projection gives
//! the asking firing (`gate@1`, or `gate/e3@1` when another execution took
//! that label): [`Observed`] labels every firing through the projection's
//! own rule ([`projection::stage_label`]) as the run's records go by.
//!
//! # How a question reaches a person
//!
//! The projection derives the pending question, its answer and its expiry
//! from Petri's records alone; the run's own record is the source of truth
//! and nothing the adapter posts is folded into it. The readers that follow
//! the run's stream rather than its projection (the server's Slack service,
//! `fabro run attach`, the web app's Q&A renderer) see the question in
//! Petri's own event: the progress record whose `derived.parsed.kind` is
//! `question`, and the answer record that closes it.
//!
//! The adapter reports what happens to each question as a [`QuestionNotice`]
//! to a [`QuestionSink`], when something observes it: a test's board. The
//! server records who answered as the `interview.answered` platform record
//! when it accepts the answer, since that is a Fabro fact Petri's answer
//! record does not carry.
//!
//! # How the answer comes back
//!
//! `POST /runs/{id}/questions/{qid}/answer` validates the answer against
//! the pending record and delivers it to the run: over the worker control
//! bus as an `interview.answer` message, which the worker's control
//! manager applies to its [`ControlInterviewer`] by question id, or
//! straight to that interviewer for a run in the server process. The
//! adapter waits on that interviewer under the same id, so an answer
//! submitted before the wait began is buffered and one submitted after it
//! is delivered. The legacy answer shape is mapped onto Petri's
//! [`Answer`]: `yes` and `no` name the gate's affirmative and negative
//! choices by key, a selection names its key, a multi-selection its keys,
//! free text is text. A cancelled or interrupted answer ends the interview
//! without one: Petri's gate fails closed on it.
//!
//! # Expiry and cancellation
//!
//! The gate owns its answer deadline (Fabro's default when the node names
//! none) and reports the expiry itself; the dispatcher then fires the
//! adapter's cancel token, as it does when the firing ends without an
//! answer or the run is cancelled. The adapter returns promptly with
//! [`InterviewReply::Cancelled`] and reports the question as expired when
//! the gate reported the expiry, else as interrupted, so an observer sees
//! the question end. The expiry report is seen by the adapter's
//! own observer ([`FabroInterviewer::observer`]), which the run registers
//! ahead of the dispatcher so the report is noted before the token fires.
//! The dispatcher races the reply against the same token and may drop the
//! reply future the moment the token fires, so the notice is posted from a
//! guard that runs whether the future completes or is dropped, on a task
//! of its own. The dispatcher's own record of the outcome (`TimedOut` with
//! the default taken, `Cancelled`, `Late`) is the authoritative one and
//! reaches the receipt, and the projection closes the question on Petri's
//! `question_expired` record or the cancelled attempt.
//!
//! # Auto-approval
//!
//! A run whose `[run.execution] approval` is `auto` answers every question
//! at once as `--auto-approve` always has (`yes`, the first option, or
//! `auto-approved` text), attributed to the engine. The question is still
//! asked and answered through Petri, so the run's stream shows what was
//! decided, and the projection closes it on the delivered answer.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use fabro_interview::{
    Answer as LegacyAnswer, AnswerSubmission, AnswerValue, AutoApproveInterviewer,
    ControlInterviewer, Interviewer as LegacyInterviewer, Question as LegacyQuestion,
};
use fabro_types::{
    InterviewOption, Principal, QuestionType, ReviewTarget, ReviewTargetKind, StageId,
    SystemActorKind,
};
use petri_execution::events::{Parsed, Projection, ViewEvent};
use petri_execution::{
    CoordinatorRecord, ExecutionId, ExecutionObserver, InterviewError, InterviewReply,
    InterviewRequest, Interviewer,
};
use petri_runtime::engine::{EngineState, EventRecord};
use petri_runtime::ir::FiringId;
use petri_runtime::steps::{Answer, Question, QuestionOption};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::projection;

/// Whether a run answers its own questions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approval {
    /// A person answers, through the API.
    Prompt,
    /// The engine answers at once, as `--auto-approve` does.
    Auto,
}

/// Petri's full identity for one question, beyond its id: where in the run
/// it was asked, for the record of who answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionIdentity {
    pub invocation_path: String,
    pub execution:       u64,
    pub firing:          u64,
    pub attempt:         u32,
    pub node:            String,
    pub occurrence:      u32,
    pub ask:             u32,
}

impl QuestionIdentity {
    fn of(request: &InterviewRequest) -> Self {
        Self {
            invocation_path: request.invocation_path.clone(),
            execution:       request.execution.raw(),
            firing:          request.firing.raw(),
            attempt:         request.attempt.raw(),
            node:            request.node.to_string(),
            occurrence:      request.occurrence,
            ask:             request.ask,
        }
    }
}

/// A question as Fabro shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct AskedQuestion {
    /// Petri's id for the question, the one id Fabro knows it by.
    pub question_id:     String,
    pub identity:        QuestionIdentity,
    pub text:            String,
    /// The label the projection gives the asking firing.
    pub stage:           String,
    pub question_type:   QuestionType,
    pub options:         Vec<InterviewOption>,
    pub allow_freeform:  bool,
    pub timeout_seconds: Option<f64>,
    pub review_target:   Option<ReviewTarget>,
}

/// What the adapter tells Fabro about a question, in the order it happens.
#[derive(Clone, Debug, PartialEq)]
pub enum QuestionNotice {
    Asked(AskedQuestion),
    Answered {
        question_id: String,
        text:        String,
        /// The answer as Fabro records it: the word, the key, the keys, or
        /// the text; a sensitive answer is masked.
        answer:      String,
        actor:       Principal,
        duration_ms: u64,
    },
    Expired {
        question_id: String,
        text:        String,
        stage:       String,
        duration_ms: u64,
    },
    Interrupted {
        question_id: String,
        text:        String,
        stage:       String,
        reason:      String,
        duration_ms: u64,
    },
}

impl QuestionNotice {
    fn question_id(&self) -> &str {
        match self {
            Self::Asked(asked) => &asked.question_id,
            Self::Answered { question_id, .. }
            | Self::Expired { question_id, .. }
            | Self::Interrupted { question_id, .. } => question_id,
        }
    }
}

/// Where the adapter reports what happens to a question, when something
/// observes it: a test's board. A run's own record of a question is
/// Petri's, and who answered it is the server's platform record.
#[async_trait::async_trait]
pub trait QuestionSink: Send + Sync {
    async fn post(&self, notice: QuestionNotice) -> anyhow::Result<()>;
}

/// A sink that drops every notice: the adapter with nothing observing it.
struct Unobserved;

#[async_trait::async_trait]
impl QuestionSink for Unobserved {
    async fn post(&self, _notice: QuestionNotice) -> anyhow::Result<()> {
        Ok(())
    }
}

/// What the adapter learns from the run's records ahead of the dispatcher:
/// the label the projection gives each firing, and the questions whose
/// expiry the gate reported. An observer the run registers ahead of the
/// dispatcher, fed the same records the projection folds, derived through
/// Petri's own [`Projection`] so a firing's visit and label come out as the
/// read side computes them.
#[derive(Default)]
pub struct Observed {
    state: Mutex<ObservedState>,
}

#[derive(Default)]
struct ObservedState {
    projection: Projection,
    /// Every label given so far, for the projection's collision rule.
    labels:     BTreeSet<String>,
    /// The label of each shown firing.
    stages:     HashMap<(ExecutionId, FiringId), StageId>,
    expired:    HashSet<(ExecutionId, String)>,
}

impl Observed {
    /// The label the projection gives `firing`, once its `visit.started`
    /// was seen.
    fn label(&self, execution: ExecutionId, firing: FiringId) -> Option<StageId> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stages
            .get(&(execution, firing))
            .cloned()
    }

    fn expired(&self, execution: ExecutionId, question: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .expired
            .contains(&(execution, question.to_string()))
    }
}

impl ExecutionObserver for Observed {
    fn on_engine_record(
        &self,
        execution: ExecutionId,
        record: &EventRecord,
        recorded_at: u64,
        state: &EngineState,
    ) {
        let mut observed = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let events = observed
            .projection
            .engine(execution, record, recorded_at, state);
        for event in &events {
            if let Some(ViewEvent::VisitStarted { .. }) = event.view() {
                let Some(subject) = event.subject.as_ref() else {
                    continue;
                };
                let Some(firing) = subject.firing else {
                    continue;
                };
                if !projection::is_shown(&subject.node) {
                    continue;
                }
                let label = projection::stage_label(
                    &subject.node.name,
                    projection::visit_of(subject),
                    execution,
                    &observed.labels,
                );
                observed.labels.insert(label.to_string());
                observed.stages.insert((execution, firing), label);
            }
            if let Some(Parsed::QuestionExpired { expired }) = event.parsed() {
                observed
                    .expired
                    .insert((execution, expired.question.clone()));
            }
        }
    }

    fn on_lifecycle(&self, record: &CoordinatorRecord) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .projection
            .lifecycle(record);
    }
}

/// The interviewer a Fabro run installs.
pub struct FabroInterviewer {
    answers:  Arc<ControlInterviewer>,
    sink:     Arc<dyn QuestionSink>,
    approval: Approval,
    observed: Arc<Observed>,
}

impl FabroInterviewer {
    /// Over the control interviewer the run's answers are delivered to.
    #[must_use]
    pub fn new(answers: Arc<ControlInterviewer>, approval: Approval) -> Self {
        Self {
            answers,
            sink: Arc::new(Unobserved),
            approval,
            observed: Arc::new(Observed::default()),
        }
    }

    /// Report what happens to each question to `sink` as well.
    #[must_use]
    pub fn with_sink(mut self, sink: Arc<dyn QuestionSink>) -> Self {
        self.sink = sink;
        self
    }

    /// The observer that labels each firing as the projection does and
    /// sees a gate report a question's expiry. A run registers it ahead of
    /// the interview dispatcher, so a question's stage is known when it is
    /// asked and the adapter tells an expiry from an interruption when the
    /// dispatcher ends its wait.
    #[must_use]
    pub fn observer(&self) -> Arc<dyn ExecutionObserver> {
        self.observed.clone()
    }

    /// Post a notice; a failure after the question was asked is logged,
    /// since the answer, not the notice, is what the run depends on.
    async fn post(&self, notice: QuestionNotice) {
        let question_id = notice.question_id().to_string();
        if let Err(error) = self.sink.post(notice).await {
            warn!(
                question_id = %question_id,
                error = format!("{error:#}"),
                "a question notice could not be posted"
            );
        }
    }
}

#[async_trait::async_trait]
impl Interviewer for FabroInterviewer {
    async fn reply(&self, request: InterviewRequest, cancel: CancellationToken) -> InterviewReply {
        let stage = self
            .observed
            .label(request.execution, request.firing)
            .map_or_else(
                || {
                    debug!(
                        node = %request.node,
                        execution = request.execution.raw(),
                        firing = request.firing.raw(),
                        "the asking firing has no label yet; the question names the node"
                    );
                    request.node.to_string()
                },
                |label| label.to_string(),
            );
        let asked = asked_question(&request, stage);
        let question_id = asked.question_id.clone();
        let text = asked.text.clone();
        let stage = asked.stage.clone();
        let legacy = legacy_question(&asked);
        if let Err(error) = self.sink.post(QuestionNotice::Asked(asked)).await {
            return InterviewReply::Failed(InterviewError::with_source(
                format!("question `{question_id}` could not be published to Fabro"),
                AnyhowError(error),
            ));
        }
        let mut outstanding = Outstanding {
            sink:        Arc::clone(&self.sink),
            observed:    Arc::clone(&self.observed),
            execution:   request.execution,
            question_id: question_id.clone(),
            text:        text.clone(),
            stage:       stage.clone(),
            started:     Instant::now(),
            open:        true,
        };
        let submission = match self.approval {
            Approval::Auto => Some(AutoApproveInterviewer::engine().ask(legacy).await),
            Approval::Prompt => tokio::select! {
                submission = self.answers.ask(legacy) => Some(submission),
                () = cancel.cancelled() => None,
            },
        };
        let duration_ms = millis(outstanding.started.elapsed());
        let Some(submission) = submission else {
            // The dispatcher ended the wait: the gate expired the question,
            // the firing finished, the run was cancelled, or the run ended.
            outstanding.close_unanswered("cancelled");
            return InterviewReply::Cancelled;
        };
        // `submission.actor` is who answered, a Fabro fact Petri's answer
        // record does not carry: the server records it as the
        // `interview.answered` platform record when it accepts the answer;
        // here it only reaches an observer.
        let Some(answer) = petri_answer(&submission.answer, &request.question) else {
            outstanding.close_unanswered(&reason_of(&submission.answer.value));
            return InterviewReply::Cancelled;
        };
        debug!(question_id = %question_id, actor = ?submission.actor, "question answered");
        outstanding.open = false;
        self.post(QuestionNotice::Answered {
            question_id,
            text,
            answer: describe(&answer, &request.question),
            actor: submission.actor,
            duration_ms,
        })
        .await;
        InterviewReply::Answered(answer)
    }
}

/// A question the adapter is waiting on. When the wait ends without an
/// answer, whether the adapter saw the cancel or the dispatcher dropped
/// the reply future first, the end of the question is posted from here
/// on its own task: as expired when the gate reported the expiry, else as
/// interrupted.
struct Outstanding {
    sink:        Arc<dyn QuestionSink>,
    observed:    Arc<Observed>,
    execution:   ExecutionId,
    /// Petri's question id, as the expiry report names it too.
    question_id: String,
    text:        String,
    stage:       String,
    started:     Instant,
    open:        bool,
}

impl Outstanding {
    /// End the question without an answer, for `reason` unless the gate
    /// reported the expiry.
    fn close_unanswered(&mut self, reason: &str) {
        if !self.open {
            return;
        }
        self.open = false;
        let duration_ms = millis(self.started.elapsed());
        let expired = self.observed.expired(self.execution, &self.question_id);
        let notice = if expired {
            QuestionNotice::Expired {
                question_id: self.question_id.clone(),
                text: self.text.clone(),
                stage: self.stage.clone(),
                duration_ms,
            }
        } else {
            QuestionNotice::Interrupted {
                question_id: self.question_id.clone(),
                text: self.text.clone(),
                stage: self.stage.clone(),
                reason: reason.to_string(),
                duration_ms,
            }
        };
        let sink = Arc::clone(&self.sink);
        let question_id = self.question_id.clone();
        let post = async move {
            if let Err(error) = sink.post(notice).await {
                warn!(
                    question_id = %question_id,
                    error = format!("{error:#}"),
                    "the end of a question could not be posted"
                );
            }
        };
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(post);
        } else {
            warn!(
                question_id = %self.question_id,
                "no runtime to post the end of a question from"
            );
        }
    }
}

impl Drop for Outstanding {
    fn drop(&mut self) {
        self.close_unanswered("cancelled");
    }
}

/// An `anyhow` error as a source for Petri's interview error.
#[derive(Debug)]
struct AnyhowError(anyhow::Error);

impl std::fmt::Display for AnyhowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.0)
    }
}

impl std::error::Error for AnyhowError {}

/// The question as Fabro shows it, under Petri's id and the stage label
/// the projection gives the asking firing.
fn asked_question(request: &InterviewRequest, stage: String) -> AskedQuestion {
    let question = &request.question;
    AskedQuestion {
        question_id: question.id.clone(),
        identity: QuestionIdentity::of(request),
        text: question.text.clone(),
        stage,
        question_type: question_type(question),
        options: question
            .options
            .iter()
            .map(|option| InterviewOption {
                key:         option.key.clone(),
                label:       option.label.clone(),
                description: None,
                preview:     None,
            })
            .collect(),
        allow_freeform: question.freeform,
        timeout_seconds: question
            .timeout_ms
            .map(|ms| Duration::from_millis(ms).as_secs_f64()),
        review_target: question.reference.as_ref().and_then(|reference| {
            let kind = match reference.kind.as_deref() {
                None | Some("document") => ReviewTargetKind::Document,
                Some(other) => {
                    warn!(
                        kind = other,
                        "review target kind is not one Fabro shows; showing a document"
                    );
                    ReviewTargetKind::Document
                }
            };
            ReviewTarget::new(&reference.label, &reference.url, kind)
                .inspect_err(|error| {
                    warn!(error = %error, "review target could not be shown");
                })
                .ok()
        }),
    }
}

/// Fabro's question type: the one the gate names, else what the shape
/// implies.
pub(crate) fn question_type(question: &Question) -> QuestionType {
    question
        .kind
        .as_deref()
        .and_then(|kind| kind.parse().ok())
        .unwrap_or(if question.options.is_empty() {
            QuestionType::Freeform
        } else {
            QuestionType::MultipleChoice
        })
}

/// The legacy question the control interviewer waits under: only the id
/// matters to it; the rest is what the auto-approve interviewer decides on.
fn legacy_question(asked: &AskedQuestion) -> LegacyQuestion {
    let mut question = LegacyQuestion::new(asked.text.clone(), asked.question_type);
    question.id.clone_from(&asked.question_id);
    question.options.clone_from(&asked.options);
    question.allow_freeform = asked.allow_freeform;
    question.timeout_seconds = asked.timeout_seconds;
    question.stage.clone_from(&asked.stage);
    question.review_target.clone_from(&asked.review_target);
    question
}

/// Petri's answer for a legacy one, or `None` when the person or the
/// engine ended the interview without one.
fn petri_answer(answer: &LegacyAnswer, question: &Question) -> Option<Answer> {
    match &answer.value {
        AnswerValue::Yes => Some(Answer::choice(&affirmative_key(question))),
        AnswerValue::No => Some(Answer::choice(&negative_key(question))),
        AnswerValue::Selected(key) => Some(Answer::choice(key)),
        AnswerValue::MultiSelected(keys) => Some(Answer::choices(keys.iter().cloned())),
        AnswerValue::Text(text) => Some(Answer::text(text.clone())),
        AnswerValue::Cancelled
        | AnswerValue::Interrupted
        | AnswerValue::Skipped
        | AnswerValue::Timeout => None,
    }
}

/// Whether a choice is the affirmative one of a yes/no gate, as the gate
/// itself matches a `yes` answer: key `y` or `yes`, or label `yes`.
fn is_affirmative(option: &QuestionOption) -> bool {
    option.key.eq_ignore_ascii_case("y")
        || option.key.eq_ignore_ascii_case("yes")
        || strip_accelerator(&option.label).eq_ignore_ascii_case("yes")
}

fn is_negative(option: &QuestionOption) -> bool {
    option.key.eq_ignore_ascii_case("n")
        || option.key.eq_ignore_ascii_case("no")
        || strip_accelerator(&option.label).eq_ignore_ascii_case("no")
}

/// The key a `yes` answer names: the affirmative choice, else the word
/// itself for the gate to match.
fn affirmative_key(question: &Question) -> String {
    question
        .options
        .iter()
        .find(|option| is_affirmative(option))
        .map_or_else(|| "yes".to_string(), |option| option.key.clone())
}

/// The key a `no` answer names: the negative choice, else the first choice
/// that is not affirmative, else the word itself.
fn negative_key(question: &Question) -> String {
    question
        .options
        .iter()
        .find(|option| is_negative(option))
        .or_else(|| {
            question
                .options
                .iter()
                .find(|option| !is_affirmative(option))
        })
        .map_or_else(|| "no".to_string(), |option| option.key.clone())
}

/// A label without its `[K] ` accelerator prefix.
fn strip_accelerator(label: &str) -> &str {
    let trimmed = label.trim();
    match trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']'))
    {
        Some((_, rest)) => rest.trim(),
        None => trimmed,
    }
}

/// The answer as the notice records it. A sensitive text answer is never
/// written out: the dispatcher registers it as a secret.
fn describe(answer: &Answer, question: &Question) -> String {
    if !answer.choices.is_empty() {
        return answer.choices.join(", ");
    }
    if let Some(choice) = &answer.choice {
        return choice.clone();
    }
    match &answer.text {
        Some(_) if question.sensitive => "***".to_string(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn reason_of(value: &AnswerValue) -> String {
    match value {
        AnswerValue::Cancelled => "cancelled",
        AnswerValue::Interrupted => "interrupted",
        AnswerValue::Skipped => "skipped",
        AnswerValue::Timeout => "timeout",
        _ => "unanswered",
    }
    .to_string()
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// An engine actor, for callers that answer on the run's behalf.
#[must_use]
pub fn engine_actor() -> Principal {
    Principal::System {
        system_kind: SystemActorKind::Engine,
    }
}

/// A submission on the run's behalf.
#[must_use]
pub fn engine_submission(answer: LegacyAnswer) -> AnswerSubmission {
    AnswerSubmission::new(answer, engine_actor())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes_no() -> Question {
        let mut question = Question::new("gate#3", "Go?");
        question.options = vec![
            QuestionOption {
                key:         "Y".into(),
                label:       "[Y] Yes".into(),
                description: None,
                preview:     None,
            },
            QuestionOption {
                key:         "N".into(),
                label:       "[N] No".into(),
                description: None,
                preview:     None,
            },
        ];
        question.kind = Some("yes_no".into());
        question
    }

    #[test]
    fn yes_and_no_name_the_gates_choices_by_key() {
        let question = yes_no();
        assert_eq!(
            petri_answer(&LegacyAnswer::yes(), &question),
            Some(Answer::choice("Y"))
        );
        assert_eq!(
            petri_answer(&LegacyAnswer::no(), &question),
            Some(Answer::choice("N"))
        );
        let mut approve = Question::new("q", "Ship?");
        approve.options = vec![
            QuestionOption {
                key:         "A".into(),
                label:       "Approve".into(),
                description: None,
                preview:     None,
            },
            QuestionOption {
                key:         "R".into(),
                label:       "Reject".into(),
                description: None,
                preview:     None,
            },
        ];
        assert_eq!(
            petri_answer(&LegacyAnswer::yes(), &approve),
            Some(Answer::choice("yes")),
            "no affirmative choice: the word reaches the gate to match"
        );
        assert_eq!(
            petri_answer(&LegacyAnswer::no(), &approve),
            Some(Answer::choice("A")),
            "the first choice that is not affirmative"
        );
    }

    #[test]
    fn selections_text_and_refusals_map_to_petris_shapes() {
        let question = yes_no();
        assert_eq!(
            petri_answer(
                &LegacyAnswer {
                    value:           AnswerValue::Selected("N".into()),
                    selected_option: None,
                    text:            None,
                },
                &question
            ),
            Some(Answer::choice("N"))
        );
        assert_eq!(
            petri_answer(
                &LegacyAnswer::multi_selected(vec!["A".into(), "B".into()]),
                &question
            ),
            Some(Answer::choices(["A", "B"]))
        );
        assert_eq!(
            petri_answer(&LegacyAnswer::text("ship it"), &question),
            Some(Answer::text("ship it"))
        );
        for ended in [
            LegacyAnswer::cancelled(),
            LegacyAnswer::interrupted(),
            LegacyAnswer::skipped(),
            LegacyAnswer::timeout(),
        ] {
            assert_eq!(petri_answer(&ended, &question), None);
        }
    }

    #[test]
    fn the_question_type_is_the_gates_else_the_shapes() {
        assert_eq!(question_type(&yes_no()), QuestionType::YesNo);
        let mut choice = yes_no();
        choice.kind = None;
        assert_eq!(question_type(&choice), QuestionType::MultipleChoice);
        let mut free = Question::new("q", "Name?");
        free.freeform = true;
        assert_eq!(question_type(&free), QuestionType::Freeform);
    }

    #[test]
    fn a_sensitive_text_answer_is_described_masked() {
        let mut question = Question::new("q", "Token?");
        question.sensitive = true;
        assert_eq!(describe(&Answer::text("hunter2"), &question), "***");
        question.sensitive = false;
        assert_eq!(describe(&Answer::text("hunter2"), &question), "hunter2");
        assert_eq!(describe(&Answer::choices(["A", "B"]), &question), "A, B");
    }
}
