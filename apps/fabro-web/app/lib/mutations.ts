import useSWRMutation from "swr/mutation";
import { useSWRConfig, type ScopedMutator } from "swr";
import type {
  PreviewUrlResponse,
  Run,
  SteerRunRequest,
  SubmitAnswerRequest,
  UpdateRunRequest,
} from "@qltysh/fabro-api-client";

import {
  apiData,
  authApi,
  humanInTheLoopApi,
  runsApi,
} from "./api-client";
import { mutateRunListCaches } from "./board-cache";
import { queryKeys } from "./query-keys";
import type { LifecycleAction, LifecycleActionError } from "./run-actions";
import {
  approveRun,
  archiveRun,
  cancelRun,
  denyRun,
  isLifecycleActionError,
  retryRun,
  unarchiveRun,
} from "./run-actions";

export type PreviewRunArg = {
  port: number;
  expires_in_secs: number;
  signed?: boolean;
};

export type PreviewMutationResult = {
  intent: "preview";
  url: string;
};

export type LifecycleMutationResult =
  | {
      intent: LifecycleAction;
      ok: true;
      run: Run;
    }
  | {
      intent: LifecycleAction;
      ok: false;
      error: LifecycleActionError | null;
    };

export function usePreviewRun(id: string | undefined) {
  return useSWRMutation(
    id ? queryKeys.runs.preview(id) : null,
    async (_key, { arg }: { arg: PreviewRunArg }): Promise<PreviewMutationResult> => {
      const result = await apiData<PreviewUrlResponse>(() =>
        humanInTheLoopApi.generatePreviewUrl(id!, arg),
      );
      return { intent: "preview", url: result.url };
    },
  );
}

export function useCancelRun(id: string | undefined) {
  return useLifecycleMutation(id, "cancel", cancelRun);
}

export function useApproveRun(id: string | undefined) {
  return useLifecycleMutation(id, "approve", approveRun);
}

export function useDenyRun(id: string | undefined) {
  return useLifecycleMutation(id, "deny", denyRun);
}

export function useArchiveRun(id: string | undefined) {
  return useLifecycleMutation(id, "archive", archiveRun);
}

export function useUnarchiveRun(id: string | undefined) {
  return useLifecycleMutation(id, "unarchive", unarchiveRun);
}

export function useRetryRun(id: string | undefined) {
  return useLifecycleMutation(id, "retry", retryRun, (run, mutate) => {
    void mutate(queryKeys.runs.detail(run.id), run, { revalidate: false });
    if (run.parent_id) {
      mutateRunListCaches(mutate);
    }
  });
}

function useLifecycleMutation(
  id: string | undefined,
  intent: LifecycleAction,
  action: (id: string) => Promise<Run>,
  onSuccessExtra?: (run: Run, mutate: ScopedMutator) => void,
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
        if (intent !== "retry") {
          // Retry doesn't mutate the source run, so skip invalidating its detail/billing keys.
          void mutate(queryKeys.runs.detail(id));
          void mutate(queryKeys.runs.billing(id));
        }
        mutateRunListCaches(mutate);
        onSuccessExtra?.(result.run, mutate);
      },
    },
  );
}

export function useUpdateRunTitle(id: string | undefined) {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    id ? queryKeys.runs.updateTitle(id) : null,
    async (_key, { arg }: { arg: UpdateRunRequest }): Promise<Run> => {
      if (!id) throw new Error("id is required");
      return apiData(() => runsApi.updateRun(id, arg));
    },
    {
      onSuccess: (run) => {
        if (!id) return;
        void mutate(queryKeys.runs.detail(id), run, { revalidate: false });
        mutateRunListCaches(mutate);
      },
    },
  );
}

export type SubmitInterviewAnswerArg = {
  questionId: string;
  answer: SubmitAnswerRequest;
};

export function useSubmitInterviewAnswer(runId: string | undefined) {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    runId ? `interview-answer:${runId}` : null,
    async (_key: string, { arg }: { arg: SubmitInterviewAnswerArg }) => {
      if (!runId) throw new Error("runId is required");
      await apiData(() =>
        humanInTheLoopApi.submitRunAnswer(runId, arg.questionId, arg.answer),
      );
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

export function useInterruptRun(runId: string | undefined) {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    runId ? `interrupt-run:${runId}` : null,
    async (_key: string) => {
      if (!runId) throw new Error("runId is required");
      await apiData(() => humanInTheLoopApi.interruptRun(runId));
    },
    {
      onSuccess: () => {
        if (!runId) return;
        void mutate(queryKeys.runs.detail(runId));
      },
    },
  );
}

export function useSteerRun(runId: string | undefined) {
  const { mutate } = useSWRConfig();
  return useSWRMutation(
    runId ? `steer-run:${runId}` : null,
    async (_key: string, { arg }: { arg: SteerRunRequest }) => {
      if (!runId) throw new Error("runId is required");
      await apiData(() => humanInTheLoopApi.steerRun(runId, arg));
    },
    {
      onSuccess: () => {
        if (!runId) return;
        void mutate(queryKeys.runs.detail(runId));
      },
    },
  );
}

export function useLoginDevToken() {
  return useSWRMutation(
    queryKeys.auth.loginDevToken(),
    async (_key, { arg }: { arg: { token: string } }) => {
      return apiData(() => authApi.loginDevToken(arg), {
        redirectOnUnauthorized: false,
      });
    },
  );
}
