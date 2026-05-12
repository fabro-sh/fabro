# Standardize Server-Sent Events (SSE) for Real-Time Configuration and Runtime State Updates: Sse Implementations Utilize

Status: proposed
Date: 2024-01-15
Deciders: Detection Pipeline (automated)

## Context

- The application requires real-time updates of runtime execution state and configuration changes to be pushed to client interfaces without polling
- Multiple route handlers (run-stages.tsx, run-files.tsx) need to stream execution progress and file processing status to web clients
- A centralized SSE implementation pattern (sse.ts) has emerged to handle persistent connections and event streaming across different runtime contexts
- The pattern provides a consistent mechanism for broadcasting configuration changes and environment state updates during execution workflows

## Problem Statement

Applications need a reliable, standardized mechanism to push real-time runtime state, configuration updates, and execution progress to clients without the overhead and latency of polling-based approaches, while maintaining consistent event handling across multiple execution contexts.

## Decision

1. MUST: SSE implementations MUST utilize a centralized library module (e.g., sse.ts) to ensure consistent event formatting and connection management

## Policy Block

- MUST SSE implementations MUST utilize a centralized library module (e.g., sse.ts) to ensure consistent event formatting and connection management

In scope:
- Web route handlers that expose runtime execution progress (run-stages, run-files, etc.)
- Configuration management endpoints that broadcast environment or setting changes
- Real-time monitoring interfaces for execution workflows
- Client-facing APIs that require push-based state synchronization

Out of scope:
- Bidirectional communication patterns requiring client-to-server messaging (use WebSockets instead)
- Binary data streaming or large file transfers
- Internal service-to-service communication (use message queues or gRPC)
- Static configuration loading at application startup

## Rationale

- Pattern detected across 3 files with 91.07% confidence indicates a deliberate architectural choice for real-time state communication
- SSE provides a lightweight, HTTP-based protocol that works through firewalls and proxies without requiring WebSocket infrastructure
- Centralized SSE library (sse.ts) ensures consistent event formatting, error handling, and connection lifecycle management across multiple route handlers
- The pattern aligns with unidirectional data flow from server to client, which matches the use case of broadcasting runtime state and configuration updates

## Consequences

Positive:
- Clients receive real-time updates without polling overhead, reducing server load and improving responsiveness
- Consistent SSE implementation across routes reduces code duplication and simplifies maintenance
- HTTP-based protocol ensures compatibility with existing infrastructure (load balancers, proxies, CDNs)
- Automatic reconnection support in browsers provides resilience against transient network failures

Negative:
- SSE connections are unidirectional, requiring separate HTTP requests for client-to-server communication
- Long-lived connections may require infrastructure tuning (timeouts, connection limits) to support many concurrent clients
- Browser connection limits (typically 6 per domain) may constrain applications with multiple simultaneous SSE streams
- Debugging and monitoring SSE connections can be more complex than traditional request-response patterns

## Alternatives

- Use polling-based approach with periodic HTTP requests to fetch runtime state updates (rejected)
  Rejected because: Polling introduces latency, increases server load with redundant requests, and provides poor user experience for real-time execution monitoring
  When valid: Acceptable for low-frequency updates (>30 seconds) or when SSE infrastructure is unavailable
- Implement WebSocket-based bidirectional communication for all real-time features (rejected)
  Rejected because: WebSockets add complexity and infrastructure requirements when only unidirectional server-to-client communication is needed for configuration and state updates
  When valid: Appropriate when bidirectional real-time communication is required (e.g., collaborative editing, chat)
- Use GraphQL subscriptions for real-time data updates (rejected)
  Rejected because: Adds GraphQL infrastructure overhead and complexity when simple event streaming suffices for runtime state broadcasting
  When valid: Valid when already using GraphQL extensively and need subscription-based data synchronization

## Risks

- SSE connections may be terminated by intermediate proxies or load balancers with aggressive timeout policies
  Mitigation: Implement periodic heartbeat messages and configure infrastructure timeouts appropriately (e.g., 60+ seconds). Document required infrastructure settings.
  Owner: Engineering team and DevOps
- Memory leaks or resource exhaustion if SSE connections are not properly cleaned up on client disconnect
  Mitigation: Implement robust connection lifecycle management with cleanup handlers. Add monitoring for open connection counts and memory usage.
  Owner: Engineering team
- Browser connection limits may prevent multiple SSE streams from functioning simultaneously
  Mitigation: Multiplex multiple event types over a single SSE connection where possible. Document connection usage and provide guidance on connection management.
  Owner: Engineering team

## Implementation Notes

- Create a centralized SSE utility module (e.g., lib/sse.ts) that provides connection setup, event formatting, and cleanup helpers
- Ensure route handlers set appropriate headers: Content-Type: text/event-stream, Cache-Control: no-cache, Connection: keep-alive
- Implement structured event payloads with consistent format (e.g., {type: string, data: object, timestamp: number})
- Add connection lifecycle logging and metrics to monitor SSE health and detect connection leaks
- Document SSE usage patterns and provide examples for common scenarios (execution progress, configuration updates)
- Consider implementing event replay or catch-up mechanisms for clients that reconnect after disconnection

## Continuation Context


Verify commands:
- grep -r "text/event-stream" apps/fabro-web/app/routes/ | wc -l
- grep -r "import.*sse" apps/fabro-web/app/routes/ | grep -v node_modules
- find apps/fabro-web/app/lib -name "sse.ts" -o -name "sse.js"

Accept when:
- All route handlers exposing runtime state use SSE with text/event-stream Content-Type header
- A centralized SSE utility module exists and is imported by route handlers requiring real-time updates
- SSE connections implement proper cleanup on client disconnect or execution completion

## Enforcement

- Verified by: Code review checklist verifying SSE usage for new real-time endpoints
- Verified by: Automated linting rules to detect missing Content-Type headers on streaming routes
- Verified by: Integration tests validating SSE connection lifecycle and event delivery
- Violation handling: Pull requests introducing polling-based approaches for real-time updates must justify why SSE is not suitable
- Violation handling: Route handlers with SSE connections missing proper cleanup must be fixed before merge
- Violation handling: Violations detected in code review are flagged and require revision
- Exception process: Document technical justification for alternative approach (e.g., WebSocket requirement, infrastructure constraints)
- Exception process: Obtain approval from technical lead or architect
- Exception process: Add architectural decision comment in code explaining exception rationale