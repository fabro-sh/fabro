import { describe, expect, test } from "bun:test";

import {
  EMPTY_AUTOMATION_FORM,
  automationFormValuesFromRun,
  automationToFormValues,
  isFormValid,
  workflowSourceFromFormValues,
} from "./automation-form";

describe("automation workflow source form values", () => {
  test("the default and create-from-run forms inherit the target checkout", () => {
    expect(workflowSourceFromFormValues(EMPTY_AUTOMATION_FORM)).toBeUndefined();

    const values = automationFormValuesFromRun({
      title: "Release",
      workflow: { name: "Release", graph_name: "release", slug: "release" },
      repository: {
        name:       "fabro-sh/fabro",
        origin_url: "https://github.com/fabro-sh/fabro.git",
      },
      sandbox: null,
    } as any);
    expect(values.usesRemoteWorkflow).toBe(false);
    expect(workflowSourceFromFormValues(values)).toBeUndefined();
  });

  test("create-from-run reads the repository from origins on the configured GitHub host", () => {
    const fromOrigin = (originUrl: string, githubWebUrl?: string) =>
      automationFormValuesFromRun(
        {
          title: "Release",
          workflow: { name: "Release", graph_name: "release", slug: "release" },
          repository: { name: "", origin_url: originUrl },
          sandbox: null,
        } as any,
        null,
        null,
        undefined,
        githubWebUrl,
      ).targetRepository;

    expect(fromOrigin("https://github.com/acme/widgets.git")).toBe("acme/widgets");
    expect(fromOrigin("git@github.com:acme/widgets.git")).toBe("acme/widgets");
    expect(fromOrigin("https://ghe.example.com/acme/widgets.git")).toBe("");

    const ghes = "https://ghe.example.com";
    expect(fromOrigin("https://ghe.example.com/acme/widgets.git", ghes)).toBe("acme/widgets");
    expect(fromOrigin("git@ghe.example.com:acme/widgets.git", ghes)).toBe("acme/widgets");
    expect(fromOrigin("https://github.com/acme/widgets.git", ghes)).toBe("");
  });

  test("branch, tag, and SHA selectors serialize with target precedence", () => {
    const base = {
      ...EMPTY_AUTOMATION_FORM,
      usesRemoteWorkflow:         true,
      workflowSourceRepository: " fabro-sh/workflows ",
      workflowSourceBranch:     " main ",
    };

    expect(workflowSourceFromFormValues(base)).toEqual({
      repo: "fabro-sh/workflows", branch: "main",
    });
    expect(workflowSourceFromFormValues({
      ...base,
      workflowSourceTag: " v1.2.3 ",
    })).toEqual({ repo: "fabro-sh/workflows", branch: "main", tag: "v1.2.3" });
    expect(workflowSourceFromFormValues({
      ...base,
      workflowSourceTag: " v1.2.3 ",
      workflowSourceSha: "ABCDEF0123456789ABCDEF0123456789ABCDEF01",
    })).toEqual({
      repo: "fabro-sh/workflows",
      branch: "main",
      tag: "v1.2.3",
      sha: "abcdef0123456789abcdef0123456789abcdef01",
    });
  });

  test("remote workflow fields are required and SHAs need 40 hex characters", () => {
    const validBase = {
      ...EMPTY_AUTOMATION_FORM,
      id:                       "nightly",
      name:                     "Nightly",
      environmentId:            "daytona-smoke",
      targetRepository:         "fabro-sh/app",
      targetBranch:             "main",
      workflow:                 "release",
      usesRemoteWorkflow:         true,
      workflowSourceRepository: "fabro-sh/workflows",
      workflowSourceBranch:     "main",
      workflowSourceSha:        "0123456789abcdef0123456789abcdef01234567",
    };

    expect(isFormValid(validBase)).toBe(true);
    expect(isFormValid({ ...validBase, workflowSourceRepository: "" })).toBe(false);
    expect(isFormValid({ ...validBase, workflowSourceBranch: "" })).toBe(false);
    expect(isFormValid({ ...validBase, workflowSourceSha: "short" })).toBe(false);
  });

  test("editing preserves an explicit source even when it equals the target", () => {
    const values = automationToFormValues({
      id:          "nightly",
      revision:    "revision",
      name:        "Nightly",
      description: null,
      target:      { kind: "git", repo: "fabro-sh/fabro", branch: "main" },
      workflow:    "release",
      workflow_source: { repo: "fabro-sh/fabro", branch: "main" },
      triggers:    [],
    });

    expect(values.usesRemoteWorkflow).toBe(true);
    expect(values.workflowSourceRepository).toBe("fabro-sh/fabro");
    expect(values.workflowSourceBranch).toBe("main");
    expect(values.workflowSourceTag).toBe("");
    expect(values.workflowSourceSha).toBe("");
    expect(workflowSourceFromFormValues({
      ...values,
      usesRemoteWorkflow: false,
    })).toBeUndefined();
  });
});
