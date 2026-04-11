use std::collections::HashMap;

use crate::context::Context;
use crate::error::Result;
use crate::outcome::{Outcome, OutcomeMeta};

pub trait NodeSpec: Send + Sync + Clone {
    fn id(&self) -> &str;
    fn is_terminal(&self) -> bool;
    fn max_visits(&self) -> Option<usize>;
}

pub trait EdgeSpec: Send + Sync + Clone {
    fn target(&self) -> &str;
    fn label(&self) -> Option<&str>;
    fn is_loop_restart(&self) -> bool;
}

pub struct EdgeSelection<G: Graph + ?Sized> {
    pub edge: G::Edge,
    pub reason: &'static str,
}

pub trait Graph: Send + Sync {
    type Node: NodeSpec + Clone;
    type Edge: EdgeSpec + Clone;
    type Meta: OutcomeMeta;

    fn get_node(&self, id: &str) -> Option<Self::Node>;
    fn find_start_node(&self) -> Result<Self::Node>;
    fn outgoing_edges(&self, node_id: &str) -> Vec<Self::Edge>;
    fn select_edge(
        &self,
        node: &Self::Node,
        outcome: &Outcome<Self::Meta>,
        context: &Context,
    ) -> Option<EdgeSelection<Self>>;
    fn check_goal_gates(
        &self,
        outcomes: &HashMap<String, Outcome<Self::Meta>>,
    ) -> std::result::Result<(), String>;
    fn get_retry_target(&self, failed_node_id: &str) -> Option<String>;
}
