//! TTL + LRU cache, in memory only.
//!
//! Expiring media URLs are never written to disk: they live here for as long
//! as YouTube says they are valid and are re-resolved on demand.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Entry<V> {
    value: V,
    expires_at: Instant,
    /// Monotonic recency stamp; the lowest value is evicted first.
    seq: u64,
}

struct Inner<V> {
    map: HashMap<String, Entry<V>>,
    counter: u64,
}

pub struct TtlCache<V> {
    ttl: Duration,
    max_entries: usize,
    inner: Mutex<Inner<V>>,
}

impl<V: Clone> TtlCache<V> {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries: max_entries.max(1),
            inner: Mutex::new(Inner {
                map: HashMap::new(),
                counter: 0,
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<V> {
        let mut inner = self.inner.lock().ok()?;
        let now = Instant::now();
        let (value, seq) = {
            let entry = inner.map.get(key)?;
            if entry.expires_at <= now {
                inner.map.remove(key);
                return None;
            }
            (entry.value.clone(), entry.seq)
        };
        inner.counter += 1;
        let counter = inner.counter;
        if let Some(entry) = inner.map.get_mut(key) {
            entry.seq = counter;
        }
        let _ = seq;
        Some(value)
    }

    /// Returns the value only when at least `min_remaining` of TTL is left.
    pub fn get_min_ttl(&self, key: &str, min_remaining: Duration) -> Option<V> {
        let mut inner = self.inner.lock().ok()?;
        let now = Instant::now();
        let entry = inner.map.get(key)?;
        if entry.expires_at <= now || entry.expires_at.duration_since(now) < min_remaining {
            inner.map.remove(key);
            return None;
        }
        Some(entry.value.clone())
    }

    pub fn set(&self, key: &str, value: V, ttl: Option<Duration>) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.counter += 1;
        let counter = inner.counter;
        inner.map.insert(
            key.to_string(),
            Entry {
                value,
                expires_at: Instant::now() + ttl.unwrap_or(self.ttl),
                seq: counter,
            },
        );
        while inner.map.len() > self.max_entries {
            let Some(oldest) = inner
                .map
                .iter()
                .min_by_key(|(_, e)| e.seq)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            inner.map.remove(&oldest);
        }
    }

    pub fn delete(&self, key: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.map.remove(key);
        }
    }
}
