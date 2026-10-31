# Sandbox admission and worker ownership

This is the application boundary for the next sandbox-driver integration. The
current runtime and saved records remain supported. The authoritative driver
contract is [shared-contract.md](https://github.com/lithoscomputer/sandbox-driver/blob/8f7358a71b7acc51fc44b79f514e9f2a029db8c2/docs/shared-contract.md)
and its [target API](https://github.com/lithoscomputer/sandbox-driver/blob/8f7358a71b7acc51fc44b79f514e9f2a029db8c2/docs/next-api.md).
Declarations alone are not provider integration evidence.

## Current boundaries

| Boundary | Current owner and representation |
| --- | --- |
| Provider permission/launch settings | `ServerSandboxProvidersSettings`, keyed by provider kind; bundled providers have enablement, plugins have launch settings. This is not yet the next API's named provider-entry catalog. |
| Environment templates | `fabro-environment` SQLite store; REST uses `EnvironmentSettings`. `run_manifest` resolves the selected template and workflow overrides. |
| Admission | `run_compiler` and `server/petri_runs` admit graphs; `operations/create` persists `RunSpec.settings` and `PetriAdmission` graph blob references. |
| Worker input | `runner::execute` fetches `/runs/{id}/state`; `petri_worker` loads the admitted graphs and reads the saved environment. Start/resume do not resolve that environment from the catalog again. |
| Connection input | `worker_launch_spec` captures Daytona URL/organization and Docker host address from the same server lookup used by server-side access. `SandboxConnectionEnv` passes those values explicitly at spawn, including absence. Docker endpoint/TLS/context still use the existing process allowlist and driver client behavior. |
| Credentials | The worker opens a vault snapshot at the server-supplied storage root. Daytona keys are vault-only here. Workload `{{ secrets.NAME }}` references remain references in admitted settings; Petri resolves them through `VaultSecrets`. |
| Server sandbox access | `sandbox_access` connects by recorded provider kind and checks the `petri.run` ownership label. Host uses the existing registry observer. |

The launch snapshot fixes server/worker configuration disagreement but is **not**
a durable admission binding. A restart still uses the current server connection
configuration. The worker also still loads its LLM catalog separately; this
preparation does not change that unrelated path. `FABRO_CONFIG` remains for those
existing consumers, not as the future sandbox catalog transport.

## Provider entries and workload specifications

At cutover, reuse driver-owned `contract::ProviderEntry`, `Environment`, and
`SandboxSpec`; do not create equivalent Fabro DTOs. Keep these meanings separate:

- A server provider-entry name selects a connection and plugin/trust inputs.
  `ProviderSchemas.config_schema` validates only that entry's `config`.
- An environment selects a provider entry and a portable workload template.
  `spec_schema` validates only the workload's `provider_config`. Common workload
  fields, capabilities, and services have their own contract validation.
- Fabro owns environment storage, API authorization, and run lifecycle policy.
  Docker/Daytona recipes, helper images, networking, and nested runners belong
  to the driver. Neither a connection form nor environment form should expose
  those recipes as Fabro policy.

The existing `[server.sandbox.providers.<kind>]` launch shape and environment
REST bodies continue to decode unchanged. In particular, the existing `plugin`
object is not the next contract's `plugin` name. A versioned boundary/explicit
legacy adapter is required before accepting the new shape; do not reinterpret
existing configuration under the same unversioned meaning. Do not activate new
forms or validate workload fields with a connection schema during preparation.

## Durable admission and credential delivery

The future admission record must contain the resolved workload, provider-entry
binding (including frozen non-secret launch inputs and secret references), and
effective application policy. Workers receive that value through the existing
run-state/blob delivery path. They construct their own catalog instance from
that value, never by reopening settings or rereading an edited environment.
Server access, pruning, and recovery must use the same saved binding.

Resolve provider and service credential names through the same server vault as
the workload secret resolver. Persist names, never resolved values. The
application adapter must reject or externalize inline credentials in entry
config/args/env before making them durable; redacted `Debug` alone does not make
serialization safe. Avoid logging resolved launch payloads. Rotation retains
the existing consumption-time vault semantics rather than pinning credential
bytes in a run.

After creation persist the driver-issued `SandboxRef { provider, namespace, id }`.
Its provider is an entry name; an environment rename or deletion cannot break
access. Namespace comparison precedes resource operations and never replaces
Fabro's ownership checks. For legacy records keep the legacy reader; never
derive an old namespace from today's URL, credentials, environment, or kind.

**Unresolved dependency:** the driver catalog must expose an admitted binding
that can be rehydrated independently of mutable `EnvironmentSource` and retain
its namespace checks; Petri must persist/use that binding and the issued ref in
its lease/recovery path. The merged type declarations do not implement this.
Only then can Fabro extend its OpenAPI/persistence boundary, replace its
factories, and test start/resume/prune against edited or deleted entries. The
existing plugin-registration proposal that rereads server settings in workers
must not become this path.

## Lifecycle compatibility

`EnvironmentLifecycleSettings::run_policy()` extracts only the two
application decisions. It is used by run deletion and the worker-exit backstop;
the persisted/API location remains `run.environment.lifecycle` for compatibility.

| preserve | stop_on_terminal | Existing application policy |
| --- | --- | --- |
| false | true | Default: prune on run deletion; stop on terminal worker-failure cleanup. |
| true | true | Skip sandbox prune on deletion; still request terminal stop. |
| false | false | Prune on deletion; skip the terminal-stop backstop. |
| true | false | Skip both sandbox prune on deletion and the terminal-stop backstop. |

Normal scope release remains Petri's. Fabro passes `Retention::Always` for all
four combinations: finishing a run is not a request to destroy its workspace.
The supervisor applies `stop_on_terminal` only after a failed worker exits and
the run has settled terminal, not while it is scheduled to resume. Local
process cleanup remains the Host guardian's responsibility. Preservation does
not override that cleanup or promise that a managed Host directory survives
run-directory deletion.

When adding `[run.lifecycle]` and provider-entry defaults, retain false/true
defaults and normalize legacy policy fields per input layer before merging.
Within one layer the new field should win; higher-precedence workflow/CLI input
must still win over lower-precedence settings regardless of spelling. Existing
`preserve_sandbox = false` remains an explicit override. Save the effective
policy once; do not consult new provider defaults for an old run. This change
does not activate the new input location or change normal-release behavior.

`auto_stop` is a provider idle timer, not either application decision. Keep it
outside the future run policy. Timer translation waits for provider evidence,
including disabled timers and Daytona's distinct zero-means-delete behavior.

## Host access through the owning worker

The next Host is a separately installed plugin with `owner_process` access and
process durability. Fabro must not link/bundle it, independently attach from
the server, or recreate a handle just because a saved directory exists.
Missing Host must fail only work selecting it, including Host-backed dry runs;
other configured providers remain usable.

The existing control path already has authenticated run-scoped WebSockets,
request IDs, acknowledgements, deadlines, and a server supervisor that knows
the worker. Reuse that ownership route; do not introduce a daemon. It currently
carries run controls, **not** sandbox RPC or file/terminal streams.

For live access, the server must authorize the run and dispatch to the current
worker attempt. That worker must use its existing Petri-owned sandbox handle
and verify the lease/ref and run ownership. Requests and replies need the
worker-attempt identity as well as the run/request ID so a replacement worker
cannot answer for a dead owner. Do not send sandbox mutations through the
existing reconnect replay mechanism: a dropped acknowledgement means unknown
outcome, not permission to retry. File/exec/terminal streams require bounded
stream transport and cancellation; the control acknowledgement channel is not
a bulk-data transport.

| Situation | Compatible product behavior at cutover |
| --- | --- |
| Live owning worker | Proxy supported files/exec/terminal operations to that worker; do not launch a server Host instance. |
| History | Display provider/ref, recorded working directory, events, and artifacts from the run record, even if Host is unavailable. Recorded status is history, not a fresh health probe. |
| Owner exited or channel lost | Refuse live access with an owner-unavailable/unknown result. Do not claim workload termination from process loss alone. |
| Retained files | Offer recorded artifacts or an explicitly authorized retained-file view without constructing a sandbox. Path existence grants neither ownership nor permission to execute. Managed versus designated workspace deletion rights must remain explicit. |
| Resume | Pause/unpause can use the same live owner. After owner loss ordinary attach/resume is not promised. An explicit recovery into a new sandbox over a retained designated workspace requires prior-owner safety to be established; it is not automatic resurrection. |

**Unresolved dependencies:** Petri must expose the current owner handle and
attempt identity to the worker access adapter; the control path needs
non-replayed sandbox requests plus bounded stream forwarding; Host must prove
fencing/owner cleanup and stale-handle behavior. Until those are integrated and
tested, retain the current observer, directory-access behavior, and existing
resume path. Removing the observer ahead of this route would break the sandbox
tab, Run Files, terminal, `fabro cp`, and Ask Fabro.
