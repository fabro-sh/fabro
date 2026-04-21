use std::hash::Hash;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use tokio::sync::{Mutex, OwnedMutexGuard};

#[derive(Debug)]
pub(crate) struct KeyedMutex<K>
where
    K: Eq + Hash + Clone,
{
    mutexes: DashMap<K, std::sync::Arc<Mutex<()>>>,
}

impl<K> Default for KeyedMutex<K>
where
    K: Eq + Hash + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K> KeyedMutex<K>
where
    K: Eq + Hash + Clone,
{
    pub(crate) fn new() -> Self {
        Self {
            mutexes: DashMap::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.mutexes.len()
    }
}

impl<K> KeyedMutex<K>
where
    K: Eq + Hash + Clone,
{
    pub(crate) async fn lock(&self, key: K) -> Guard<'_, K> {
        let mutex = self
            .mutexes
            .entry(key.clone())
            .or_insert_with(|| std::sync::Arc::new(Mutex::new(())))
            .clone();

        Guard {
            keyed_mutex: self,
            key,
            held_lock: Some(mutex.lock_owned().await),
        }
    }
}

pub(crate) struct Guard<'a, K>
where
    K: Eq + Hash + Clone,
{
    keyed_mutex: &'a KeyedMutex<K>,
    key:         K,
    held_lock:   Option<OwnedMutexGuard<()>>,
}

impl<K> Drop for Guard<'_, K>
where
    K: Eq + Hash + Clone,
{
    fn drop(&mut self) {
        self.held_lock.take();

        if let Entry::Occupied(entry) = self.keyed_mutex.mutexes.entry(self.key.clone()) {
            if std::sync::Arc::strong_count(entry.get()) == 1 {
                entry.remove();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use tokio::task::{JoinSet, yield_now};
    use tokio::time::{sleep, timeout};

    use super::KeyedMutex;

    #[tokio::test]
    async fn distinct_keys_do_not_contend() {
        let keyed_mutex = KeyedMutex::new();
        let _first = keyed_mutex.lock("alpha".to_string()).await;

        timeout(
            Duration::from_millis(50),
            keyed_mutex.lock("beta".to_string()),
        )
        .await
        .expect("distinct keys should not block");
    }

    #[tokio::test]
    async fn same_key_serializes_access() {
        let keyed_mutex = Arc::new(KeyedMutex::new());
        let first = keyed_mutex.lock("alpha".to_string()).await;
        let acquired = Arc::new(AtomicBool::new(false));

        let worker_mutex = Arc::clone(&keyed_mutex);
        let worker_acquired = Arc::clone(&acquired);
        let waiter = tokio::spawn(async move {
            let _second = worker_mutex.lock("alpha".to_string()).await;
            worker_acquired.store(true, Ordering::SeqCst);
        });

        sleep(Duration::from_millis(10)).await;
        assert!(
            !acquired.load(Ordering::SeqCst),
            "second waiter should block while first guard is held"
        );

        drop(first);
        timeout(Duration::from_millis(100), waiter)
            .await
            .expect("waiter should proceed after first guard drops")
            .unwrap();
        assert!(acquired.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn drops_unused_entries_after_last_guard_releases() {
        let keyed_mutex = KeyedMutex::new();
        let guard = keyed_mutex.lock("alpha".to_string()).await;

        assert_eq!(keyed_mutex.len(), 1);
        drop(guard);

        assert_eq!(keyed_mutex.len(), 0);
    }

    #[tokio::test]
    async fn stress_same_key_keeps_single_mutex_and_cleans_up() {
        let keyed_mutex = Arc::new(KeyedMutex::new());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let violations = Arc::new(AtomicUsize::new(0));
        let mut tasks = JoinSet::new();

        for _ in 0..16 {
            let keyed_mutex = Arc::clone(&keyed_mutex);
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            let violations = Arc::clone(&violations);
            tasks.spawn(async move {
                for _ in 0..64 {
                    let _guard = keyed_mutex.lock("alpha".to_string()).await;
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(current, Ordering::SeqCst);
                    if current != 1 {
                        violations.fetch_add(1, Ordering::SeqCst);
                    }
                    yield_now().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            });
        }

        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }

        assert_eq!(violations.load(Ordering::SeqCst), 0);
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(keyed_mutex.len(), 0);
    }
}
