use std::{collections::HashMap, hash::Hash};

pub struct InstanceCount<K: Eq + Hash> {
    counters: HashMap<K, usize>,
}

impl<K: Eq + Hash> Default for InstanceCount<K> {
    fn default() -> Self {
        Self {
            counters: Default::default(),
        }
    }
}

impl<K: Eq + Hash + Clone> InstanceCount<K> {
    pub fn has_instance(&self, key: &K) -> bool {
        self.counters.contains_key(key)
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    pub fn inc(&mut self, key: &K) -> usize {
        if let Some(count) = self.counters.get_mut(key) {
            let value = *count + 1;
            *count = value;
            return value;
        }

        self.counters.insert(key.clone(), 1);
        1
    }

    pub fn dec(&mut self, key: &K) -> Option<usize> {
        let count = self.counters.get_mut(key)?;
        assert_ne!(*count, 0);

        let value = *count - 1;
        *count = value;

        if value == 0 {
            let _ = self.counters.remove(key);
        }

        Some(value)
    }
}
