use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::Value;

use crate::config::Config;
use crate::engine::scenario::{RequestContext, Scenario};
use crate::logs::RequestLog;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum NamespaceKey {
    Global,
    Bearer(String),
}

impl fmt::Display for NamespaceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Global => write!(f, "Global"),
            Self::Bearer(token) => write!(f, "Bearer: {token}"),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DebugSnapshot {
    pub namespaces: Vec<NamespaceSnapshot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NamespaceSnapshot {
    pub key: String,
    pub scenarios: Vec<ScenarioSnapshot>,
    pub request_logs: Vec<RequestLog>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScenarioSnapshot {
    pub endpoint: String,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub input_contains: Option<String>,
    pub metadata: serde_json::Map<String, Value>,
    pub script_kind: String,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub config: Config,
    inner: Arc<AppStateInner>,
}

#[derive(Debug)]
struct AppStateInner {
    namespaces: Mutex<HashMap<NamespaceKey, NamespaceState>>,
}

#[derive(Debug)]
struct NamespaceState {
    next_response_number: u64,
    scenarios: Vec<Scenario>,
    request_logs: Vec<RequestLog>,
}

impl Default for NamespaceState {
    fn default() -> Self {
        Self {
            next_response_number: 1,
            scenarios: Vec::new(),
            request_logs: Vec::new(),
        }
    }
}

impl AppState {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            inner: Arc::new(AppStateInner {
                namespaces: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub fn next_response_id(&self, namespace: &NamespaceKey) -> u64 {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let namespace_state = namespaces.entry(namespace.clone()).or_default();
        let response_id = namespace_state.next_response_number;
        namespace_state.next_response_number += 1;
        response_id
    }

    pub fn enqueue_scenarios(&self, namespace: &NamespaceKey, mut scenarios: Vec<Scenario>) {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .entry(namespace.clone())
            .or_default()
            .scenarios
            .append(&mut scenarios);
    }

    pub fn take_matching_scenario(
        &self,
        namespace: &NamespaceKey,
        request: &RequestContext,
    ) -> Option<Scenario> {
        let mut namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let scenarios = &mut namespaces.entry(namespace.clone()).or_default().scenarios;
        let position = scenarios
            .iter()
            .position(|scenario| scenario.matches(request))?;
        Some(scenarios.remove(position))
    }

    pub fn log_request(&self, namespace: &NamespaceKey, request: RequestContext) {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .entry(namespace.clone())
            .or_default()
            .request_logs
            .push(RequestLog {
                endpoint: request.endpoint,
                model: request.model,
                stream: request.stream,
                input_text: request.input_text,
                metadata: request.metadata,
            });
    }

    pub fn request_logs(&self, namespace: &NamespaceKey) -> Vec<RequestLog> {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .get(namespace)
            .map(|namespace_state| namespace_state.request_logs.clone())
            .unwrap_or_default()
    }

    pub fn reset(&self, namespace: &NamespaceKey) {
        self.inner
            .namespaces
            .lock()
            .expect("namespaces lock")
            .remove(namespace);
    }

    pub fn debug_snapshot(&self) -> DebugSnapshot {
        let namespaces = self.inner.namespaces.lock().expect("namespaces lock");
        let mut result = Vec::new();
        for (key, ns) in namespaces.iter() {
            result.push(NamespaceSnapshot {
                key: key.to_string(),
                scenarios: ns
                    .scenarios
                    .iter()
                    .map(|s| ScenarioSnapshot {
                        endpoint: s.matcher.endpoint.clone(),
                        model: s.matcher.model.clone(),
                        stream: s.matcher.stream,
                        input_contains: s.matcher.input_contains.clone(),
                        metadata: s.matcher.metadata.clone(),
                        script_kind: s.script.script_kind().to_owned(),
                    })
                    .collect(),
                request_logs: ns.request_logs.clone(),
            });
        }
        DebugSnapshot { namespaces: result }
    }
}
