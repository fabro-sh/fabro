import useSWRMutation from "swr/mutation";
import { useSWRConfig } from "swr";
import type {
  PreviewUrlResponse,
  RunStatusResponse,
  SubmitAnswerRequest,
} from "@qltysh/fabro-api-client";

import { apiJsonMutation } from "./api-client";
import { queryKeys } from "./query-keys";
import type { LifecycleAction, LifecycleActionError } from "./run-actions";
import {
  archiveRun,
  cancelRun,
  isLifecycleActionError,
  unarchiveRun,
} from "./run-actions";

export type PreviewRunArg = {
  port: number;
  expires_in_secs: number;
};

export type PreviewMutationResult = {
  intent: "preview";
  url: string;
};

export type LifecycleMutationResult =
  | {
      intent: LifecycleAction;
      ok: true;
      run: RunStatusResponse;
    }
  | {
      intent: LifecycleAction;
      ok: false;
      error: LifecycleActionError | null;
    };

export function usePreviewRun(id: string | undefined) {
  return useSWRMutation(
    id ? queryKeys.runs.preview(id) : null,
    async (key: string, { arg }: { arg: PreviewRunArg }): Promise<PreviewMutationResult> => {
      const result = await apiJsonMutation<PreviewUrlResponse, PreviewRunArg>(key, { arg });
      return { intent: "preview", url: result.url };
    },
  );
}

export function useCancelRun(id: string | undefined) {
  return useLifecycleMutation(id, "cancel", cancelRun);
}

export function useArchiveRun(id: string | undefined) {
  return useLifecycleMutation(id, "archive", archiveRun);
}

export function useUnarchiveRun(id: string | undefined) {
  return useLifecycleMutation(id, "unarchive", unarchiveRun);
}

function useLifecycleMutation(
  id: string | undefined,
  intent: LifecycleAction,
  action: (id: string) => Promise<RunStatusResponse>,
) {
  const { mutate } = useSWRConfig();
  const key = id ? queryKeys.runs[intent](id) : null;
  return useSWRMutation(
    key,
    async (): Promise<LifecycleMutationResult> => {
      if (!id) {
        return { intent, ok: false, error: null };
      }
      try {
        return { intent, ok: true, run: await action(id) };
      } catch (error) {
        return {
          intent,
          ok: false,
          error: isLifecycleActionError(error) ? error : null,
        };
      }
    },
    {
      onSuccess: (result) => {
        if (!id || !result.ok) return;
        void mutate(queryKeys.runs.detail(id));
        void mutate(queryKeys.boards.runs());
        void mutate(queryKeys.runs.billing(id));
      },
    },
  );
}

export type SubmitInterviewAnswerArg = SubmitAnswerRequest & { questionId: string };

export function useSubmitInterviewAnswer(runId: string | undefined) {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    runId ? `interview-answer:${runId}` : null,
    async (_key: string, { arg }: { arg: SubmitInterviewAnswerArg }) => {
      if (!runId) throw new Error("runId is required");
      const { questionId, ...body } = arg;
      const path = `/api/v1/runs/${encodeURIComponent(runId)}/questions/${encodeURIComponent(questionId)}/answer`;
      await apiJsonMutation<void, SubmitAnswerRequest>(path, { arg: body });
    },
    {
      onSuccess: () => {
        if (!runId) return;
        void mutate(queryKeys.runs.questions(runId, 25, 0));
        void mutate(queryKeys.runs.detail(runId));
      },
    },
  );
}

export interface SteerRunArg {
  text: string;
  interrupt: boolean;
}

export function useSteerRun(runId: string | undefined) {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    runId ? `steer:${runId}` : null,
    async (_key: string, { arg }: { arg: SteerRunArg }) => {
      if (!runId) throw new Error("runId is required");
      await apiJsonMutation<void, SteerRunArg>(
        queryKeys.runs.steer(runId),
        { arg },
      );
    },
    {
      onSuccess: () => {
        if (!runId) return;
        void mutate(queryKeys.runs.detail(runId));
      },
    },
  );
}

export function useToggleDemoMode() {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    queryKeys.demo.toggle(),
    async (key: string, { arg }: { arg: { enabled: boolean } }) => {
      await apiJsonMutation<void, { enabled: boolean }>(key, { arg });
    },
    {
      onSuccess: () => {
        void mutate(queryKeys.auth.me());
      },
    },
  );
}

export function useLoginDevToken() {
  return useSWRMutation(
    "/auth/login/dev-token",
    async (key: string, { arg }: { arg: { token: string } }) => {
      const response = await fetch(key, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(arg),
      });
      if (!response.ok) {
        throw new Error(response.statusText || `HTTP ${response.status}`);
      }
      return response.json() as Promise<{ ok: boolean }>;
    },
  );
}