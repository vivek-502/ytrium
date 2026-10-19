//! Provider-backed recommendations.
//!
//! Expands "seed" artists into similar material through the provider's search,
//! then orders results deterministically (stable per seed set) so the UI does
//! not flicker between requests. Ported from `providers/registry.ts`.

use std::collections::HashSet;
use std::sync::OnceLock;

use super::youtube;
use super::Track;
use crate::cache::TtlCache;
use crate::config;
use crate::error::ApiResult;

static RECOMMENDATION_CACHE: OnceLock<TtlCache<Vec<Track>>> = OnceLock::new();

fn recommendation_cache() -> &'static TtlCache<Vec<Track>> {
    RECOMMENDATION_CACHE
        .get_or_init(|| TtlCache::new(config::RECOMMENDATION_TTL, config::CACHE_MAX_ENTRIES))
}

/// Deterministic tiny string hash (FNV-1a) for stable shuffling.
fn fnv1a(input: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for byte in input.as_bytes() {
        h ^= *byte as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

pub async fn get_recommendations(seeds: &[String], limit: usize) -> ApiResult<Vec<Track>> {
    let normalized: Vec<String> = seeds
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(4)
        .collect();
    let key = format!(
        "rec::{}::{}",
        normalized.join("|").to_lowercase(),
        limit
    );
    if let Some(hit) = recommendation_cache().get(&key) {
        return Ok(hit);
    }

    let queries: Vec<String> = if normalized.is_empty() {
        vec!["popular music right now".to_string()]
    } else {
        normalized.clone()
    };
    let per_query = std::cmp::max(6, (limit as f64 / queries.len() as f64).ceil() as usize + 2);

    let mut out: Vec<Track> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for query in &queries {
        match youtube::search(query, per_query).await {
            Ok(results) => {
                for t in results.tracks {
                    if seen.insert(t.id.clone()) {
                        out.push(t);
                    }
                }
            }
            Err(err) => {
                eprintln!("[recommendations] seed failed ({query}): {err}");
            }
        }
    }

    // Deterministic shuffle keyed by seed+id so order is stable.
    let shuffle_key = if normalized.is_empty() {
        "default".to_string()
    } else {
        normalized.join("|")
    };
    out.sort_by_key(|t| fnv1a(&format!("{shuffle_key}{}", t.id)));
    out.truncate(limit);
    recommendation_cache().set(&key, out.clone(), None);
    Ok(out)
}
