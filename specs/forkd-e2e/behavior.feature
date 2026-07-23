Feature: forkd end-to-end capability
  As a cal orchestrator
  I want to create isolated microVM sandboxes, execute commands inside them, and delete them
  So that agent tasks run in reproducible, ephemeral environments without polluting the host

  @cal-forkd-e2e
  @task-create-sandbox
  Scenario: creating a sandbox from a snapshot tag returns a server-assigned id
    Given the forkd service is reachable
    And a snapshot tag "zen-gate-base" exists in the snapshot registry
    When I send a create-sandbox request with snapshot_tag "zen-gate-base"
    Then the response status is 201
    And the response body contains a non-empty "id" field
    And the returned id matches the pattern "[a-f0-9-]{36}"

  @cal-forkd-e2e
  @task-exec-command
  Scenario: exec runs an argv command in the microVM and returns its exit code and output
    Given a sandbox exists with id stored as "sandbox_id"
    When I send an exec request to sandbox "sandbox_id" with argv ["echo", "hello forkd"]
    Then the response status is 200
    And the response body contains exit_code 0
    And the response body stdout contains "hello forkd"
    And the response body stderr is empty

  @cal-forkd-e2e
  @task-exec-working-dir-and-env
  Scenario: working_dir and env are honored by folding into the argv execution context
    Given a sandbox exists with id stored as "sandbox_id"
    And the sandbox filesystem contains a directory "/workspace/project"
    When I send an exec request to sandbox "sandbox_id" with:
      | field       | value                        |
      | argv        | ["pwd"]                      |
      | working_dir | /workspace/project           |
    Then the response stdout contains "/workspace/project"
    When I send an exec request to sandbox "sandbox_id" with:
      | field       | value                                        |
      | argv        | ["sh", "-c", "echo $FORKD_TEST_VAR"]         |
      | env         | {"FORKD_TEST_VAR": "sentinel-42"}            |
    Then the response stdout contains "sentinel-42"
    And the response exit_code is 0

  @cal-forkd-e2e
  @task-delete-idempotent
  Scenario: delete is idempotent
    Given a sandbox exists with id stored as "sandbox_id"
    When I send a delete request for sandbox "sandbox_id"
    Then the response status is 204
    When I send a delete request for sandbox "sandbox_id" again
    Then the response status is 204 or 404
    And no error body indicating a server fault is returned

  @cal-forkd-e2e
  @task-real-workflow
  Scenario: a real workflow executing git clone and running a command completes inside the microVM
    Given the forkd service is reachable
    And a snapshot tag "zen-gate-base" exists in the snapshot registry
    When I create a sandbox from snapshot_tag "zen-gate-base" and store its id as "wf_sandbox_id"
    And I exec in sandbox "wf_sandbox_id" with argv ["git", "clone", "--depth", "1", "https://github.com/nicowillis/hello-world.git", "/tmp/repo"]
    Then the exec response exit_code is 0
    And the sandbox filesystem path "/tmp/repo" exists
    When I exec in sandbox "wf_sandbox_id" with argv ["sh", "-c", "cd /tmp/repo && ls -1"] and working_dir "/"
    Then the exec response exit_code is 0
    And the exec response stdout is non-empty
    When I delete sandbox "wf_sandbox_id"
    Then the delete response status is 204