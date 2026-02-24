// Pipeline API types and fetch wrappers

export type PipelineStatus = "running" | "completed" | "failed" | "cancelled";

export interface PipelineStatusResponse {
  id: string;
  status: PipelineStatus;
  error?: string;
}

export interface StartPipelineResponse {
  id: string;
}

export interface ApiQuestionOption {
  key: string;
  label: string;
}

export interface ApiQuestion {
  id: string;
  text: string;
  question_type: string;
  options: ApiQuestionOption[];
  allow_freeform: boolean;
}

export interface SubmitAnswerResponse {
  accepted: boolean;
}

export interface Checkpoint {
  timestamp: string;
  current_node: string;
  completed_nodes: string[];
  node_retries: Record<string, number>;
  context_values: Record<string, unknown>;
  logs: string[];
  node_outcomes: Record<string, Outcome>;
  next_node_id?: string;
}

export interface Outcome {
  status: string;
  preferred_label?: string;
  suggested_next_ids: string[];
  context_updates: Record<string, unknown>;
  notes?: string;
  failure_reason?: string;
}

export type PipelineEvent =
  | { PipelineStarted: { name: string; id: string } }
  | { PipelineCompleted: { duration_ms: number; artifact_count: number } }
  | { PipelineFailed: { error: string; duration_ms: number } }
  | { StageStarted: { name: string; index: number } }
  | { StageCompleted: { name: string; index: number; duration_ms: number; status: string; preferred_label?: string; suggested_next_ids: string[]; usage?: { model: string; input_tokens: number; output_tokens: number } } }
  | { StageFailed: { name: string; index: number; error: string; will_retry: boolean } }
  | { StageRetrying: { name: string; index: number; attempt: number; delay_ms: number } }
  | { ParallelStarted: { branch_count: number } }
  | { ParallelBranchStarted: { branch: string; index: number } }
  | { ParallelBranchCompleted: { branch: string; index: number; duration_ms: number; success: boolean } }
  | { ParallelCompleted: { duration_ms: number; success_count: number; failure_count: number } }
  | { InterviewStarted: { question: string; stage: string } }
  | { InterviewCompleted: { question: string; answer: string; duration_ms: number } }
  | { InterviewTimeout: { question: string; stage: string; duration_ms: number } }
  | { CheckpointSaved: { node_id: string } };

export type ContextSnapshot = Record<string, unknown>;

const API_BASE = "/api";

export async function listPipelines(): Promise<PipelineStatusResponse[]> {
  const res = await fetch(`${API_BASE}/pipelines`);
  if (!res.ok) throw new Error(`List failed: ${res.status}`);
  return res.json() as Promise<PipelineStatusResponse[]>;
}

export async function startPipeline(dotSource: string): Promise<StartPipelineResponse> {
  const res = await fetch(`${API_BASE}/pipelines`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ dot_source: dotSource }),
  });
  if (!res.ok) throw new Error(`Start failed: ${res.status}`);
  return res.json() as Promise<StartPipelineResponse>;
}

export async function getPipelineStatus(id: string): Promise<PipelineStatusResponse> {
  const res = await fetch(`${API_BASE}/pipelines/${id}`);
  if (!res.ok) throw new Error(`Status failed: ${res.status}`);
  return res.json() as Promise<PipelineStatusResponse>;
}

export async function getQuestions(id: string): Promise<ApiQuestion[]> {
  const res = await fetch(`${API_BASE}/pipelines/${id}/questions`);
  if (!res.ok) throw new Error(`Questions failed: ${res.status}`);
  return res.json() as Promise<ApiQuestion[]>;
}

export async function submitAnswer(
  pipelineId: string,
  questionId: string,
  value: string,
  selectedOptionKey?: string,
): Promise<SubmitAnswerResponse> {
  const body: Record<string, string> = { value };
  if (selectedOptionKey !== undefined) {
    body.selected_option_key = selectedOptionKey;
  }
  const res = await fetch(`${API_BASE}/pipelines/${pipelineId}/questions/${questionId}/answer`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`Answer failed: ${res.status}`);
  return res.json() as Promise<SubmitAnswerResponse>;
}

export async function getCheckpoint(id: string): Promise<Checkpoint | null> {
  const res = await fetch(`${API_BASE}/pipelines/${id}/checkpoint`);
  if (!res.ok) throw new Error(`Checkpoint failed: ${res.status}`);
  return res.json() as Promise<Checkpoint | null>;
}

export async function getContext(id: string): Promise<ContextSnapshot> {
  const res = await fetch(`${API_BASE}/pipelines/${id}/context`);
  if (!res.ok) throw new Error(`Context failed: ${res.status}`);
  return res.json() as Promise<ContextSnapshot>;
}

export async function cancelPipeline(id: string): Promise<void> {
  const res = await fetch(`${API_BASE}/pipelines/${id}/cancel`, { method: "POST" });
  if (!res.ok) throw new Error(`Cancel failed: ${res.status}`);
}

export async function getGraph(id: string): Promise<string> {
  const res = await fetch(`${API_BASE}/pipelines/${id}/graph`);
  if (!res.ok) throw new Error(`Graph failed: ${res.status}`);
  return res.text();
}

export function eventsUrl(id: string): string {
  return `${API_BASE}/pipelines/${id}/events`;
}
