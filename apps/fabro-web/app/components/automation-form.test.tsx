import { describe, expect, test } from "bun:test";

import {
  EMPTY_AUTOMATION_FORM,
  automationFormValuesFromRun,
  automationToFormValues,
  isFormValid,
  triggersFromFormValues,
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

describe("automation jira trigger form values", () => {
  test("triggersFromFormValues emits a jira trigger with nullable statuses", () => {
    const filled = triggersFromFormValues({
      ...EMPTY_AUTOMATION_FORM,
      manualEnabled:         false,
      jiraEnabled:           true,
      jiraJql:               " project = FAB ",
      jiraInProgressStatus:  " In Progress ",
      jiraReviewStatus:      " In Review ",
    });
    expect(filled).toEqual([{
      id: "jira", type: "jira", enabled: true,
      jql: "project = FAB", in_progress_status: "In Progress", review_status: "In Review",
    }]);

    const blank = triggersFromFormValues({
      ...EMPTY_AUTOMATION_FORM,
      manualEnabled: false,
      jiraEnabled:   true,
      jiraJql:       "project = FAB",
    });
    expect(blank).toHaveLength(1);
    expect((blank[0] as any).in_progress_status).toBeUndefined();
    expect((blank[0] as any).review_status).toBeUndefined();
  });

  test("automationToFormValues reads a jira trigger", () => {
    const values = automationToFormValues({
      id: "a", name: "A", workflow: "w", target: null,
      triggers: [{
        id: "jira", type: "jira", enabled: true,
        jql: "project = FAB", in_progress_status: "Doing",
      }],
    } as any);
    expect(values.jiraEnabled).toBe(true);
    expect(values.jiraJql).toBe("project = FAB");
    expect(values.jiraInProgressStatus).toBe("Doing");
    expect(values.jiraReviewStatus).toBe("");
  });

  test("isFormValid rejects an enabled jira trigger with blank JQL", () => {
    const valid = {
      ...EMPTY_AUTOMATION_FORM,
      id: "a", name: "A", environmentId: "e", targetRepository: "o/r", workflow: "w",
    };
    expect(isFormValid({ ...valid, jiraEnabled: true, jiraJql: "  " })).toBe(false);
    expect(isFormValid({ ...valid, jiraEnabled: true, jiraJql: "project = FAB" })).toBe(true);
    expect(isFormValid({ ...valid, jiraEnabled: false, jiraJql: "" })).toBe(true);
  });
});
