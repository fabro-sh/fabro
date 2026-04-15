use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use fabro_util::backoff::BackoffPolicy;
use futures::{Stream, StreamExt, future, stream};
use tokio::sync::{OnceCell, mpsc};
use tokio::time;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::client::Client;
use crate::error::Error;
use crate::provider::StreamEventStream;
use crate::retry::retry;
use crate::tools::{RepairToolCallFn, Tool, execute_all_tools_with_repair};
use crate::types::{
    FinishReason, GenerateResult, Message, ObjectStreamEvent, ReasoningEffort, Request, Response,
    ResponseFormat, ResponseFormatType, RetryPolicy, StepResult, StreamEvent, TimeoutOptions,
    TokenCounts, ToolCall, ToolChoice, ToolDefinition,
};

/// Module-level default client (Section 2.5).
static DEFAULT_CLIENT: OnceCell<Arc<Client>> = OnceCell::const_new();

/// Set the module-level default client.
pub fn set_default_client(client: Client) {
    let _ = DEFAULT_CLIENT.set(Arc::new(client));
}

/// Get the default client, lazily initialized from env.
async fn get_default_client() -> Result<Arc<Client>, Error> {
    if let Some(client) = DEFAULT_CLIENT.get() {
        return Ok(client.clone());
    }
    let client = Arc::new(Client::from_env().await?);
    let _ = DEFAULT_CLIENT.set(client.clone());
    Ok(client)
}

fn build_initial_messages(params: &GenerateParams) -> Result<Vec<Message>, Error> {
    let mut messages = Vec::new();
    if let Some(system) = &params.system {
        messages.push(Message::system(system));
    }
    if let Some(ref prompt) = params.prompt {
        if params.messages.is_some() {
            return Err(Error::Configuration {
                message: "Cannot specify both 'prompt' and 'messages'".into(),
                source: None,
            });
        }
        messages.push(Message::user(prompt));
    } else if let Some(ref msgs) = params.messages {
        messages.extend(msgs.clone());
    }
    Ok(messages)
}

fn build_request(
    params: &GenerateParams,
    messages: &[Message],
    tool_definitions: Option<&[ToolDefinition]>,
) -> Request {
    Request {
        model: params.model.clone(),
        messages: messages.to_vec(),
        provider: params.provider.clone(),
        tools: tool_definitions.map(<[ToolDefinition]>::to_vec),
        tool_choice: params.tool_choice.clone(),
        response_format: params.response_format.clone(),
        temperature: params.temperature,
        top_p: params.top_p,
        max_tokens: params.max_tokens,
        stop_sequences: params.stop_sequences.clone(),
        reasoning_effort: params.reasoning_effort,
        speed: params.speed.clone(),
        metadata: params.metadata.clone(),
        provider_options: params.provider_options.clone(),
    }
}

fn build_generate_result(steps: Vec<StepResult>, total_usage: TokenCounts) -> GenerateResult {
    let last = steps.last().expect("steps should not be empty");
    let response = last.response.clone();
    let tool_results = last.tool_results.clone();
    GenerateResult {
        response,
        tool_results,
        total_usage,
        steps,
        output: None,
    }
}

/// High-level blocking generation function (Section 4.3).
///
/// Wraps `Client.complete()` with tool execution loops, prompt standardization,
/// and automatic retries.
///
/// # Errors
///
/// Returns `Error::Configuration` if both `prompt` and `messages` are set,
/// or any provider error encountered during generation or tool execution.
///
/// # Panics
///
/// Panics if a tool's `execute` handler is `None` when matched during tool
/// execution.
pub async fn generate(params: GenerateParams) -> Result<GenerateResult, Error> {
    let client = match params.client.clone() {
        Some(c) => c,
        None => get_default_client().await?,
    };
    let retry_policy = RetryPolicy {
        max_retries: params.max_retries,
        backoff: BackoffPolicy {
            initial_delay: std::time::Duration::from_micros(1),
            jitter: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut messages = build_initial_messages(&params)?;
    let tool_definitions: Option<Vec<ToolDefinition>> = params
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.definition.clone()).collect());

    let max_tool_rounds = params.max_tool_rounds;

    let abort_signal = params.abort_signal.clone();

    let generate_future = async {
        let mut steps: Vec<StepResult> = Vec::new();
        let mut total_usage = TokenCounts::default();

        let mut round = 0u32;
        loop {
            if let Some(ref token) = abort_signal {
                if token.is_cancelled() {
                    warn!("Generation interrupted by cancellation token");
                    return Err(Error::Interrupt {
                        message: "Generation interrupted by cancellation token".into(),
                    });
                }
            }

            let request = build_request(&params, &messages, tool_definitions.as_deref());

            debug!(
                model = %params.model,
                provider = ?params.provider,
                messages = messages.len(),
                tools = tool_definitions.as_ref().map_or(0, std::vec::Vec::len),
                "Sending LLM request"
            );

            let client_ref = client.clone();
            let response = if let Some(per_step) = params.timeout.as_ref().and_then(|t| t.per_step)
            {
                let duration = std::time::Duration::from_secs_f64(per_step);
                time::timeout(
                    duration,
                    retry(&retry_policy, || {
                        let c = client_ref.clone();
                        let r = request.clone();
                        async move { c.complete(&r).await }
                    }),
                )
                .await
                .map_err(|_| {
                    warn!(timeout_secs = per_step, "Per-step timeout exceeded");
                    Error::RequestTimeout {
                        message: format!("Per-step timeout of {per_step}s exceeded"),
                        source: None,
                    }
                })?
            } else {
                retry(&retry_policy, || {
                    let c = client_ref.clone();
                    let r = request.clone();
                    async move { c.complete(&r).await }
                })
                .await
            }?;

            debug!(
                model = %response.model,
                provider = %response.provider,
                input_tokens = response.usage.input_tokens,
                output_tokens = response.usage.output_tokens,
                finish_reason = ?response.finish_reason,
                "LLM response received"
            );

            let tool_calls = response.tool_calls();
            let mut tool_results = Vec::new();

            if let Some(tools) = &params.tools {
                if !tool_calls.is_empty()
                    && response.finish_reason == FinishReason::ToolCalls
                    && max_tool_rounds > 0
                {
                    debug!(
                        tool_calls = tool_calls.len(),
                        round = round,
                        "Executing tool calls"
                    );
                    if tools.iter().any(|t| t.is_active()) {
                        let tool_refs: Vec<&Tool> =
                            tools.iter().map(std::convert::AsRef::as_ref).collect();
                        tool_results = execute_all_tools_with_repair(
                            &tool_refs,
                            &tool_calls,
                            &messages,
                            abort_signal.as_ref(),
                            params.repair_tool_call.as_ref(),
                        )
                        .await;
                    }
                }
            }

            total_usage += response.usage.clone();

            steps.push(StepResult {
                response,
                tool_results,
            });

            let last = steps.last().expect("just pushed");
            let should_continue = !tool_calls.is_empty()
                && last.response.finish_reason == FinishReason::ToolCalls
                && round < max_tool_rounds
                && !last.tool_results.is_empty()
                && !params.stop_when.as_ref().is_some_and(|f| f(&steps));

            if !should_continue {
                break;
            }

            if let Some(ref token) = abort_signal {
                if token.is_cancelled() {
                    return Err(Error::Interrupt {
                        message: "Generation interrupted by cancellation token".into(),
                    });
                }
            }

            let last = steps.last().expect("just pushed");
            messages.push(last.response.message.clone());
            for result in &last.tool_results {
                messages.push(Message::tool_result(
                    &result.tool_call_id,
                    result.content.clone(),
                    result.is_error,
                ));
            }

            round += 1;
        }

        Ok(build_generate_result(steps, total_usage))
    };

    if let Some(total) = params.timeout.as_ref().and_then(|t| t.total) {
        let duration = std::time::Duration::from_secs_f64(total);
        time::timeout(duration, generate_future)
            .await
            .map_err(|_| {
                warn!(timeout_secs = total, "Total generation timeout exceeded");
                Error::RequestTimeout {
                    message: format!("Total timeout of {total}s exceeded"),
                    source: None,
                }
            })?
    } else {
        generate_future.await
    }
}

/// Callback type for custom stop conditions in the tool loop.
pub type StopCondition = Arc<dyn Fn(&[StepResult]) -> bool + Send + Sync>;

/// Parameters for `generate()` (Section 4.3).
#[derive(Clone)]
pub struct GenerateParams {
    pub model: String,
    pub prompt: Option<String>,
    pub messages: Option<Vec<Message>>,
    pub system: Option<String>,
    pub tools: Option<Vec<Arc<Tool>>>,
    pub tool_choice: Option<ToolChoice>,
    pub max_tool_rounds: u32,
    pub response_format: Option<ResponseFormat>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub max_tokens: Option<i64>,
    pub stop_sequences: Option<Vec<String>>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub speed: Option<String>,
    pub provider: Option<String>,
    pub provider_options: Option<serde_json::Value>,
    pub metadata: Option<std::collections::HashMap<String, String>>,
    pub max_retries: u32,
    pub timeout: Option<TimeoutOptions>,
    pub client: Option<Arc<Client>>,
    /// Cancellation token to interrupt generation (Section 4.8).
    pub abort_signal: Option<CancellationToken>,
    /// Custom stop condition checked after each tool round (Section 4.3).
    pub stop_when: Option<StopCondition>,
    /// Callback to repair invalid tool call arguments (Section 5.8).
    pub repair_tool_call: Option<RepairToolCallFn>,
}

impl GenerateParams {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            prompt: None,
            messages: None,
            system: None,
            tools: None,
            tool_choice: None,
            max_tool_rounds: 1,
            response_format: None,
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop_sequences: None,
            reasoning_effort: None,
            speed: None,
            provider: None,
            provider_options: None,
            metadata: None,
            max_retries: 2,
            timeout: None,
            client: None,
            abort_signal: None,
            stop_when: None,
            repair_tool_call: None,
        }
    }

    #[must_use]
    pub fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
        self
    }

    #[must_use]
    pub fn messages(mut self, messages: Vec<Message>) -> Self {
        self.messages = Some(messages);
        self
    }

    #[must_use]
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    #[must_use]
    pub fn client(mut self, client: Arc<Client>) -> Self {
        self.client = Some(client);
        self
    }

    #[must_use]
    pub fn tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = Some(tools.into_iter().map(Arc::new).collect());
        self
    }

    #[must_use]
    pub const fn max_tool_rounds(mut self, rounds: u32) -> Self {
        self.max_tool_rounds = rounds;
        self
    }

    #[must_use]
    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    #[must_use]
    pub fn tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = Some(tool_choice);
        self
    }

    #[must_use]
    pub fn response_format(mut self, response_format: ResponseFormat) -> Self {
        self.response_format = Some(response_format);
        self
    }

    #[must_use]
    pub const fn temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    #[must_use]
    pub const fn top_p(mut self, top_p: f64) -> Self {
        self.top_p = Some(top_p);
        self
    }

    #[must_use]
    pub const fn max_tokens(mut self, max_tokens: i64) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    #[must_use]
    pub fn stop_sequences(mut self, stop_sequences: Vec<String>) -> Self {
        self.stop_sequences = Some(stop_sequences);
        self
    }

    #[must_use]
    pub fn reasoning_effort(mut self, reasoning_effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(reasoning_effort);
        self
    }

    #[must_use]
    pub fn provider_options(mut self, provider_options: serde_json::Value) -> Self {
        self.provider_options = Some(provider_options);
        self
    }

    #[must_use]
    pub fn metadata(mut self, metadata: std::collections::HashMap<String, String>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    #[must_use]
    pub const fn max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    #[must_use]
    pub const fn timeout(mut self, timeout: TimeoutOptions) -> Self {
        self.timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn abort_signal(mut self, token: CancellationToken) -> Self {
        self.abort_signal = Some(token);
        self
    }

    /// Set a custom stop condition for the tool loop (Section 4.3).
    ///
    /// The callback receives the accumulated steps so far and returns `true`
    /// to stop the tool loop early.
    #[must_use]
    pub fn stop_when(mut self, f: impl Fn(&[StepResult]) -> bool + Send + Sync + 'static) -> Self {
        self.stop_when = Some(Arc::new(f));
        self
    }

    #[must_use]
    pub fn repair_tool_call(mut self, repair: RepairToolCallFn) -> Self {
        self.repair_tool_call = Some(repair);
        self
    }
}

/// `StreamAccumulator` collects stream events into a complete Response (Section
/// 4.4).
pub struct StreamAccumulator {
    text_parts: Vec<String>,
    reasoning_parts: Vec<String>,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<FinishReason>,
    usage: Option<TokenCounts>,
    response: Option<Response>,
}

impl StreamAccumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            text_parts: Vec::new(),
            reasoning_parts: Vec::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            response: None,
        }
    }

    pub fn process(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::TextDelta { delta, .. } => {
                self.text_parts.push(delta.clone());
            }
            StreamEvent::ReasoningDelta { delta } => {
                self.reasoning_parts.push(delta.clone());
            }
            StreamEvent::ToolCallEnd { tool_call } => {
                self.tool_calls.push(tool_call.clone());
            }
            StreamEvent::Finish {
                finish_reason,
                usage,
                response,
            } => {
                self.finish_reason = Some(finish_reason.clone());
                self.usage = Some(usage.clone());
                self.response = Some(*response.clone());
                debug!(
                    model = %response.model,
                    input_tokens = response.usage.input_tokens,
                    output_tokens = response.usage.output_tokens,
                    "LLM stream complete"
                );
            }
            _ => {}
        }
    }

    #[must_use]
    pub const fn response(&self) -> Option<&Response> {
        self.response.as_ref()
    }

    #[must_use]
    pub fn text(&self) -> String {
        self.text_parts.join("")
    }

    #[must_use]
    pub fn reasoning(&self) -> Option<String> {
        if self.reasoning_parts.is_empty() {
            None
        } else {
            Some(self.reasoning_parts.join(""))
        }
    }
}

impl Default for StreamAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps a streaming response with an internal `StreamAccumulator` and
/// convenience methods.
///
/// Implements `Stream<Item = Result<StreamEvent, Error>>` so it can be used
/// as a drop-in replacement for `StreamEventStream`. Also supports multi-step
/// tool loops when active tools are provided.
pub struct StreamResult {
    inner: StreamEventStream,
    accumulator: StreamAccumulator,
}

impl StreamResult {
    fn new(inner: StreamEventStream) -> Self {
        Self {
            inner,
            accumulator: StreamAccumulator::new(),
        }
    }

    /// Returns the accumulated response after the stream has ended.
    #[must_use]
    pub const fn response(&self) -> Option<&Response> {
        self.accumulator.response()
    }

    /// Returns the current partially accumulated response state.
    #[must_use]
    pub const fn partial_response(&self) -> Option<&Response> {
        self.accumulator.response()
    }

    /// Returns a stream that yields only text delta strings.
    #[must_use]
    pub fn text_stream(self) -> Pin<Box<dyn Stream<Item = Result<String, Error>> + Send>> {
        Box::pin(self.filter_map(|result| {
            future::ready(match result {
                Ok(StreamEvent::TextDelta { delta, .. }) => Some(Ok(delta)),
                Err(e) => Some(Err(e)),
                _ => None,
            })
        }))
    }
}

impl Stream for StreamResult {
    type Item = Result<StreamEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let inner = self.inner.as_mut();
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                self.accumulator.process(&event);
                Poll::Ready(Some(Ok(event)))
            }
            other => other,
        }
    }
}

/// High-level streaming generation (Section 4.4).
/// Returns a `StreamResult` that the caller can iterate over.
/// Supports multi-step tool loops when active tools are provided.
///
/// # Errors
///
/// Returns `Error::Configuration` if both `prompt` and `messages` are set,
/// or any provider error encountered during streaming setup.
pub async fn stream(params: GenerateParams) -> Result<StreamResult, Error> {
    let inner = stream_with_tool_loop(params).await?;
    Ok(StreamResult::new(inner))
}

/// Streaming generation with multi-step tool loop support.
///
/// When active tools are provided and the model returns tool calls:
/// - Collects the stream to get the complete first response
/// - Executes tools concurrently
/// - Starts a new stream with updated conversation
/// - Yields all events from all rounds seamlessly
/// - Continues until no more tool calls or `max_tool_rounds` reached
///
/// # Errors
///
/// Returns `Error::Configuration` if both `prompt` and `messages` are set,
/// or any provider error encountered during streaming setup.
async fn stream_with_tool_loop(params: GenerateParams) -> Result<StreamEventStream, Error> {
    let client = match params.client.clone() {
        Some(c) => c,
        None => get_default_client().await?,
    };
    let mut messages = build_initial_messages(&params)?;
    let tool_definitions: Option<Vec<ToolDefinition>> = params
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.definition.clone()).collect());
    let abort_signal = params.abort_signal.clone();
    let max_tool_rounds = params.max_tool_rounds;
    let repair_tool_call = params.repair_tool_call.clone();

    let has_active_tools = max_tool_rounds > 0
        && params
            .tools
            .as_ref()
            .is_some_and(|tools| tools.iter().any(|t| t.is_active()));

    debug!(model = %params.model, "Starting LLM stream");

    if !has_active_tools {
        // No tool loop needed, just stream directly
        return stream_generate_raw(&client, &params, &messages, tool_definitions.as_deref()).await;
    }

    // Tool loop: collect events from each round, execute tools, continue
    let (tx, rx) = mpsc::channel::<Result<StreamEvent, Error>>(64);

    let tools = params.tools.clone();
    let retry_policy = RetryPolicy {
        max_retries: params.max_retries,
        backoff: BackoffPolicy {
            initial_delay: std::time::Duration::from_micros(1),
            jitter: false,
            ..Default::default()
        },
        ..Default::default()
    };

    tokio::spawn(async move {
        let tool_loop_future = async {
            let mut round = 0u32;
            let mut steps: Vec<StepResult> = Vec::new();

            loop {
                if let Some(ref token) = abort_signal {
                    if token.is_cancelled() {
                        let _ = tx
                            .send(Err(Error::Interrupt {
                                message: "Stream interrupted by cancellation token".into(),
                            }))
                            .await;
                        return;
                    }
                }

                let request = build_request(&params, &messages, tool_definitions.as_deref());

                // Retry initial connection (Section 6.6), with optional per_step timeout
                let stream_connect = retry(&retry_policy, || {
                    let c = client.clone();
                    let r = request.clone();
                    async move { c.stream(&r).await }
                });

                let stream_result =
                    if let Some(per_step) = params.timeout.as_ref().and_then(|t| t.per_step) {
                        let duration = std::time::Duration::from_secs_f64(per_step);
                        time::timeout(duration, stream_connect)
                            .await
                            .unwrap_or_else(|_| {
                                Err(Error::RequestTimeout {
                                    message: format!("Per-step timeout of {per_step}s exceeded"),
                                    source: None,
                                })
                            })
                    } else {
                        stream_connect.await
                    };

                let mut inner_stream = match stream_result {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                };

                // Collect stream and forward events, accumulating for tool call detection
                let mut accumulator = StreamAccumulator::new();

                while let Some(item) = inner_stream.next().await {
                    if let Some(ref token) = abort_signal {
                        if token.is_cancelled() {
                            let _ = tx
                                .send(Err(Error::Interrupt {
                                    message: "Stream interrupted by cancellation token".into(),
                                }))
                                .await;
                            return;
                        }
                    }

                    if let Ok(event) = &item {
                        accumulator.process(event);
                    } else {
                        let _ = tx.send(item).await;
                        return;
                    }

                    // Forward the event to the consumer
                    if tx.send(item).await.is_err() {
                        return; // Consumer dropped
                    }
                }

                // Check if we should continue with tool calls
                let response = match accumulator.response() {
                    Some(r) => r.clone(),
                    None => return, // No response accumulated, stream ended
                };

                let tool_calls = response.tool_calls();
                if tool_calls.is_empty()
                    || response.finish_reason != FinishReason::ToolCalls
                    || round >= max_tool_rounds
                {
                    return; // No more tool rounds needed
                }

                // Execute tools
                let Some(tool_list) = &tools else { return };

                let tool_refs: Vec<&Tool> =
                    tool_list.iter().map(std::convert::AsRef::as_ref).collect();
                let tool_results = execute_all_tools_with_repair(
                    &tool_refs,
                    &tool_calls,
                    &messages,
                    abort_signal.as_ref(),
                    repair_tool_call.as_ref(),
                )
                .await;

                if tool_results.is_empty() {
                    return;
                }

                // Track step results for stop_when
                steps.push(StepResult {
                    response: response.clone(),
                    tool_results: tool_results.clone(),
                });

                // Check stop_when condition (Section 4.3)
                if params.stop_when.as_ref().is_some_and(|f| f(&steps)) {
                    // Emit StepFinish but do not continue to next round
                    let step_finish = StreamEvent::step_finish(
                        response.finish_reason.clone(),
                        response.usage.clone(),
                        response,
                        tool_calls,
                        tool_results,
                    );
                    let _ = tx.send(Ok(step_finish)).await;
                    return;
                }

                // Emit StepFinish event between steps
                let step_finish = StreamEvent::step_finish(
                    response.finish_reason.clone(),
                    response.usage.clone(),
                    response.clone(),
                    tool_calls,
                    tool_results.clone(),
                );
                if tx.send(Ok(step_finish)).await.is_err() {
                    return; // Consumer dropped
                }

                // Append assistant message and tool results to conversation
                messages.push(response.message.clone());
                for result in &tool_results {
                    messages.push(Message::tool_result(
                        &result.tool_call_id,
                        result.content.clone(),
                        result.is_error,
                    ));
                }

                round += 1;
            }
        };

        // Apply total timeout if configured (Section 4.7)
        if let Some(total) = params.timeout.as_ref().and_then(|t| t.total) {
            let duration = std::time::Duration::from_secs_f64(total);
            if time::timeout(duration, tool_loop_future).await.is_err() {
                let _ = tx
                    .send(Err(Error::RequestTimeout {
                        message: format!("Total timeout of {total}s exceeded"),
                        source: None,
                    }))
                    .await;
            }
        } else {
            tool_loop_future.await;
        }
    });

    Ok(Box::pin(ReceiverStream::new(rx)))
}

/// Internal single-round streaming (no tool loop). Used by `stream_object()`.
async fn stream_generate_raw(
    client: &Arc<Client>,
    params: &GenerateParams,
    messages: &[Message],
    tool_definitions: Option<&[ToolDefinition]>,
) -> Result<StreamEventStream, Error> {
    let request = build_request(params, messages, tool_definitions);

    // Apply per_step timeout to the initial connection (Section 4.7)
    let inner_stream = if let Some(per_step) = params.timeout.as_ref().and_then(|t| t.per_step) {
        let duration = std::time::Duration::from_secs_f64(per_step);
        time::timeout(duration, client.stream(&request))
            .await
            .map_err(|_| Error::RequestTimeout {
                message: format!("Per-step timeout of {per_step}s exceeded"),
                source: None,
            })??
    } else {
        client.stream(&request).await?
    };

    // Apply interrupt signal if present
    let stream: StreamEventStream = if let Some(ref token) = params.abort_signal {
        let token = token.clone();
        let mapped = inner_stream.map(move |item| {
            if token.is_cancelled() {
                return Err(Error::Interrupt {
                    message: "Stream interrupted by cancellation token".into(),
                });
            }
            item
        });
        Box::pin(mapped)
    } else {
        inner_stream
    };

    // Apply total timeout to the stream (Section 4.7)
    if let Some(total) = params.timeout.as_ref().and_then(|t| t.total) {
        let duration = std::time::Duration::from_secs_f64(total);
        let deadline = time::Instant::now() + duration;
        let total_copy = total;
        let timed_stream = stream::unfold((stream, false), move |(mut stream, done)| async move {
            if done {
                return None;
            }
            match time::timeout_at(deadline, stream.next()).await {
                Ok(Some(item)) => Some((item, (stream, false))),
                Ok(None) => None, // stream completed naturally
                Err(_) => Some((
                    Err(Error::RequestTimeout {
                        message: format!("Total timeout of {total_copy}s exceeded"),
                        source: None,
                    }),
                    (stream, true),
                )),
            }
        });
        Ok(Box::pin(timed_stream))
    } else {
        Ok(stream)
    }
}

/// High-level streaming generation (Section 4.4).
/// Returns a `StreamEventStream` that the caller can iterate over.
///
/// Alias: prefer [`stream()`] for consistency with the spec.
///
/// # Errors
///
/// Returns `Error::Configuration` if both `prompt` and `messages` are set,
/// or any provider error encountered during streaming setup.
pub async fn stream_generate(params: GenerateParams) -> Result<StreamEventStream, Error> {
    let client = match params.client.clone() {
        Some(c) => c,
        None => get_default_client().await?,
    };
    let messages = build_initial_messages(&params)?;
    let tool_definitions: Option<Vec<ToolDefinition>> = params
        .tools
        .as_ref()
        .map(|tools| tools.iter().map(|t| t.definition.clone()).collect());

    stream_generate_raw(&client, &params, &messages, tool_definitions.as_deref()).await
}

/// Structured output generation with schema validation (Section 4.5).
///
/// # Errors
///
/// Returns `Error::NoObjectGenerated` if the response is not valid JSON,
/// or any error from `generate()`.
pub async fn generate_object(
    params: GenerateParams,
    schema: serde_json::Value,
) -> Result<GenerateResult, Error> {
    let params = GenerateParams {
        response_format: Some(ResponseFormat {
            kind: ResponseFormatType::JsonSchema,
            json_schema: Some(schema),
            strict: true,
        }),
        ..params
    };

    let mut result = generate(params).await?;

    // Try to parse the text as JSON
    match serde_json::from_str::<serde_json::Value>(&result.text()) {
        Ok(parsed) => {
            result.output = Some(parsed);
            Ok(result)
        }
        Err(e) => Err(Error::NoObjectGenerated {
            message: format!("Failed to parse response as JSON: {e}"),
        }),
    }
}

/// Stream type for `stream_object()`.
pub type ObjectStream =
    Pin<Box<dyn futures::Stream<Item = Result<ObjectStreamEvent, Error>> + Send>>;

/// Wraps an `ObjectStream` with an `object()` accessor for the final parsed
/// value.
///
/// Implements `Stream<Item = Result<ObjectStreamEvent, Error>>` so it can be
/// used as a drop-in replacement for `ObjectStream`. Tracks the last `Complete`
/// event's object internally so callers can retrieve it after the stream ends.
pub struct ObjectStreamResult {
    inner: ObjectStream,
    object: Option<serde_json::Value>,
}

impl ObjectStreamResult {
    fn new(inner: ObjectStream) -> Self {
        Self {
            inner,
            object: None,
        }
    }

    /// Returns the final parsed object after the stream has yielded a
    /// `Complete` event.
    #[must_use]
    pub const fn object(&self) -> Option<&serde_json::Value> {
        self.object.as_ref()
    }
}

impl Stream for ObjectStreamResult {
    type Item = Result<ObjectStreamEvent, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let inner = self.inner.as_mut();
        match inner.poll_next(cx) {
            Poll::Ready(Some(Ok(event))) => {
                if let ObjectStreamEvent::Complete { ref object, .. } = event {
                    self.object = Some(object.clone());
                }
                Poll::Ready(Some(Ok(event)))
            }
            other => other,
        }
    }
}

/// Streaming structured output with incremental JSON parsing (Section 4.6).
///
/// Combines streaming with structured output: sets `response_format` to
/// `json_schema`, streams the response, and attempts to parse the accumulated
/// text as JSON on each text delta. Yields `ObjectStreamEvent::Partial` when a
/// new valid partial parse is obtained, `ObjectStreamEvent::Delta` for every
/// raw stream event, and `ObjectStreamEvent::Complete` when the stream finishes
/// with the final parsed object.
///
/// # Errors
///
/// Returns `Error::Configuration` if both `prompt` and `messages` are set,
/// `Error::NoObjectGenerated` if the final accumulated text is not valid
/// JSON, or any provider error encountered during streaming.
pub async fn stream_object(
    params: GenerateParams,
    schema: serde_json::Value,
) -> Result<ObjectStreamResult, Error> {
    let params = GenerateParams {
        response_format: Some(ResponseFormat {
            kind: ResponseFormatType::JsonSchema,
            json_schema: Some(schema),
            strict: true,
        }),
        ..params
    };

    let inner_stream = stream(params).await?;

    let mapped = inner_stream.scan(
        (String::new(), Option::<serde_json::Value>::None),
        |(accumulated_text, last_parsed), event| {
            let mut events: Vec<Result<ObjectStreamEvent, Error>> = Vec::new();

            match &event {
                Ok(stream_event) => {
                    // Accumulate text from TextDelta events
                    if let StreamEvent::TextDelta { delta, .. } = stream_event {
                        accumulated_text.push_str(delta);

                        // Try incremental JSON parse
                        if let Ok(parsed) =
                            serde_json::from_str::<serde_json::Value>(accumulated_text)
                        {
                            if last_parsed.as_ref() != Some(&parsed) {
                                *last_parsed = Some(parsed.clone());
                                events.push(Ok(ObjectStreamEvent::Partial { object: parsed }));
                            }
                        }
                    }

                    // On Finish, yield the Complete event with final parsed object
                    if let StreamEvent::Finish { response, .. } = stream_event {
                        match serde_json::from_str::<serde_json::Value>(accumulated_text) {
                            Ok(final_object) => {
                                events.push(Ok(ObjectStreamEvent::Complete {
                                    object: final_object,
                                    response: response.clone(),
                                }));
                            }
                            Err(e) => {
                                events.push(Err(Error::NoObjectGenerated {
                                    message: format!("Failed to parse final response as JSON: {e}"),
                                }));
                            }
                        }
                    } else {
                        // Yield the raw delta event
                        events.push(Ok(ObjectStreamEvent::Delta {
                            event: stream_event.clone(),
                        }));
                    }
                }
                Err(e) => {
                    events.push(Err(Error::Stream {
                        message: format!("{e}"),
                        source: None,
                    }));
                }
            }

            future::ready(Some(stream::iter(events)))
        },
    );

    Ok(ObjectStreamResult::new(Box::pin(mapped.flatten())))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    use futures::{StreamExt, stream};
    use tokio::time::sleep;

    use super::*;
    use crate::client::Client;
    use crate::error::{ProviderErrorDetail, ProviderErrorKind};
    use crate::provider::ProviderAdapter;
    use crate::types::{ContentPart, Role, ToolResult};

    /// Mock provider that returns configurable responses.
    struct MockProvider {
        response_text: String,
    }

    impl MockProvider {
        fn new(text: &str) -> Self {
            Self {
                response_text: text.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            Ok(Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant(&self.response_text),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                raw: None,
                warnings: vec![],
                rate_limit: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            let text = self.response_text.clone();
            let events = vec![
                Ok(StreamEvent::text_delta(&text, Some("t1".into()))),
                Ok(StreamEvent::finish(
                    FinishReason::Stop,
                    TokenCounts {
                        input_tokens: 10,
                        output_tokens: 20,
                        ..Default::default()
                    },
                    Response {
                        id: "resp_1".into(),
                        model: "mock-model".into(),
                        provider: "mock".into(),
                        message: Message::assistant(&text),
                        finish_reason: FinishReason::Stop,
                        usage: TokenCounts {
                            input_tokens: 10,
                            output_tokens: 20,
                            ..Default::default()
                        },
                        raw: None,
                        warnings: vec![],
                        rate_limit: None,
                    },
                )),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn mock_client(text: &str) -> Arc<Client> {
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), Arc::new(MockProvider::new(text)));
        Arc::new(Client::new(providers, Some("mock".to_string()), vec![]))
    }

    #[tokio::test]
    async fn generate_simple_text() {
        let result = generate(
            GenerateParams::new("mock-model")
                .prompt("Hello")
                .client(mock_client("Hi there!")),
        )
        .await
        .unwrap();

        assert_eq!(result.text(), "Hi there!");
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.usage.input_tokens, 10);
        assert_eq!(result.steps.len(), 1);
    }

    #[tokio::test]
    async fn generate_with_system_message() {
        let result = generate(
            GenerateParams::new("mock-model")
                .system("You are helpful")
                .prompt("Hello")
                .client(mock_client("Greetings!")),
        )
        .await
        .unwrap();

        assert_eq!(result.text(), "Greetings!");
    }

    #[tokio::test]
    async fn generate_with_messages() {
        let result = generate(
            GenerateParams::new("mock-model")
                .messages(vec![
                    Message::user("Hello"),
                    Message::assistant("Hi"),
                    Message::user("How are you?"),
                ])
                .client(mock_client("I'm doing well!")),
        )
        .await
        .unwrap();

        assert_eq!(result.text(), "I'm doing well!");
    }

    #[tokio::test]
    async fn generate_errors_on_both_prompt_and_messages() {
        let result = generate(GenerateParams {
            model: "mock-model".into(),
            prompt: Some("Hello".into()),
            messages: Some(vec![Message::user("World")]),
            client: Some(mock_client("test")),
            ..GenerateParams::new("mock-model")
        })
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Configuration { .. }));
    }

    /// Mock provider that returns tool calls then text
    struct ToolCallMockProvider {
        call_count: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for ToolCallMockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);

            if count == 0 {
                // First call: return tool call
                Ok(Response {
                    id: "resp_1".into(),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message {
                        role: Role::Assistant,
                        content: vec![ContentPart::ToolCall(ToolCall::new(
                            "call_1",
                            "get_weather",
                            serde_json::json!({"city": "SF"}),
                        ))],
                        name: None,
                        tool_call_id: None,
                    },
                    finish_reason: FinishReason::ToolCalls,
                    usage: TokenCounts {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                })
            } else {
                // Second call: return text
                Ok(Response {
                    id: "resp_2".into(),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message::assistant("The weather in SF is 72F"),
                    finish_reason: FinishReason::Stop,
                    usage: TokenCounts {
                        input_tokens: 20,
                        output_tokens: 10,
                        ..Default::default()
                    },
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                })
            }
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            Ok(Box::pin(stream::empty()))
        }
    }

    #[tokio::test]
    async fn generate_with_tool_loop() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(ToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let result = generate(
            GenerateParams::new("mock-model")
                .prompt("What's the weather in SF?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |args, _ctx| async move {
                        let city = args["city"].as_str().unwrap_or("unknown");
                        Ok(serde_json::json!(format!("72F in {}", city)))
                    },
                )])
                .max_tool_rounds(5)
                .client(client),
        )
        .await
        .unwrap();

        assert_eq!(result.text(), "The weather in SF is 72F");
        assert_eq!(result.steps.len(), 2);
        assert_eq!(result.total_usage.input_tokens, 30);
        assert_eq!(result.total_usage.output_tokens, 15);
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stream_accumulator_collects_events() {
        let mut acc = StreamAccumulator::new();

        acc.process(&StreamEvent::TextStart {
            text_id: Some("t1".into()),
        });

        acc.process(&StreamEvent::text_delta("Hello", Some("t1".into())));
        acc.process(&StreamEvent::text_delta(" world", Some("t1".into())));

        let resp = Response {
            id: "r1".into(),
            model: "m".into(),
            provider: "p".into(),
            message: Message::assistant("Hello world"),
            finish_reason: FinishReason::Stop,
            usage: TokenCounts {
                input_tokens: 5,
                output_tokens: 2,
                ..Default::default()
            },
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };

        acc.process(&StreamEvent::finish(
            FinishReason::Stop,
            resp.usage.clone(),
            resp,
        ));

        assert_eq!(acc.text(), "Hello world");
        assert_eq!(acc.reasoning(), None);
        assert!(acc.response().is_some());
        assert_eq!(acc.response().unwrap().text(), "Hello world");
    }

    #[tokio::test]
    async fn stream_accumulator_collects_reasoning() {
        let mut acc = StreamAccumulator::new();

        acc.process(&StreamEvent::ReasoningDelta {
            delta: "Let me think...".into(),
        });

        assert_eq!(acc.reasoning(), Some("Let me think...".to_string()));
    }

    #[tokio::test]
    async fn stream_generate_returns_events() {
        let client = mock_client("Hello stream!");
        let mut stream = stream_generate(
            GenerateParams::new("mock-model")
                .prompt("Hi")
                .client(client),
        )
        .await
        .unwrap();

        let first = stream.next().await.unwrap().unwrap();
        match &first {
            StreamEvent::TextDelta { delta, .. } => assert_eq!(delta, "Hello stream!"),
            other => panic!("Expected TextDelta, got {other:?}"),
        }

        let second = stream.next().await.unwrap().unwrap();
        assert!(matches!(second, StreamEvent::Finish { .. }));
    }

    #[tokio::test]
    async fn generate_object_parses_json() {
        // Create a mock that returns valid JSON
        let client = mock_client(r#"{"name": "Alice", "age": 30}"#);

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name", "age"]
        });

        let result = generate_object(
            GenerateParams::new("mock-model")
                .prompt("Extract name and age")
                .client(client),
            schema,
        )
        .await
        .unwrap();

        assert!(result.output.is_some());
        let output = result.output.unwrap();
        assert_eq!(output["name"], "Alice");
        assert_eq!(output["age"], 30);
    }

    #[tokio::test]
    async fn generate_object_errors_on_invalid_json() {
        let client = mock_client("not valid json");

        let result = generate_object(
            GenerateParams::new("mock-model")
                .prompt("Extract data")
                .client(client),
            serde_json::json!({"type": "object"}),
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Error::NoObjectGenerated { .. }
        ));
    }

    #[tokio::test]
    async fn generate_stop_when_halts_tool_loop() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(ToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let result = generate(
            GenerateParams::new("mock-model")
                .prompt("What's the weather in SF?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |args, _ctx| async move {
                        let city = args["city"].as_str().unwrap_or("unknown");
                        Ok(serde_json::json!(format!("72F in {}", city)))
                    },
                )])
                .max_tool_rounds(5)
                .stop_when(|_steps| true) // Stop immediately after first round
                .client(client),
        )
        .await
        .unwrap();

        // stop_when returned true, so the tool loop should stop after 1 step
        assert_eq!(result.steps.len(), 1);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn generate_params_builder_methods() {
        let params = GenerateParams::new("test-model")
            .prompt("hello")
            .system("you are helpful")
            .temperature(0.7)
            .top_p(0.9)
            .max_tokens(100)
            .stop_sequences(vec!["STOP".to_string()])
            .reasoning_effort(ReasoningEffort::High)
            .provider("anthropic")
            .provider_options(serde_json::json!({"key": "value"}))
            .max_retries(5)
            .tool_choice(ToolChoice::Required)
            .response_format(ResponseFormat {
                kind: ResponseFormatType::JsonObject,
                json_schema: None,
                strict: false,
            })
            .max_tool_rounds(3);

        assert_eq!(params.model, "test-model");
        assert_eq!(params.prompt.as_deref(), Some("hello"));
        assert_eq!(params.system.as_deref(), Some("you are helpful"));
        assert_eq!(params.temperature, Some(0.7));
        assert_eq!(params.top_p, Some(0.9));
        assert_eq!(params.max_tokens, Some(100));
        assert_eq!(params.stop_sequences, Some(vec!["STOP".to_string()]));
        assert_eq!(params.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(params.provider.as_deref(), Some("anthropic"));
        assert!(params.provider_options.is_some());
        assert_eq!(params.max_retries, 5);
        assert_eq!(params.tool_choice, Some(ToolChoice::Required));
        assert!(params.response_format.is_some());
        assert_eq!(params.max_tool_rounds, 3);
    }

    #[test]
    fn generate_params_timeout_builder() {
        let params = GenerateParams::new("test-model").timeout(TimeoutOptions {
            total: Some(30.0),
            per_step: Some(10.0),
        });
        assert!(params.timeout.is_some());
        let t = params.timeout.unwrap();
        assert_eq!(t.total, Some(30.0));
        assert_eq!(t.per_step, Some(10.0));
    }

    /// Mock provider that streams JSON tokens incrementally.
    struct StreamingJsonMockProvider {
        deltas: Vec<String>,
        full_text: String,
    }

    impl StreamingJsonMockProvider {
        fn new(deltas: Vec<&str>) -> Self {
            let full_text: String = deltas.iter().copied().collect();
            Self {
                deltas: deltas.into_iter().map(String::from).collect(),
                full_text,
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for StreamingJsonMockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            Ok(Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant(&self.full_text),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts::default(),
                raw: None,
                warnings: vec![],
                rate_limit: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            let mut events: Vec<Result<StreamEvent, Error>> = self
                .deltas
                .iter()
                .map(|d| Ok(StreamEvent::text_delta(d.as_str(), Some("t1".into()))))
                .collect();

            events.push(Ok(StreamEvent::finish(
                FinishReason::Stop,
                TokenCounts {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                Response {
                    id: "resp_1".into(),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message::assistant(&self.full_text),
                    finish_reason: FinishReason::Stop,
                    usage: TokenCounts {
                        input_tokens: 10,
                        output_tokens: 20,
                        ..Default::default()
                    },
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                },
            )));

            Ok(Box::pin(stream::iter(events)))
        }
    }

    fn streaming_json_mock_client(deltas: Vec<&str>) -> Arc<Client> {
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert(
            "mock".to_string(),
            Arc::new(StreamingJsonMockProvider::new(deltas)),
        );
        Arc::new(Client::new(providers, Some("mock".to_string()), vec![]))
    }

    #[tokio::test]
    async fn stream_object_yields_complete_event() {
        let client = streaming_json_mock_client(vec![r#"{"name": "Alice", "age": 30}"#]);

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            },
            "required": ["name", "age"]
        });

        let obj_stream = stream_object(
            GenerateParams::new("mock-model")
                .prompt("Extract info")
                .client(client),
            schema,
        )
        .await
        .unwrap();

        let events: Vec<ObjectStreamEvent> = obj_stream
            .filter_map(|r| future::ready(r.ok()))
            .collect()
            .await;

        let complete = events
            .iter()
            .find(|e| matches!(e, ObjectStreamEvent::Complete { .. }));
        assert!(complete.is_some(), "Expected a Complete event");

        if let ObjectStreamEvent::Complete { object, .. } = complete.unwrap() {
            assert_eq!(object["name"], "Alice");
            assert_eq!(object["age"], 30);
        }
    }

    #[tokio::test]
    async fn stream_object_yields_partial_events_incrementally() {
        let client =
            streaming_json_mock_client(vec![r#"{"name""#, r#": "Bob""#, r#", "age": 25}"#]);

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "age": {"type": "integer"}
            }
        });

        let obj_stream = stream_object(
            GenerateParams::new("mock-model")
                .prompt("Extract info")
                .client(client),
            schema,
        )
        .await
        .unwrap();

        let events: Vec<ObjectStreamEvent> = obj_stream
            .filter_map(|r| future::ready(r.ok()))
            .collect()
            .await;

        let partial_count = events
            .iter()
            .filter(|e| matches!(e, ObjectStreamEvent::Partial { .. }))
            .count();

        assert!(
            partial_count >= 1,
            "Expected at least one Partial event, got {partial_count}"
        );

        let delta_count = events
            .iter()
            .filter(|e| matches!(e, ObjectStreamEvent::Delta { .. }))
            .count();

        assert_eq!(delta_count, 3);

        let last_complete = events
            .iter()
            .rev()
            .find(|e| matches!(e, ObjectStreamEvent::Complete { .. }));
        assert!(last_complete.is_some(), "Expected a Complete event");
        if let ObjectStreamEvent::Complete { object, .. } = last_complete.unwrap() {
            assert_eq!(object["name"], "Bob");
            assert_eq!(object["age"], 25);
        }
    }

    #[tokio::test]
    async fn stream_object_errors_on_invalid_final_json() {
        let client = streaming_json_mock_client(vec![r#"{"name": "Alice"#]);

        let schema = serde_json::json!({"type": "object"});

        let obj_stream = stream_object(
            GenerateParams::new("mock-model")
                .prompt("Extract info")
                .client(client),
            schema,
        )
        .await
        .unwrap();

        let results: Vec<Result<ObjectStreamEvent, Error>> = obj_stream.collect().await;

        let has_error = results.iter().any(std::result::Result::is_err);
        assert!(has_error, "Expected an error for invalid final JSON");
    }

    #[tokio::test]
    async fn generate_abort_signal_before_call() {
        let token = CancellationToken::new();
        token.cancel();

        let result = generate(
            GenerateParams::new("mock-model")
                .prompt("Hello")
                .client(mock_client("Hi"))
                .abort_signal(token),
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Interrupt { .. }));
    }

    #[tokio::test]
    async fn generate_abort_signal_between_tool_rounds() {
        // Provider that always returns tool calls
        struct AlwaysToolCallProvider {
            call_count: Arc<AtomicU32>,
            cancel_token: CancellationToken,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for AlwaysToolCallProvider {
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, _request: &Request) -> Result<Response, Error> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                // Cancel after first call completes
                if count == 0 {
                    self.cancel_token.cancel();
                }
                Ok(Response {
                    id: format!("resp_{count}"),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message {
                        role: Role::Assistant,
                        content: vec![ContentPart::ToolCall(ToolCall::new(
                            format!("call_{count}"),
                            "get_weather",
                            serde_json::json!({"city": "SF"}),
                        ))],
                        name: None,
                        tool_call_id: None,
                    },
                    finish_reason: FinishReason::ToolCalls,
                    usage: TokenCounts::default(),
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                })
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
                Ok(Box::pin(stream::empty()))
            }
        }

        let call_count = Arc::new(AtomicU32::new(0));
        let token = CancellationToken::new();
        let token_clone = token.clone();

        let provider: Arc<dyn ProviderAdapter> = Arc::new(AlwaysToolCallProvider {
            call_count: call_count.clone(),
            cancel_token: token_clone,
        });
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let result = generate(
            GenerateParams::new("mock-model")
                .prompt("What's the weather?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(10)
                .abort_signal(token)
                .client(client),
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Error::Interrupt { .. }));
        // Should have only made 1 call before aborting
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_abort_signal_terminates_stream() {
        let token = CancellationToken::new();
        let token_clone = token.clone();

        // Create a mock that produces events, but cancel after stream starts
        let client = mock_client("Hello stream!");
        token_clone.cancel();

        let mut stream_result = stream(
            GenerateParams::new("mock-model")
                .prompt("Hi")
                .client(client)
                .abort_signal(token),
        )
        .await
        .unwrap();

        let first = stream_result.next().await.unwrap();
        assert!(first.is_err());
        assert!(matches!(first.unwrap_err(), Error::Interrupt { .. }));
    }

    #[tokio::test]
    async fn generate_max_tool_rounds_zero_skips_tool_execution() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(ToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let tool_executed = Arc::new(AtomicU32::new(0));
        let tool_executed_clone = tool_executed.clone();

        let result = generate(
            GenerateParams::new("mock-model")
                .prompt("What's the weather in SF?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    move |_args, _ctx| {
                        let counter = tool_executed_clone.clone();
                        async move {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(serde_json::json!("72F"))
                        }
                    },
                )])
                .max_tool_rounds(0)
                .client(client),
        )
        .await
        .unwrap();

        // Should return after first LLM call without executing any tools
        assert_eq!(result.steps.len(), 1);
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
        assert_eq!(tool_executed.load(Ordering::SeqCst), 0);
        // The tool results should be empty since tools were not executed
        assert!(result.tool_results.is_empty());
    }

    #[test]
    fn generate_params_abort_signal_builder() {
        let token = CancellationToken::new();
        let params = GenerateParams::new("test-model").abort_signal(token);
        assert!(params.abort_signal.is_some());
    }

    #[tokio::test]
    async fn stream_result_accumulates_response() {
        let client = mock_client("Hello!");
        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("Hi")
                .client(client),
        )
        .await
        .unwrap();

        assert!(result.response().is_none());
        assert!(result.partial_response().is_none());

        // Consume all events
        while result.next().await.is_some() {}

        assert!(result.response().is_some());
        assert_eq!(result.response().unwrap().text(), "Hello!");
    }

    #[tokio::test]
    async fn stream_result_text_stream() {
        let client = streaming_json_mock_client(vec!["Hello", " ", "world"]);
        let result = stream(
            GenerateParams::new("mock-model")
                .prompt("Hi")
                .client(client),
        )
        .await
        .unwrap();

        let texts: Vec<String> = result
            .text_stream()
            .filter_map(|r| future::ready(r.ok()))
            .collect()
            .await;

        assert_eq!(texts, vec!["Hello", " ", "world"]);
    }

    /// Mock provider that streams tool calls then text on second stream
    struct StreamingToolCallMockProvider {
        call_count: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for StreamingToolCallMockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            Ok(Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant("fallback"),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts::default(),
                raw: None,
                warnings: vec![],
                rate_limit: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);

            if count == 0 {
                // First stream: return tool call
                let tool_call =
                    ToolCall::new("call_1", "get_weather", serde_json::json!({"city": "SF"}));
                let response = Response {
                    id: "resp_1".into(),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message {
                        role: Role::Assistant,
                        content: vec![ContentPart::ToolCall(tool_call.clone())],
                        name: None,
                        tool_call_id: None,
                    },
                    finish_reason: FinishReason::ToolCalls,
                    usage: TokenCounts {
                        input_tokens: 10,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                };
                let events = vec![
                    Ok(StreamEvent::ToolCallEnd { tool_call }),
                    Ok(StreamEvent::finish(
                        FinishReason::ToolCalls,
                        response.usage.clone(),
                        response,
                    )),
                ];
                Ok(Box::pin(stream::iter(events)))
            } else {
                // Second stream: return text
                let text = "The weather in SF is 72F";
                let response = Response {
                    id: "resp_2".into(),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message::assistant(text),
                    finish_reason: FinishReason::Stop,
                    usage: TokenCounts {
                        input_tokens: 20,
                        output_tokens: 10,
                        ..Default::default()
                    },
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                };
                let events = vec![
                    Ok(StreamEvent::text_delta(text, Some("t1".into()))),
                    Ok(StreamEvent::finish(
                        FinishReason::Stop,
                        response.usage.clone(),
                        response,
                    )),
                ];
                Ok(Box::pin(stream::iter(events)))
            }
        }
    }

    #[tokio::test]
    async fn stream_with_tool_loop_executes_tools() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(StreamingToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("What's the weather in SF?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(5)
                .client(client),
        )
        .await
        .unwrap();

        // Collect all events
        let mut events = Vec::new();
        while let Some(item) = result.next().await {
            events.push(item);
        }

        // Should have events from both rounds
        assert_eq!(call_count.load(Ordering::SeqCst), 2);

        // Should have text deltas from the second round
        let text_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::TextDelta { delta, .. }) => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_deltas, vec!["The weather in SF is 72F"]);

        // The final response should be the text response
        assert!(result.response().is_some());
        assert_eq!(
            result.response().unwrap().text(),
            "The weather in SF is 72F"
        );
    }

    #[tokio::test]
    async fn stream_no_tool_loop_when_max_rounds_zero() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(StreamingToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("What's the weather?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(0)
                .client(client),
        )
        .await
        .unwrap();

        // Consume all events
        while result.next().await.is_some() {}

        // Only one stream call, no tool execution
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_accumulator_handles_step_finish() {
        let mut acc = StreamAccumulator::new();

        let response = Response {
            id: "resp_1".into(),
            model: "mock-model".into(),
            provider: "mock".into(),
            message: Message::assistant("tool step"),
            finish_reason: FinishReason::ToolCalls,
            usage: TokenCounts {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
            raw: None,
            warnings: vec![],
            rate_limit: None,
        };

        let tool_calls = vec![ToolCall::new(
            "call_1",
            "get_weather",
            serde_json::json!({"city": "SF"}),
        )];

        let tool_results = vec![ToolResult::success("call_1", serde_json::json!("72F"))];

        // Processing StepFinish should not panic and should not set the final response
        acc.process(&StreamEvent::step_finish(
            FinishReason::ToolCalls,
            response.usage.clone(),
            response,
            tool_calls,
            tool_results,
        ));

        // StepFinish should not set the final response (only Finish does that)
        assert!(acc.response().is_none());
        assert_eq!(acc.text(), "");
    }

    #[tokio::test]
    async fn stream_with_tool_loop_emits_step_finish() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(StreamingToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("What's the weather in SF?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(5)
                .client(client),
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Some(item) = result.next().await {
            events.push(item);
        }

        // Should have a StepFinish event between the tool call round and text round
        let step_finish_count = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::StepFinish { .. })))
            .count();
        assert_eq!(
            step_finish_count, 1,
            "Expected exactly one StepFinish event"
        );

        // Verify StepFinish contents
        let step_finish = events
            .iter()
            .find_map(|e| match e {
                Ok(StreamEvent::StepFinish {
                    finish_reason,
                    tool_calls,
                    tool_results,
                    ..
                }) => Some((finish_reason, tool_calls, tool_results)),
                _ => None,
            })
            .expect("StepFinish event should exist");

        assert_eq!(*step_finish.0, FinishReason::ToolCalls);
        assert_eq!(step_finish.1.len(), 1);
        assert_eq!(step_finish.1[0].name, "get_weather");
        assert_eq!(step_finish.2.len(), 1);
        assert_eq!(step_finish.2[0].tool_call_id, "call_1");
    }

    #[tokio::test]
    async fn stream_stop_when_halts_streaming_tool_loop() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(StreamingToolCallMockProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("What's the weather in SF?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(5)
                .stop_when(|_steps| true) // Stop immediately after first round
                .client(client),
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Some(item) = result.next().await {
            events.push(item);
        }

        // stop_when returned true, so only 1 stream call should have been made
        assert_eq!(call_count.load(Ordering::SeqCst), 1);

        // Should have a StepFinish event but no second round text
        let step_finish_count = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::StepFinish { .. })))
            .count();
        assert_eq!(
            step_finish_count, 1,
            "Expected StepFinish event from stopped round"
        );

        // Should NOT have any text deltas (second round never started)
        let text_delta_count = events
            .iter()
            .filter(|e| matches!(e, Ok(StreamEvent::TextDelta { .. })))
            .count();
        assert_eq!(
            text_delta_count, 0,
            "Expected no text deltas since loop was stopped"
        );
    }

    /// Mock provider that fails on stream N times then succeeds
    struct FailThenStreamProvider {
        call_count: Arc<AtomicU32>,
        failures: u32,
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for FailThenStreamProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            Ok(Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant("fallback"),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts::default(),
                raw: None,
                warnings: vec![],
                rate_limit: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);

            if count < self.failures {
                return Err(Error::Provider {
                    kind: ProviderErrorKind::Server,
                    detail: Box::new(ProviderErrorDetail {
                        status_code: Some(500),
                        ..ProviderErrorDetail::new("server error", "mock")
                    }),
                });
            }

            let text = "Hello after retry";
            let response = Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant(text),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts {
                    input_tokens: 10,
                    output_tokens: 20,
                    ..Default::default()
                },
                raw: None,
                warnings: vec![],
                rate_limit: None,
            };
            let events = vec![
                Ok(StreamEvent::text_delta(text, Some("t1".into()))),
                Ok(StreamEvent::finish(
                    FinishReason::Stop,
                    response.usage.clone(),
                    response,
                )),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn stream_retry_on_initial_connection() {
        let call_count = Arc::new(AtomicU32::new(0));
        let provider: Arc<dyn ProviderAdapter> = Arc::new(FailThenStreamProvider {
            call_count: call_count.clone(),
            failures: 2, // fail twice, succeed on third
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        // Need active tools so the tool loop path (with retry) is used
        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("Hi")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(1)
                .max_retries(3)
                .client(client),
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Some(item) = result.next().await {
            events.push(item);
        }

        // Should have called stream 3 times (2 failures + 1 success)
        assert_eq!(call_count.load(Ordering::SeqCst), 3);

        // Should have received the text from the successful attempt
        let text_deltas: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Ok(StreamEvent::TextDelta { delta, .. }) => Some(delta.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text_deltas, vec!["Hello after retry"]);
    }

    /// Mock provider that delays before returning stream
    struct SlowStreamProvider {
        delay: std::time::Duration,
    }

    #[async_trait::async_trait]
    impl ProviderAdapter for SlowStreamProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        async fn complete(&self, _request: &Request) -> Result<Response, Error> {
            Ok(Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant("fallback"),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts::default(),
                raw: None,
                warnings: vec![],
                rate_limit: None,
            })
        }

        async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
            sleep(self.delay).await;
            let text = "Slow response";
            let response = Response {
                id: "resp_1".into(),
                model: "mock-model".into(),
                provider: "mock".into(),
                message: Message::assistant(text),
                finish_reason: FinishReason::Stop,
                usage: TokenCounts::default(),
                raw: None,
                warnings: vec![],
                rate_limit: None,
            };
            let events = vec![
                Ok(StreamEvent::text_delta(text, Some("t1".into()))),
                Ok(StreamEvent::finish(
                    FinishReason::Stop,
                    TokenCounts::default(),
                    response,
                )),
            ];
            Ok(Box::pin(stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn stream_per_step_timeout() {
        let provider: Arc<dyn ProviderAdapter> = Arc::new(SlowStreamProvider {
            delay: std::time::Duration::from_secs(5),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        // Need active tools so the tool loop path (with timeout) is used
        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("Hi")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(1)
                .timeout(TimeoutOptions {
                    total: None,
                    per_step: Some(0.01), // 10ms timeout, provider takes 5s
                })
                .max_retries(0)
                .client(client),
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Some(item) = result.next().await {
            events.push(item);
        }

        // Should have received a timeout error
        let has_timeout = events
            .iter()
            .any(|e| matches!(e, Err(Error::RequestTimeout { .. })));
        assert!(has_timeout, "Expected a RequestTimeout error");
    }

    #[tokio::test]
    async fn stream_total_timeout() {
        // Use a streaming tool call provider with a slow tool to trigger total timeout
        // across multiple rounds
        /// Provider that always returns tool calls with a delay on the second
        /// stream
        struct SlowToolCallStreamProvider {
            call_count: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl ProviderAdapter for SlowToolCallStreamProvider {
            fn name(&self) -> &'static str {
                "mock"
            }

            async fn complete(&self, _request: &Request) -> Result<Response, Error> {
                Ok(Response {
                    id: "resp_1".into(),
                    model: "mock-model".into(),
                    provider: "mock".into(),
                    message: Message::assistant("fallback"),
                    finish_reason: FinishReason::Stop,
                    usage: TokenCounts::default(),
                    raw: None,
                    warnings: vec![],
                    rate_limit: None,
                })
            }

            async fn stream(&self, _request: &Request) -> Result<StreamEventStream, Error> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);

                if count == 0 {
                    // First stream: return tool call quickly
                    let tool_call =
                        ToolCall::new("call_1", "get_weather", serde_json::json!({"city": "SF"}));
                    let response = Response {
                        id: "resp_1".into(),
                        model: "mock-model".into(),
                        provider: "mock".into(),
                        message: Message {
                            role: Role::Assistant,
                            content: vec![ContentPart::ToolCall(tool_call.clone())],
                            name: None,
                            tool_call_id: None,
                        },
                        finish_reason: FinishReason::ToolCalls,
                        usage: TokenCounts::default(),
                        raw: None,
                        warnings: vec![],
                        rate_limit: None,
                    };
                    let events = vec![
                        Ok(StreamEvent::ToolCallEnd { tool_call }),
                        Ok(StreamEvent::finish(
                            FinishReason::ToolCalls,
                            TokenCounts::default(),
                            response,
                        )),
                    ];
                    Ok(Box::pin(stream::iter(events)))
                } else {
                    // Second stream: delay long enough to exceed total timeout
                    sleep(std::time::Duration::from_secs(5)).await;
                    let text = "Should not arrive";
                    let response = Response {
                        id: "resp_2".into(),
                        model: "mock-model".into(),
                        provider: "mock".into(),
                        message: Message::assistant(text),
                        finish_reason: FinishReason::Stop,
                        usage: TokenCounts::default(),
                        raw: None,
                        warnings: vec![],
                        rate_limit: None,
                    };
                    let events = vec![
                        Ok(StreamEvent::text_delta(text, Some("t1".into()))),
                        Ok(StreamEvent::finish(
                            FinishReason::Stop,
                            TokenCounts::default(),
                            response,
                        )),
                    ];
                    Ok(Box::pin(stream::iter(events)))
                }
            }
        }

        let call_count = Arc::new(AtomicU32::new(0));

        let provider: Arc<dyn ProviderAdapter> = Arc::new(SlowToolCallStreamProvider {
            call_count: call_count.clone(),
        });

        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("mock".to_string(), provider);
        let client = Arc::new(Client::new(providers, Some("mock".to_string()), vec![]));

        let mut result = stream(
            GenerateParams::new("mock-model")
                .prompt("What's the weather?")
                .tools(vec![Tool::active(
                    "get_weather",
                    "Get weather",
                    serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
                    |_args, _ctx| async { Ok(serde_json::json!("72F")) },
                )])
                .max_tool_rounds(5)
                .timeout(TimeoutOptions {
                    total: Some(0.05), // 50ms total timeout
                    per_step: None,
                })
                .max_retries(0)
                .client(client),
        )
        .await
        .unwrap();

        let mut events = Vec::new();
        while let Some(item) = result.next().await {
            events.push(item);
        }

        // Should have received a total timeout error
        let has_timeout = events
            .iter()
            .any(|e| matches!(e, Err(Error::RequestTimeout { .. })));
        assert!(
            has_timeout,
            "Expected a RequestTimeout error from total timeout"
        );
    }
}
