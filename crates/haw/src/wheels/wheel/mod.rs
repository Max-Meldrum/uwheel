pub mod read;
pub mod write;

use crate::{aggregator::Aggregator, time, wheels::wheel::read::ReadWheelOps};
use core::fmt::Debug;
use read::rw_impl::ReadWheel;
use write::{WriteAheadWheel, DEFAULT_WRITE_AHEAD_SLOTS};

#[derive(Debug, Clone)]
pub struct Options {
    drill_down: bool,
    write_ahead_capacity: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            drill_down: false,
            write_ahead_capacity: DEFAULT_WRITE_AHEAD_SLOTS,
        }
    }
}
impl Options {
    pub fn with_drill_down(mut self) -> Self {
        self.drill_down = true;
        self
    }
    pub fn with_write_ahead(mut self, capacity: usize) -> Self {
        self.write_ahead_capacity = capacity;
        self
    }
}

/// A Reader-Writer Wheel
#[derive(Debug, Clone)]
pub struct RwWheel<A: Aggregator> {
    write: WriteAheadWheel<A>,
    read: ReadWheel<A>,
}
impl<A: Aggregator> RwWheel<A> {
    /// Creates a new Wheel starting from the given time
    ///
    /// Time is represented as milliseconds
    pub fn new(time: u64) -> Self {
        Self {
            write: WriteAheadWheel::with_watermark(time),
            read: ReadWheel::new(time),
        }
    }
    /// Creates a new Wheel starting from the given time with drill down enabled
    ///
    /// Time is represented as milliseconds
    pub fn with_drill_down(time: u64) -> Self {
        Self {
            write: WriteAheadWheel::with_watermark(time),
            read: ReadWheel::with_drill_down(time),
        }
    }
    pub fn with_options(time: u64, opts: Options) -> Self {
        let write: WriteAheadWheel<A> =
            WriteAheadWheel::with_capacity_and_watermark(opts.write_ahead_capacity, time);
        let read: ReadWheel<A> = if opts.drill_down {
            ReadWheel::with_drill_down(time)
        } else {
            ReadWheel::new(time)
        };
        Self { write, read }
    }
    /// Returns a mutable reference to the Write-ahead Wheel
    pub fn write(&mut self) -> &mut WriteAheadWheel<A> {
        &mut self.write
    }
    /// Returns a reference to the underlying ReadWheel
    pub fn read(&self) -> &ReadWheel<A> {
        &self.read
    }
    /// Returns the current watermark of this wheel
    pub fn watermark(&self) -> u64 {
        self.write.watermark()
    }
    /// Advance the watermark of the wheel by the given [time::Duration]
    #[inline]
    pub fn advance(&mut self, duration: time::Duration) {
        self.read.advance(duration, &mut self.write);
        debug_assert_eq!(self.write.watermark(), self.read.watermark());
    }

    /// Advances the time of the wheel aligned by the lowest unit (Second)
    #[inline]
    pub fn advance_to(&mut self, watermark: u64) {
        self.read.advance_to(watermark, &mut self.write);
        debug_assert_eq!(self.write.watermark(), self.read.watermark());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{aggregator::U32SumAggregator, time::*, wheels::WheelExt, *};

    #[cfg(feature = "sync")]
    #[test]
    fn read_wheel_move_thread_test() {
        let mut rw_wheel: RwWheel<U32SumAggregator> = RwWheel::new(0);
        rw_wheel.write().insert(Entry::new(1, 999)).unwrap();
        rw_wheel.advance(1.seconds());

        let read = rw_wheel.read().clone();

        let handle = std::thread::spawn(move || {
            assert_eq!(read.interval(1.seconds()), Some(1));
        });

        handle.join().expect("Failed to join the thread.");
    }

    #[test]
    fn interval_test() {
        let mut time = 0;
        let mut wheel = RwWheel::<U32SumAggregator>::new(time);
        wheel.advance(1.seconds());

        assert!(wheel.write().insert(Entry::new(1u32, 1000)).is_ok());
        assert!(wheel.write().insert(Entry::new(5u32, 5000)).is_ok());
        assert!(wheel.write().insert(Entry::new(11u32, 11000)).is_ok());

        wheel.advance(5.seconds());
        assert_eq!(wheel.write().watermark(), 6000);

        let expected: &[_] = &[&None, &Some(1u32), &None, &None, &None, &Some(5)];
        assert_eq!(
            &wheel
                .read()
                .raw()
                .seconds_unchecked()
                .iter()
                .collect::<Vec<&Option<u32>>>(),
            expected
        );

        assert_eq!(
            wheel.read().raw().seconds_unchecked().interval(5),
            Some(6u32)
        );
        assert_eq!(
            wheel.read().raw().seconds_unchecked().interval(1),
            Some(5u32)
        );

        time = 12000;
        wheel.advance_to(time);

        assert!(wheel.write().insert(Entry::new(100u32, 61000)).is_ok());
        assert!(wheel.write().insert(Entry::new(100u32, 63000)).is_ok());
        assert!(wheel.write().insert(Entry::new(100u32, 67000)).is_ok());

        // go pass seconds wheel
        time = 65000;
        wheel.advance_to(time);
    }

    #[test]
    fn mixed_timestamp_insertions_test() {
        let mut time = 1000;
        let mut wheel = RwWheel::<U32SumAggregator>::new(time);
        wheel.advance_to(time);

        assert!(wheel.write().insert(Entry::new(1u32, 1000)).is_ok());
        assert!(wheel.write().insert(Entry::new(5u32, 5000)).is_ok());
        assert!(wheel.write().insert(Entry::new(11u32, 11000)).is_ok());

        time = 6000; // new watermark
        wheel.advance_to(time);

        assert_eq!(wheel.read().raw().seconds_unchecked().total(), Some(6u32));
        // check we get the same result by combining the range of last 6 seconds
        assert_eq!(
            wheel
                .read()
                .raw()
                .seconds_unchecked()
                .combine_and_lower_range(0..5),
            Some(6u32)
        );
    }

    #[test]
    fn write_ahead_test() {
        let mut time = 0;
        let mut wheel = RwWheel::<U32SumAggregator>::new(time);

        time += 58000; // 58 seconds
        wheel.advance_to(time);
        // head: 58
        // tail:0
        assert_eq!(wheel.write().write_ahead_len(), 64);

        // current watermark is 58000, this should be rejected
        assert!(wheel
            .write()
            .insert(Entry::new(11u32, 11000))
            .unwrap_err()
            .is_late());

        // current watermark is 58000, with max_write_ahead_ts 128000.
        // should overflow
        assert!(wheel
            .write()
            .insert(Entry::new(11u32, 158000))
            .unwrap_err()
            .is_overflow());
    }

    #[test]
    fn full_cycle_test() {
        let mut wheel = RwWheel::<U32SumAggregator>::new(0);

        let ticks = wheel.read().remaining_ticks() - 1;
        wheel.advance(time::Duration::seconds(ticks as i64));

        // one tick away from full cycle clear
        assert_eq!(
            wheel.read().raw().seconds_unchecked().rotation_count(),
            SECONDS - 1
        );
        assert_eq!(
            wheel.read().raw().minutes_unchecked().rotation_count(),
            MINUTES - 1
        );
        assert_eq!(
            wheel.read().raw().hours_unchecked().rotation_count(),
            HOURS - 1
        );
        assert_eq!(
            wheel.read().raw().days_unchecked().rotation_count(),
            DAYS - 1
        );
        assert_eq!(
            wheel.read().raw().weeks_unchecked().rotation_count(),
            WEEKS - 1
        );
        assert_eq!(
            wheel.read().raw().years_unchecked().rotation_count(),
            YEARS - 1
        );

        // force full cycle clear
        wheel.advance(1.seconds());

        // rotation count of all wheels should be zero
        assert_eq!(wheel.read().raw().seconds_unchecked().rotation_count(), 0,);
        assert_eq!(wheel.read().raw().minutes_unchecked().rotation_count(), 0,);
        assert_eq!(wheel.read().raw().hours_unchecked().rotation_count(), 0,);
        assert_eq!(wheel.read().raw().days_unchecked().rotation_count(), 0,);
        assert_eq!(wheel.read().raw().weeks_unchecked().rotation_count(), 0,);
        assert_eq!(wheel.read().raw().years_unchecked().rotation_count(), 0,);

        // Verify len of all wheels
        assert_eq!(wheel.read().raw().seconds_unchecked().len(), SECONDS);
        assert_eq!(wheel.read().raw().minutes_unchecked().len(), MINUTES);
        assert_eq!(wheel.read().raw().hours_unchecked().len(), HOURS);
        assert_eq!(wheel.read().raw().days_unchecked().len(), DAYS);
        assert_eq!(wheel.read().raw().weeks_unchecked().len(), WEEKS);
        assert_eq!(wheel.read().raw().years_unchecked().len(), YEARS);

        assert!(wheel.read().is_full());
        assert!(!wheel.read().is_empty());
        assert!(wheel.read().landmark().is_none());
    }

    #[test]
    fn drill_down_test() {
        use crate::{aggregator::U64SumAggregator, wheels::wheel::read::aggregation::DrillCut};

        let mut time = 0;
        let mut wheel = RwWheel::<U64SumAggregator>::with_drill_down(time);

        let days_as_secs = time::Duration::days((DAYS + 1) as i64).whole_seconds();

        for _ in 0..days_as_secs {
            let entry = Entry::new(1u64, time);
            wheel.write().insert(entry).unwrap();
            time += 1000; // increase by 1 second
            wheel.advance_to(time);
        }

        // can't drill down on seconds wheel as it is the first wheel
        assert!(wheel
            .read()
            .raw()
            .seconds_unchecked()
            .drill_down(1)
            .is_none());

        // Drill down on each wheel (e.g., minute, hours, days) and confirm summed results

        assert_eq!(
            wheel
                .read()
                .raw()
                .minutes_unchecked()
                .drill_down(1)
                .unwrap()
                .iter()
                .sum::<u64>(),
            60u64
        );

        assert_eq!(
            wheel
                .read()
                .raw()
                .hours_unchecked()
                .drill_down(1)
                .unwrap()
                .iter()
                .sum::<u64>(),
            60u64 * 60
        );

        assert_eq!(
            wheel
                .read()
                .raw()
                .days_unchecked()
                .drill_down(1)
                .unwrap()
                .iter()
                .sum::<u64>(),
            60u64 * 60 * 24
        );

        // drill down range of 3 and confirm combined aggregates
        let decoded = wheel
            .read()
            .raw()
            .minutes_unchecked()
            .combine_drill_down_range(..3);
        assert_eq!(decoded[0], 3);
        assert_eq!(decoded[1], 3);
        assert_eq!(decoded[59], 3);

        // test cut of last 5 seconds of last 1 minute + first 10 aggregates of last 2 min
        let decoded = wheel
            .read()
            .raw()
            .minutes_unchecked()
            .drill_down_cut(
                DrillCut {
                    slot: 1,
                    range: 55..,
                },
                DrillCut {
                    slot: 2,
                    range: ..10,
                },
            )
            .unwrap();
        assert_eq!(decoded.len(), 15);
        let sum = decoded.iter().sum::<u64>();
        assert_eq!(sum, 15u64);

        // drill down whole of minutes wheel
        let decoded = wheel
            .read()
            .raw()
            .minutes_unchecked()
            .combine_drill_down_range(..);
        let sum = decoded.iter().sum::<u64>();
        assert_eq!(sum, 3600u64);
    }

    #[test]
    fn drill_down_holes_test() {
        let mut time = 0;
        let mut wheel = RwWheel::<U32SumAggregator>::with_drill_down(time);

        for _ in 0..30 {
            let entry = Entry::new(1u32, time);
            wheel.write().insert(entry).unwrap();
            time += 2000; // increase by 2 seconds
            wheel.advance_to(time);
        }

        wheel.advance_to(time);

        // confirm there are "holes" as we bump time by 2 seconds above
        let decoded = wheel
            .read()
            .raw()
            .minutes_unchecked()
            .drill_down(1)
            .unwrap()
            .to_vec();
        assert_eq!(decoded[0], 1);
        assert_eq!(decoded[1], 0);
        assert_eq!(decoded[2], 1);
        assert_eq!(decoded[3], 0);

        assert_eq!(decoded[58], 1);
        assert_eq!(decoded[59], 0);
    }

    #[test]
    fn merge_test() {
        let time = 0;
        let mut wheel = RwWheel::<U32SumAggregator>::new(time);

        let entry = Entry::new(1u32, 5000);
        wheel.write().insert(entry).unwrap();

        wheel.advance(7.days());

        let fresh_wheel_time = 0;
        let fresh_wheel = RwWheel::<U32SumAggregator>::new(fresh_wheel_time);
        fresh_wheel.read().merge(wheel.read());

        assert_eq!(fresh_wheel.read().watermark(), wheel.read().watermark());
        assert_eq!(fresh_wheel.read().landmark(), wheel.read().landmark());
        assert_eq!(
            fresh_wheel.read().remaining_ticks(),
            wheel.read().remaining_ticks()
        );
    }

    #[test]
    fn merge_drill_down_test() {
        let mut time = 0;
        let mut wheel = RwWheel::<U32SumAggregator>::with_drill_down(time);

        for _ in 0..30 {
            let entry = Entry::new(1u32, time);
            wheel.write().insert(entry).unwrap();
            time += 2000; // increase by 2 seconds
            wheel.advance_to(time);
        }

        wheel.advance_to(time);

        let mut time = 0;
        let mut other_wheel = RwWheel::<U32SumAggregator>::with_drill_down(time);

        for _ in 0..30 {
            let entry = Entry::new(1u32, time);
            other_wheel.write().insert(entry).unwrap();
            time += 2000; // increase by 2 seconds
            other_wheel.advance_to(time);
        }

        other_wheel.advance_to(time);

        // merge other_wheel into ´wheel´
        wheel.read().merge(other_wheel.read());

        // same as drill_down_holes test but confirm that drill down slots have be merged between wheels
        let decoded = wheel
            .read()
            .raw()
            .minutes_unchecked()
            .drill_down(1)
            .unwrap()
            .to_vec();
        assert_eq!(decoded[0], 2);
        assert_eq!(decoded[1], 0);
        assert_eq!(decoded[2], 2);
        assert_eq!(decoded[3], 0);

        assert_eq!(decoded[58], 2);
        assert_eq!(decoded[59], 0);
    }

    #[cfg(all(feature = "rkyv", feature = "std"))]
    #[test]
    fn serde_test() {
        /*
        let time = 1000;
        let wheel: RwWheel<U32SumAggregator> = RwWheel::new(time);

        let mut raw_wheel = wheel.read().raw().as_bytes();

        for _ in 0..3 {
            let mut wheel = Wheel::<U32SumAggregator>::from_bytes(&raw_wheel).unwrap();
            wheel.insert(Entry::new(1u32, time + 100)).unwrap();
            raw_wheel = wheel.as_bytes();
        }

        assert!(Wheel::<U32SumAggregator>::from_bytes(&raw_wheel).is_ok());
        */

        // TODO: fix WaW serialization
        /*
        time += 1000;
        wheel.advance_to(time);

        assert_eq!(
            wheel.seconds_unchecked().combine_and_lower_range(..),
            Some(3u32)
        );

        let raw_wheel = wheel.as_bytes();

        // deserialize seconds wheel only and confirm same query works
        let seconds_wheel =
            Wheel::<U32SumAggregator>::seconds_wheel_from_bytes(&raw_wheel).unwrap();

        assert_eq!(seconds_wheel.combine_and_lower_range(..), Some(3u32));
        */
    }
}