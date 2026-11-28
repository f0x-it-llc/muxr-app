//! A fixed-capacity run of evenly spaced optional values, oldest first; `revision` moves on every
//! mutation so a consumer can tell a changed run from an unchanged one.

use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub struct SeriesBuffer {
    slots: VecDeque<Option<f32>>,
    capacity: usize,
    revision: u64,
}

impl SeriesBuffer {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            slots: VecDeque::with_capacity(capacity),
            capacity,
            revision: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn push(&mut self, value: f32) {
        self.push_slot(Some(value));
    }

    pub fn push_gap(&mut self) {
        self.push_slot(None);
    }

    pub fn push_slot(&mut self, slot: Option<f32>) {
        self.append(slot);
        self.revision += 1;
    }

    /// Keeps the newest `capacity` of `values`.
    pub fn replace_all(&mut self, values: impl IntoIterator<Item = Option<f32>>) {
        self.slots.clear();
        for slot in values {
            self.append(slot);
        }
        self.revision += 1;
    }

    pub fn clear(&mut self) {
        self.slots.clear();
        self.revision += 1;
    }

    pub fn reset(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        self.clear();
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = Option<f32>> + '_ {
        self.slots.iter().copied()
    }

    fn append(&mut self, slot: Option<f32>) {
        if self.slots.len() == self.capacity {
            self.slots.pop_front();
        }
        self.slots.push_back(slot);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(buffer: &SeriesBuffer) -> Vec<Option<f32>> {
        buffer.values().collect()
    }

    #[test]
    fn pushes_past_capacity_drop_the_oldest_slot() {
        let mut buffer = SeriesBuffer::new(3);
        buffer.push(1.0);
        buffer.push_gap();
        buffer.push(3.0);
        buffer.push(4.0);
        assert_eq!(values(&buffer), [None, Some(3.0), Some(4.0)]);
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.capacity(), 3);
    }

    #[test]
    fn replace_all_keeps_the_newest_values_in_order() {
        let mut buffer = SeriesBuffer::new(2);
        buffer.replace_all([Some(1.0), None, Some(3.0)]);
        assert_eq!(values(&buffer), [None, Some(3.0)]);
        buffer.replace_all([Some(9.0)]);
        assert_eq!(values(&buffer), [Some(9.0)]);
    }

    #[test]
    fn every_mutation_moves_the_revision_forward() {
        let mut buffer = SeriesBuffer::new(2);
        let mut seen = vec![buffer.revision()];
        buffer.push(1.0);
        seen.push(buffer.revision());
        buffer.push_gap();
        seen.push(buffer.revision());
        buffer.replace_all([]);
        seen.push(buffer.revision());
        buffer.clear();
        seen.push(buffer.revision());
        buffer.reset(5);
        seen.push(buffer.revision());
        assert!(seen.windows(2).all(|pair| pair[1] > pair[0]), "{seen:?}");
        assert_eq!(buffer.capacity(), 5);
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_zero_capacity_holds_one_slot() {
        let mut buffer = SeriesBuffer::new(0);
        buffer.push(1.0);
        buffer.push(2.0);
        assert_eq!(values(&buffer), [Some(2.0)]);
    }
}
