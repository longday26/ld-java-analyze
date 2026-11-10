//! Atomic publication of immutable index snapshots.

use crate::index::{IndexSnapshot, QueryEngine};
use arc_swap::ArcSwap;
use std::sync::Arc;

/// Publishes immutable snapshots and hands out read-only query engines.
///
/// A query engine always owns one `Arc<IndexSnapshot>`; publishing a newer
/// snapshot never mutates or invalidates an in-flight query.
#[derive(Debug, Default)]
pub struct SnapshotStore {
    snapshots: ArcSwap<IndexSnapshot>,
}

impl SnapshotStore {
    pub fn new(initial: Option<Arc<IndexSnapshot>>) -> Self {
        let snapshots = match initial {
            Some(snapshot) => ArcSwap::from(snapshot),
            None => ArcSwap::from(Arc::new(IndexSnapshot::default())),
        };
        Self { snapshots }
    }

    pub fn publish(&self, snapshot: Arc<IndexSnapshot>) {
        self.snapshots.store(snapshot);
    }

    pub fn current(&self) -> QueryEngine {
        QueryEngine::new(self.snapshots.load_full())
    }

    pub fn generation(&self) -> u64 {
        self.snapshots.load().generation()
    }
}
