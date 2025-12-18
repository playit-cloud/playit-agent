use std::{
    mem::{ManuallyDrop, MaybeUninit},
    u32,
};

pub struct IdSlab<T> {
    entries: Vec<Entry<T>>,
    free_slots: Vec<usize>,
}

struct Entry<T> {
    id: u64,
    value: MaybeUninit<T>,
}

pub struct IdSlabVacantEntry<'a, T> {
    slab: &'a mut IdSlab<T>,
    id: u64,
    slot: usize,
}

const EMPTY_BIT: u64 = 1u64 << 63;
const EMPTY_BIT_NEG: u64 = !EMPTY_BIT;
const USE_NUM: u64 = (u32::MAX as u64) + 1;
const SLOT_MASK: u64 = 0x00000000FFFFFFFF;

impl<T> IdSlab<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        let mut slab = IdSlab {
            entries: Vec::with_capacity(capacity),
            free_slots: Vec::with_capacity(capacity),
        };

        for pos in 0..capacity {
            slab.entries.push(Entry {
                id: EMPTY_BIT | (pos as u64),
                value: MaybeUninit::uninit(),
            });

            slab.free_slots.push(capacity - (pos + 1));
        }

        slab
    }

    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len() - self.free_slots.len()
    }

    pub fn available(&self) -> usize {
        self.free_slots.len()
    }

    pub fn get(&self, id: u64) -> Option<&T> {
        let slot = self.slot(id)?;

        let entry = &self.entries[slot];
        if (entry.id & EMPTY_BIT) == EMPTY_BIT {
            return None;
        }

        unsafe { Some(entry.value.assume_init_ref()) }
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut T> {
        let slot = self.slot(id)?;

        let entry = &mut self.entries[slot];
        if (entry.id & EMPTY_BIT) == EMPTY_BIT {
            return None;
        }

        unsafe { Some(entry.value.assume_init_mut()) }
    }

    pub fn remove(&mut self, id: u64) -> Option<T> {
        let slot = self.slot(id)?;

        let entry = &mut self.entries[slot];
        if (entry.id & EMPTY_BIT) == EMPTY_BIT {
            return None;
        }

        entry.id = EMPTY_BIT | (entry.id + USE_NUM);
        assert_eq!(entry.id & EMPTY_BIT, EMPTY_BIT);

        self.free_slots.push(slot);

        Some(unsafe { std::mem::replace(&mut entry.value, MaybeUninit::uninit()).assume_init() })
    }

    fn slot(&self, id: u64) -> Option<usize> {
        let slot = (id & SLOT_MASK) as usize;
        if self.entries.len() <= slot {
            return None;
        }
        Some(slot)
    }

    pub fn insert(&mut self, value: T) -> Result<u64, T> {
        let slot = match self.free_slots.pop() {
            Some(v) => v,
            None => return Err(value),
        };

        let entry = &mut self.entries[slot];
        assert!((entry.id & EMPTY_BIT) == EMPTY_BIT);

        entry.id = EMPTY_BIT_NEG & entry.id;
        assert!((entry.id & EMPTY_BIT) == 0);

        entry.value.write(value);
        Ok(entry.id)
    }

    pub fn vacant_entry(&mut self) -> Option<IdSlabVacantEntry<T>> {
        let slot = self.free_slots.pop()?;
        let id = self.entries[slot].id & EMPTY_BIT_NEG;

        Some(IdSlabVacantEntry {
            slab: self,
            id,
            slot,
        })
    }

    pub fn iter(&self) -> IdSlabIter<T> {
        IdSlabIter {
            slab: self,
            slot: 0,
            remaining: self.len(),
        }
    }

    pub fn iter_mut(&mut self) -> IdSlabIterMut<T> {
        let remaining = self.len();
        IdSlabIterMut {
            slab: self,
            slot: 0,
            remaining,
        }
    }
}

impl<'a, T> IdSlabVacantEntry<'a, T> {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn insert(self, value: T) -> u64 {
        let entry = &mut self.slab.entries[self.slot];
        assert!(entry.id & EMPTY_BIT == EMPTY_BIT);

        let id = EMPTY_BIT_NEG & entry.id;
        assert!((id & EMPTY_BIT) == 0);

        entry.id = id;
        entry.value.write(value);

        let _ = ManuallyDrop::new(self);
        id
    }
}

impl<'a, T> Drop for IdSlabVacantEntry<'a, T> {
    fn drop(&mut self) {
        self.slab.free_slots.push(self.slot);
    }
}

pub struct IdSlabIter<'a, T> {
    slab: &'a IdSlab<T>,
    slot: usize,
    remaining: usize,
}

impl<'a, T> Iterator for IdSlabIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        while self.slot < self.slab.entries.len() && self.remaining > 0 {
            let entry = &self.slab.entries[self.slot];
            self.slot += 1;

            if (entry.id & EMPTY_BIT) == EMPTY_BIT {
                continue;
            }

            self.remaining -= 1;
            return Some(unsafe { entry.value.assume_init_ref() });
        }

        None
    }
}

pub struct IdSlabIterMut<'a, T> {
    slab: &'a mut IdSlab<T>,
    slot: usize,
    remaining: usize,
}

impl<'a, T> Iterator for IdSlabIterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<Self::Item> {
        let len = self.slab.entries.len();

        while self.slot < len && self.remaining > 0 {
            let entry = &mut self.slab.entries[self.slot];
            self.slot += 1;

            if (entry.id & EMPTY_BIT) == EMPTY_BIT {
                continue;
            }

            self.remaining -= 1;
            return Some(unsafe { std::mem::transmute(entry.value.assume_init_mut()) });
        }

        None
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use rand::{seq::SliceRandom, thread_rng};

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

            ids.shuffle(&mut thread_rng());
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
}
