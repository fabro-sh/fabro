CREATE TABLE automation_jira_triggers (
    automation_id TEXT NOT NULL,
    id TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    jql TEXT NOT NULL,
    in_progress_status TEXT,
    review_status TEXT,
    PRIMARY KEY (automation_id, id),
    FOREIGN KEY (automation_id) REFERENCES automations(id) ON DELETE CASCADE,
    CHECK (length(id) BETWEEN 1 AND 63),
    CHECK (substr(id, 1, 1) GLOB '[a-z0-9]'),
    CHECK (id NOT GLOB '*[^a-z0-9_-]*'),
    CHECK (enabled IN (0, 1)),
    CHECK (length(trim(jql)) > 0)
);

-- No foreign key on automation_id: claims outlive the automation.
CREATE TABLE jira_issue_runs (
    automation_id TEXT NOT NULL,
    issue_key TEXT NOT NULL,
    run_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    run_created_at TEXT,
    PRIMARY KEY (automation_id, issue_key),
    CHECK (length(trim(issue_key)) > 0)
);

CREATE INDEX jira_issue_runs_run_id ON jira_issue_runs(run_id);
