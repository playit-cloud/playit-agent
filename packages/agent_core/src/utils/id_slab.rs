use slotmap::{DefaultKey, Key, KeyData, SlotMap};

pub struct IdSlab<T> {
    entries: SlotMap<DefaultKey, Entry<T>>,
    capacity: usize,
}

enum Entry<T> {
    Reserved,
    Occupied(T),
}

pub struct IdSlabVacantEntry<'a, T> {
    slab: Option<&'a mut IdSlab<T>>,
    key: DefaultKey,
}

impl<T> IdSlab<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: SlotMap::with_capacity(capacity),
            capacity,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn available(&self) -> usize {
        self.capacity.saturating_sub(self.entries.len())
    }

    pub fn get(&self, id: u64) -> Option<&T> {
        match self.entries.get(key_from_id(id))? {
            Entry::Reserved => None,
            Entry::Occupied(value) => Some(value),
        }
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut T> {
        match self.entries.get_mut(key_from_id(id))? {
            Entry::Reserved => None,
            Entry::Occupied(value) => Some(value),
        }
    }

    pub fn remove(&mut self, id: u64) -> Option<T> {
        match self.entries.remove(key_from_id(id))? {
            Entry::Reserved => None,
            Entry::Occupied(value) => Some(value),
        }
    }

    pub fn insert(&mut self, value: T) -> Result<u64, T> {
        if self.entries.len() >= self.capacity {
            return Err(value);
        }

        let key = self.entries.insert(Entry::Occupied(value));
        Ok(id_from_key(key))
    }

    pub fn vacant_entry(&mut self) -> Option<IdSlabVacantEntry<'_, T>> {
        if self.entries.len() >= self.capacity {
            return None;
        }

        let key = self.entries.insert(Entry::Reserved);
        Some(IdSlabVacantEntry {
            slab: Some(self),
            key,
        })
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.entries.values().filter_map(|entry| match entry {
            Entry::Reserved => None,
            Entry::Occupied(value) => Some(value),
        })
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.values_mut().filter_map(|entry| match entry {
            Entry::Reserved => None,
            Entry::Occupied(value) => Some(value),
        })
    }
}

impl<'a, T> IdSlabVacantEntry<'a, T> {
    pub fn id(&self) -> u64 {
        id_from_key(self.key)
    }

    pub fn insert(mut self, value: T) -> u64 {
        let slab = self
            .slab
            .take()
            .expect("vacant entry must always own its slab reference");
        let entry = slab
            .entries
            .get_mut(self.key)
            .expect("reserved slot must exist");
        *entry = Entry::Occupied(value);
        id_from_key(self.key)
    }
}

impl<'a, T> Drop for IdSlabVacantEntry<'a, T> {
    fn drop(&mut self) {
        if let Some(slab) = self.slab.take() {
            let _ = slab.entries.remove(self.key);
        }
    }
}

fn id_from_key(key: DefaultKey) -> u64 {
    key.data().as_ffi()
}

fn key_from_id(id: u64) -> DefaultKey {
    KeyData::from_ffi(id).into()
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use rand::{rng, seq::SliceRandom};

    use super::IdSlab;

    #[test]
    fn test() {
        let mut slab = IdSlab::<String>::with_capacity(16);
        let world_id = slab.insert("hello world".to_string()).unwrap();

        let mut old_ids = HashSet::new();
        let mut ids = Vec::new();

        for i in 0..100 {
            for j in 0..8 {
                let entry = slab.vacant_entry().unwrap();
                assert!(old_ids.insert(entry.id()));

                ids.push(entry.insert(format!("{} - {}", i, j)));
                assert_eq!(slab.len(), ids.len() + 1);
            }

            ids.shuffle(&mut rng());
            while 7 < ids.len() {
                let id = ids.pop().unwrap();

                slab.remove(id).unwrap();
                assert_eq!(slab.len(), ids.len() + 1);
            }
        }

        assert_eq!(slab.get(world_id).unwrap(), "hello world");
        for id in ids {
            slab.remove(id).unwrap();
        }
    }

    #[test]
    fn dropped_vacant_entry_releases_capacity() {
        let mut slab = IdSlab::<String>::with_capacity(1);

        {
            let _entry = slab.vacant_entry().unwrap();
        }

        assert_eq!(slab.available(), 1);
        assert!(slab.insert("value".to_string()).is_ok());
    }
}
