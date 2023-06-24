use super::{
    util::{create_pair_type, pairs_capacity, pairs_space, PairType},
    WindowWheel,
};
use crate::{
    aggregator::{Aggregator, InverseExt},
    time::Duration,
    Entry,
    Error,
    Wheel,
};

use core::iter::Iterator;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, vec::Vec};

#[cfg(feature = "rkyv")]
use rkyv::{Archive, Deserialize, Serialize};

/// A fixed-sized wheel used to maintain partial aggregates for slides that can later
/// be used to inverse windows.
#[repr(C)]
#[cfg_attr(feature = "rkyv", derive(Archive, Deserialize, Serialize))]
#[derive(Debug, Clone)]
pub struct PairsWheel<A: Aggregator> {
    capacity: usize,
    aggregator: A,
    slots: Box<[Option<A::PartialAggregate>]>,
    tail: usize,
    head: usize,
}

impl<A: Aggregator> PairsWheel<A> {
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(capacity.is_power_of_two(), "Capacity must be power of two");
        Self {
            capacity,
            aggregator: Default::default(),
            slots: (0..capacity)
                .map(|_| None)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            head: 0,
            tail: 0,
        }
    }
    #[inline]
    pub fn tick(&mut self) -> Option<A::PartialAggregate> {
        if !self.is_empty() {
            let tail = self.tail;
            self.tail = self.wrap_add(self.tail, 1);
            self.slot(tail).take()
        } else {
            None
        }
    }

    /// Returns `true` if the wheel is empty or `false` if it contains slots
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.tail == self.head
    }

    #[inline]
    pub fn push(&mut self, data: A::PartialAggregate, aggregator: &A) {
        Self::insert(self.slot(self.head), data, aggregator);
        self.head = self.wrap_add(self.head, 1);
    }

    #[inline]
    fn slot(&mut self, idx: usize) -> &mut Option<A::PartialAggregate> {
        &mut self.slots[idx]
    }
    #[inline]
    fn insert(slot: &mut Option<A::PartialAggregate>, entry: A::PartialAggregate, aggregator: &A) {
        match slot {
            Some(curr) => {
                let new_curr = aggregator.combine(*curr, entry);
                *curr = new_curr;
            }
            None => {
                *slot = Some(entry);
            }
        }
    }
    pub(crate) fn slot_idx_from_head(&self, subtrahend: usize) -> usize {
        self.wrap_sub(self.head, subtrahend)
    }
    /// Returns the index in the underlying buffer for a given logical element
    /// index - subtrahend.
    #[inline]
    fn wrap_sub(&self, idx: usize, subtrahend: usize) -> usize {
        wrap_index(idx.wrapping_sub(subtrahend), self.capacity)
    }
    /// Combines partial aggregates of the last `subtrahend` slots
    ///
    /// - If given a interval, returns the combined partial aggregate based on that interval,
    ///   or `None` if out of bounds
    #[inline]
    pub fn interval(&self, subtrahend: usize) -> Option<A::PartialAggregate> {
        let tail = self.slot_idx_from_head(subtrahend);
        let iter = Iter::<A>::new(&self.slots, tail, self.head);
        Some(self.combine_slots(iter))
    }
    // helper method to combine partial aggregates in slots
    // TODO: duplicate of AggregationWheel
    #[inline]
    fn combine_slots<'a>(
        &self,
        iter: impl Iterator<Item = &'a Option<A::PartialAggregate>>,
    ) -> A::PartialAggregate {
        iter.flatten()
            .fold(Default::default(), |a, b| self.aggregator.combine(a, *b))
    }

    /// Returns the current number of used slots (includes empty NONE slots as well)
    pub fn len(&self) -> usize {
        count(self.tail, self.head, self.capacity)
    }
    /// Returns the index in the underlying buffer for a given logical element
    /// index + addend.
    #[inline]
    fn wrap_add(&self, idx: usize, addend: usize) -> usize {
        wrap_index(idx.wrapping_add(addend), self.capacity)
    }
}

/// Returns the index in the underlying buffer for a given logical element index.
#[inline]
fn wrap_index(index: usize, size: usize) -> usize {
    // size is always a power of 2
    debug_assert!(size.is_power_of_two());
    index & (size - 1)
}

/// Calculate the number of elements left to be read in the buffer
#[inline]
fn count(tail: usize, head: usize, size: usize) -> usize {
    // size is always a power of 2
    (head.wrapping_sub(tail)) & (size - 1)
}

pub struct Iter<'a, A: Aggregator> {
    ring: &'a [Option<A::PartialAggregate>],
    tail: usize,
    head: usize,
}

impl<'a, A: Aggregator> Iter<'a, A> {
    pub(super) fn new(ring: &'a [Option<A::PartialAggregate>], tail: usize, head: usize) -> Self {
        Iter { ring, tail, head }
    }
}
impl<'a, A: Aggregator> Iterator for Iter<'a, A> {
    type Item = &'a Option<A::PartialAggregate>;

    #[inline]
    fn next(&mut self) -> Option<&'a Option<A::PartialAggregate>> {
        if self.tail == self.head {
            return None;
        }
        let tail = self.tail;
        self.tail = wrap_index(self.tail.wrapping_add(1), self.ring.len());
        // Safety:
        // - `self.tail` in a ring buffer is always a valid index.
        // - `self.head` and `self.tail` equality is checked above.
        Some(&self.ring[tail])
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = count(self.tail, self.head, self.ring.len());
        (len, Some(len))
    }
}

#[derive(Default, Copy, Clone)]
pub struct Builder {
    range: usize,
    slide: usize,
    time: u64,
}

impl Builder {
    pub fn with_watermark(mut self, watermark: u64) -> Self {
        self.time = watermark;
        self
    }
    pub fn with_range(mut self, range: Duration) -> Self {
        self.range = range.whole_milliseconds() as usize;
        self
    }
    pub fn with_slide(mut self, slide: Duration) -> Self {
        self.slide = slide.whole_milliseconds() as usize;
        self
    }
    pub fn build<A: Aggregator + InverseExt>(self) -> LazyWindowWheel<A> {
        // TODO: sanity check of range and slide
        LazyWindowWheel::new(self.time, self.range, self.slide)
    }
}

/// A window wheel that uses the Pairs technique to store partial aggregates
/// for a RANGE r and SLIDE s. It utilises a regular HAW for insertions and populating
/// window slices.
#[allow(dead_code)]
pub struct LazyWindowWheel<A: Aggregator> {
    range: usize,
    slide: usize,
    pair_ticks_remaining: usize,
    current_pair_len: usize,
    pair_type: PairType,
    pairs_wheel: PairsWheel<A>,
    wheel: Wheel<A>,
    // When the next window starts
    next_window_start: u64,
    // When the next window ends
    next_window_end: u64,
    next_pair_start: u64,
    next_pair_end: u64,
    in_p1: bool,
    window_results: Vec<A::PartialAggregate>,
    aggregator: A,
}

impl<A: Aggregator> LazyWindowWheel<A> {
    pub fn new(time: u64, range: usize, slide: usize) -> Self {
        let pair_type = create_pair_type(range, slide);
        let next_window_start = time + slide as u64;
        let current_pair_len = match pair_type {
            PairType::Even(slide) => slide,
            PairType::Uneven(_, p2) => p2,
        };
        let next_pair_start = 0;
        let next_pair_end = time + current_pair_len as u64;
        Self {
            range,
            slide,
            current_pair_len,
            pair_ticks_remaining: current_pair_len,
            pair_type: create_pair_type(range, slide),
            pairs_wheel: PairsWheel::with_capacity(pairs_capacity(range, slide)),
            wheel: Wheel::new(time),
            next_window_start,
            next_window_end: time + range as u64,
            next_pair_start,
            next_pair_end,
            in_p1: false,
            window_results: Vec::new(),
            aggregator: Default::default(),
        }
    }
    fn current_pair_duration(&self) -> Duration {
        Duration::milliseconds(self.current_pair_len as i64)
    }
    fn update_pair_len(&mut self) {
        if let PairType::Uneven(p1, p2) = self.pair_type {
            if self.in_p1 {
                self.current_pair_len = p2;
                self.in_p1 = false;
            } else {
                self.current_pair_len = p1;
                self.in_p1 = true;
            }
        }
    }
    fn _range_interval_duration(&self) -> Duration {
        Duration::seconds((self.range / 1000) as i64)
    }
    // Combines aggregates from the Pairs Wheel in worst-case [2r/s] or best-case [r/s]
    #[inline]
    fn compute_window(&self) -> A::PartialAggregate {
        let pair_slots = pairs_space(self.range, self.slide);
        self.pairs_wheel.interval(pair_slots).unwrap_or_default()
    }
}

impl<A: Aggregator> WindowWheel<A> for LazyWindowWheel<A> {
    fn advance_to(&mut self, new_watermark: u64) {
        //let diff = new_watermark.saturating_sub(self.wheel.watermark());
        //let ticks = diff / self.slide as u64;
        /*
        dbg!((
            self.wheel.watermark(),
            self.next_pair_end,
            self.next_window_end,
            self.current_pair_len
        ));
        */

        // "Intuitively, pairs break slicing only when a stream window starts or ends"

        // if we passed a pair slice then we need to store it in the Pairs Wheel
        if new_watermark >= self.next_pair_end {
            self.wheel.advance_to(self.next_pair_end);
            // take partial aggregate for this pair and insert into Pairs Wheel?

            let partial = self
                .wheel
                .interval(self.current_pair_duration())
                .unwrap_or_default();

            // Update pair metadata
            self.update_pair_len();

            self.next_pair_end = self.wheel.watermark() + self.current_pair_len as u64;

            self.pairs_wheel.push(partial, &self.aggregator);
        }

        if new_watermark >= self.next_window_end {
            self.wheel.advance_to(self.next_window_end);

            // Window computation:
            let window = self.compute_window();

            // how many "pairs" we need to pop off from the Pairs wheel
            let removals = match self.pair_type {
                PairType::Even(_) => 1,
                PairType::Uneven(_, _) => 2,
            };
            for _i in 0..removals {
                let _ = self.pairs_wheel.tick();
            }

            self.window_results.push(window);

            // next window ends at next slide (p1+p2)
            self.next_window_end += self.slide as u64;
        }
        self.wheel.advance_to(new_watermark);
    }

    #[inline]
    fn insert(&mut self, entry: Entry<A::Input>) -> Result<(), Error<A::Input>> {
        self.wheel.insert(entry)
    }
    /// Returns a reference to the underlying HAW
    fn wheel(&self) -> &Wheel<A> {
        &self.wheel
    }
    // just for testing now
    fn results(&self) -> &[A::PartialAggregate] {
        &self.window_results
    }
}