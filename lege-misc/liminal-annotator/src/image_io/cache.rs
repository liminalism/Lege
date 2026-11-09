//! A byte-budgeted LRU of decoded pages. Only the current page and a few
//! neighbours are ever resident; nothing scales with the project size.

use std::collections::HashMap;
use std::sync::Arc;

use super::decode::PageImage;

pub struct ImageCache {
    budget: usize,
    used: usize,
    tick: u64,
    entries: HashMap<String, (Arc<PageImage>, u64)>,
}

impl ImageCache {
    pub fn new(budget_bytes: usize) -> ImageCache {
        ImageCache {
            budget: budget_bytes,
            used: 0,
            tick: 0,
            entries: HashMap::new(),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<Arc<PageImage>> {
        self.tick += 1;
        let t = self.tick;
        self.entries.get_mut(key).map(|(img, used)| {
            *used = t;
            img.clone()
        })
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Insert, evicting least-recently-used entries other than `pinned`
    /// until the budget holds. A single page larger than the budget is
    /// still kept (it is the one on screen).
    pub fn insert(&mut self, key: String, img: Arc<PageImage>, pinned: &[&str]) {
        self.tick += 1;
        let size = img.byte_size();
        if let Some((old, _)) = self.entries.insert(key.clone(), (img, self.tick)) {
            self.used -= old.byte_size();
        }
        self.used += size;
        while self.used > self.budget {
            let victim = self
                .entries
                .iter()
                .filter(|(k, _)| k.as_str() != key && !pinned.contains(&k.as_str()))
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone());
            match victim {
                Some(k) => {
                    if let Some((img, _)) = self.entries.remove(&k) {
                        self.used -= img.byte_size();
                    }
                }
                None => break,
            }
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.used
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_io::decode::{Level, PageImage};

    fn img(n: usize) -> Arc<PageImage> {
        Arc::new(PageImage {
            levels: vec![Level {
                w: n as u32,
                h: 1,
                px: vec![0; n],
            }],
        })
    }

    #[test]
    fn evicts_lru_within_budget() {
        let mut c = ImageCache::new(100 * 4);
        c.insert("a".into(), img(40), &[]);
        c.insert("b".into(), img(40), &[]);
        c.get("a");
        c.insert("c".into(), img(40), &[]);
        assert!(c.contains("a") && c.contains("c") && !c.contains("b"));
        assert!(c.used_bytes() <= 400);
    }
}
