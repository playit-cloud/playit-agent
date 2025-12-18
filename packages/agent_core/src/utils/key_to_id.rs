use std::{
    collections::{hash_map, HashMap},
    hash::Hash,
};

use super::id_slab::IdSlab;

pub struct KeyToId<K: Eq + Hash + Clone, V> {
    items: IdSlab<V>,
    lookup: HashMap<K, u64>,
}

impl<K: Eq + Hash + Clone, V> Default for KeyToId<K, V> {
    fn default() -> Self {
        Self {
            items: IdSlab::with_capacity(1024 * 1024),
            lookup: HashMap::new(),
        }
    }
}

impl<K: Eq + Hash + Clone, V> KeyToId<K, V> {
    pub fn get_or_add<F: FnOnce() -> V>(&mut self, key: K, value_fn: F) -> Option<u64> {
        match self.lookup.entry(key) {
            hash_map::Entry::Occupied(o) => Some(*o.get()),
            hash_map::Entry::Vacant(v) => {
                let entry = self.items.vacant_entry()?;

                let id = entry.id();
                entry.insert(value_fn());

                v.insert(id);
                Some(id)
            }
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<(u64, V)> {
        let id = self.lookup.remove(key)?;
        let removed_item = self.items.remove(id).expect("item at id not found");
        Some((id, removed_item))
    }
}
