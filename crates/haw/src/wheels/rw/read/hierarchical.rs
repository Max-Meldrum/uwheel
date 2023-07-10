use core::{
    cmp,
    fmt::Debug,
    iter::IntoIterator,
    option::{
        Option,
        Option::{None, Some},
    },
};

#[cfg(feature = "rkyv")]
use rkyv::{
    de::deserializers::{SharedDeserializeMap, SharedDeserializeMapError},
    ser::serializers::{
        AlignedSerializer,
        AllocScratch,
        CompositeSerializer,
        FallbackScratch,
        HeapScratch,
        SharedSerializeMap,
    },
    ser::Serializer,
    AlignedVec,
};
#[cfg(feature = "rkyv")]
use rkyv::{Archive, Deserialize, Infallible, Serialize};

#[cfg(all(feature = "rkyv", not(feature = "std")))]
use alloc::boxed::Box;

use super::{
    super::write::WriteAheadWheel,
    aggregation::{AggregationWheel, MaybeWheel},
};
use crate::{aggregator::Aggregator, time};

pub const SECONDS: usize = 60;
pub const MINUTES: usize = 60;
pub const HOURS: usize = 24;
pub const DAYS: usize = 7;
pub const WEEKS: usize = 52;
pub const YEARS: usize = 10;

// Default Wheel Capacities (power of two)
const SECONDS_CAP: usize = 64;
const MINUTES_CAP: usize = 64;
const HOURS_CAP: usize = 32;
const DAYS_CAP: usize = 8;
const WEEKS_CAP: usize = 64;
const YEARS_CAP: usize = YEARS.next_power_of_two();

/// Type alias for an AggregationWheel representing seconds
pub type SecondsWheel<A> = AggregationWheel<SECONDS_CAP, A>;
/// Type alias for an AggregationWheel representing minutes
pub type MinutesWheel<A> = AggregationWheel<MINUTES_CAP, A>;
/// Type alias for an AggregationWheel representing hours
pub type HoursWheel<A> = AggregationWheel<HOURS_CAP, A>;
/// Type alias for an AggregationWheel representing days
pub type DaysWheel<A> = AggregationWheel<DAYS_CAP, A>;
/// Type alias for an AggregationWheel representing weeks
pub type WeeksWheel<A> = AggregationWheel<WEEKS_CAP, A>;
/// Type alias for an AggregationWheel representing years
pub type YearsWheel<A> = AggregationWheel<YEARS_CAP, A>;

cfg_rkyv! {
    // Alias for an aggregators [`MutablePartialAggregate`] type
    pub type MutablePartialAggregate<A> = <A as Aggregator>::MutablePartialAggregate;
    // Alias for an aggregators [`PartialAggregate`] type
    pub type PartialAggregate<A> = <A as Aggregator>::PartialAggregate;
    // Alias for an aggregators [`PartialAggregate`] archived type
    type Archived<A> = <<A as Aggregator>::PartialAggregate as Archive>::Archived;
    // Alias for the default serializer used by the crate
    pub type DefaultSerializer = CompositeSerializer<
        AlignedSerializer<AlignedVec>,
        FallbackScratch<HeapScratch<4096>, AllocScratch>,
        SharedSerializeMap,
    >;
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    drill_down: bool,
}
impl Options {
    pub fn with_drill_down(mut self) -> Self {
        self.drill_down = true;
        self
    }
}

/// Hierarchical Aggregation Wheel
#[repr(C)]
#[cfg_attr(feature = "rkyv", derive(Archive, Deserialize, Serialize))]
#[derive(Clone, Debug)]
pub struct Haw<A>
where
    A: Aggregator,
{
    watermark: u64,
    #[cfg_attr(feature = "rkyv", omit_bounds)]
    seconds_wheel: MaybeWheel<SECONDS_CAP, A>,
    #[cfg_attr(feature = "rkyv", omit_bounds)]
    minutes_wheel: MaybeWheel<MINUTES_CAP, A>,
    hours_wheel: MaybeWheel<HOURS_CAP, A>,
    days_wheel: MaybeWheel<DAYS_CAP, A>,
    weeks_wheel: MaybeWheel<WEEKS_CAP, A>,
    years_wheel: MaybeWheel<YEARS_CAP, A>,
    // for some reason rkyv fails to compile without this.
    #[cfg(feature = "rkyv")]
    _marker: A::PartialAggregate,
}

impl<A> Haw<A>
where
    A: Aggregator,
{
    const SECOND_AS_MS: u64 = time::Duration::SECOND.whole_milliseconds() as u64;
    const MINUTES_AS_SECS: u64 = time::Duration::MINUTE.whole_seconds() as u64;
    const HOURS_AS_SECS: u64 = time::Duration::HOUR.whole_seconds() as u64;
    const DAYS_AS_SECS: u64 = time::Duration::DAY.whole_seconds() as u64;
    const WEEK_AS_SECS: u64 = time::Duration::WEEK.whole_seconds() as u64;
    const YEAR_AS_SECS: u64 = Self::WEEK_AS_SECS * WEEKS as u64;
    const CYCLE_LENGTH_SECS: u64 = Self::CYCLE_LENGTH.whole_seconds() as u64;

    const TOTAL_SECS_IN_WHEEL: u64 = Self::YEAR_AS_SECS * YEARS as u64;
    pub const CYCLE_LENGTH: time::Duration =
        time::Duration::seconds((Self::YEAR_AS_SECS * (YEARS as u64 + 1)) as i64); // need 1 extra to force full cycle rotation
    pub const TOTAL_WHEEL_SLOTS: usize = SECONDS + MINUTES + HOURS + DAYS + WEEKS + YEARS;

    /// Creates a new Wheel starting from the given time with drill-down capabilities
    ///
    /// Time is represented as milliseconds
    pub fn with_drill_down(time: u64) -> Self {
        let opts = Options::default().with_drill_down();
        Self::with_options(time, opts)
    }

    /// Creates a new Wheel starting from the given time
    ///
    /// Time is represented as milliseconds
    pub fn new(time: u64) -> Self {
        Self::base(time)
    }

    pub fn with_options(time: u64, options: Options) -> Self {
        if options.drill_down {
            Self::base_drill_down(time)
        } else {
            Self::base(time)
        }
    }

    fn base(time: u64) -> Self {
        Self {
            watermark: time,
            seconds_wheel: MaybeWheel::with_capacity(SECONDS),
            minutes_wheel: MaybeWheel::with_capacity(MINUTES),
            hours_wheel: MaybeWheel::with_capacity(HOURS),
            days_wheel: MaybeWheel::with_capacity(DAYS),
            weeks_wheel: MaybeWheel::with_capacity(WEEKS),
            years_wheel: MaybeWheel::with_capacity(YEARS),
            #[cfg(feature = "rkyv")]
            _marker: Default::default(),
        }
    }
    fn base_drill_down(time: u64) -> Self {
        Self {
            watermark: time,
            seconds_wheel: MaybeWheel::with_capacity_and_drill_down(SECONDS),
            minutes_wheel: MaybeWheel::with_capacity_and_drill_down(MINUTES),
            hours_wheel: MaybeWheel::with_capacity_and_drill_down(HOURS),
            days_wheel: MaybeWheel::with_capacity_and_drill_down(DAYS),
            weeks_wheel: MaybeWheel::with_capacity_and_drill_down(WEEKS),
            years_wheel: MaybeWheel::with_capacity_and_drill_down(YEARS),
            #[cfg(feature = "rkyv")]
            _marker: Default::default(),
        }
    }

    /// Returns how many wheel slots are utilised
    pub fn len(&self) -> usize {
        self.seconds_wheel.len()
            + self.minutes_wheel.len()
            + self.hours_wheel.len()
            + self.days_wheel.len()
            + self.weeks_wheel.len()
            + self.years_wheel.len()
    }

    /// Returns true if the internal wheel time has never been advanced
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns true if all slots in the hierarchy are utilised
    pub fn is_full(&self) -> bool {
        self.len() == Self::TOTAL_WHEEL_SLOTS
    }

    /// Returns how many ticks (seconds) are left until the wheel is fully utilised
    pub fn remaining_ticks(&self) -> u64 {
        Self::TOTAL_SECS_IN_WHEEL - self.current_time_in_cycle().whole_seconds() as u64
    }

    /// Returns Duration that represents where the wheel currently is in its cycle
    #[inline]
    pub fn current_time_in_cycle(&self) -> time::Duration {
        let secs = self.seconds_wheel.rotation_count() as u64;
        let min_secs = self.minutes_wheel.rotation_count() as u64 * Self::MINUTES_AS_SECS;
        let hr_secs = self.hours_wheel.rotation_count() as u64 * Self::HOURS_AS_SECS;
        let day_secs = self.days_wheel.rotation_count() as u64 * Self::DAYS_AS_SECS;
        let week_secs = self.weeks_wheel.rotation_count() as u64 * Self::WEEK_AS_SECS;
        let year_secs = self.years_wheel.rotation_count() as u64 * Self::YEAR_AS_SECS;
        let cycle_time = secs + min_secs + hr_secs + day_secs + week_secs + year_secs;
        time::Duration::seconds(cycle_time as i64)
    }

    /// Advance the watermark of the wheel by the given [time::Duration]
    #[inline(always)]
    pub fn advance(&mut self, duration: time::Duration, waw: &mut WriteAheadWheel<A>) {
        let mut ticks: usize = duration.whole_seconds() as usize;

        // helper fn to tick N times
        let tick_n = |ticks: usize, haw: &mut Self, waw: &mut WriteAheadWheel<A>| {
            for _ in 0..ticks {
                haw.tick(waw);
            }
        };

        if ticks <= SECONDS {
            tick_n(ticks, self, waw);
        } else if ticks <= Self::CYCLE_LENGTH_SECS as usize {
            // force full rotation
            let rem_ticks = self.seconds_wheel.get_or_insert().ticks_remaining();
            tick_n(rem_ticks, self, waw);
            ticks -= rem_ticks;

            // calculate how many fast_ticks we can perform
            let fast_ticks = ticks.saturating_div(SECONDS);

            if fast_ticks == 0 {
                // if fast ticks is 0, then tick normally
                tick_n(ticks, self, waw);
            } else {
                // perform a number of fast ticks
                // NOTE: currently a fast tick is a full SECONDS rotation.
                let fast_tick_ms = (SECONDS - 1) as u64 * Self::SECOND_AS_MS;
                for _ in 0..fast_ticks {
                    self.seconds_wheel.get_or_insert().fast_skip_tick();
                    self.watermark += fast_tick_ms;
                    *waw.watermark_mut() += fast_tick_ms;
                    self.tick(waw);
                    ticks -= SECONDS;
                }
                // tick any remaining ticks
                tick_n(ticks, self, waw);
            }
        } else {
            // Exceeds full cycle length, clear all!
            self.clear();
        }
    }

    /// Advances the time of the wheel aligned by the lowest unit (Second)
    #[inline]
    pub fn advance_to(&mut self, watermark: u64, waw: &mut WriteAheadWheel<A>) {
        let diff = watermark.saturating_sub(self.watermark());
        self.advance(time::Duration::milliseconds(diff as i64), waw);
    }

    /// Clears the state of all wheels
    pub fn clear(&mut self) {
        self.seconds_wheel.clear();
        self.minutes_wheel.clear();
        self.hours_wheel.clear();
        self.days_wheel.clear();
        self.weeks_wheel.clear();
        self.years_wheel.clear();
    }

    /// Return the current watermark as milliseconds for this wheel
    #[inline]
    pub fn watermark(&self) -> u64 {
        self.watermark
    }
    /// Returns the aggregate in the given time interval
    pub fn interval_and_lower(&self, dur: time::Duration) -> Option<A::Aggregate> {
        self.interval(dur).map(|partial| A::lower(partial))
    }

    /// Returns the partial aggregate in the given time interval
    #[inline]
    pub fn interval(&self, dur: time::Duration) -> Option<A::PartialAggregate> {
        // closure that turns i64 to None if it is zero
        let to_option = |num: i64| {
            if num == 0 {
                None
            } else {
                Some(num as usize)
            }
        };
        let second = to_option(dur.whole_seconds() % SECONDS as i64);
        let minute = to_option(dur.whole_minutes() % MINUTES as i64);
        let hour = to_option(dur.whole_hours() % HOURS as i64);
        let day = to_option(dur.whole_days() % DAYS as i64);
        let week = to_option(dur.whole_weeks() % WEEKS as i64);
        let year = to_option((dur.whole_weeks() / WEEKS as i64) % YEARS as i64);
        //dbg!((second, minute, hour, day, week, year));
        self.combine_time(second, minute, hour, day, week, year)
    }

    #[inline]
    pub fn combine_time(
        &self,
        second: Option<usize>,
        minute: Option<usize>,
        hour: Option<usize>,
        day: Option<usize>,
        week: Option<usize>,
        year: Option<usize>,
    ) -> Option<A::PartialAggregate> {
        // Multi-Wheel Query => Need to make sure we don't duplicate data across granularities.

        match (year, week, day, hour, minute, second) {
            // ywdhms
            (Some(year), Some(week), Some(day), Some(hour), Some(minute), Some(second)) => {
                let second = cmp::min(self.seconds_wheel.rotation_count(), second);
                let minute = cmp::min(self.minutes_wheel.rotation_count(), minute);
                let hour = cmp::min(self.hours_wheel.rotation_count(), hour);
                let day = cmp::min(self.days_wheel.rotation_count(), day);
                let week = cmp::min(self.weeks_wheel.rotation_count(), week);

                let sec = self.seconds_wheel.interval_or_total(second);
                let min = self.minutes_wheel.interval_or_total(minute);
                let hr = self.hours_wheel.interval_or_total(hour);
                let day = self.days_wheel.interval_or_total(day);
                let week = self.weeks_wheel.interval_or_total(week);
                let year = self.years_wheel.interval(year);

                Self::reduce([sec, min, hr, day, week, year])
            }
            // wdhms
            (None, Some(week), Some(day), Some(hour), Some(minute), Some(second)) => {
                let second = cmp::min(self.seconds_wheel.rotation_count(), second);
                let minute = cmp::min(self.minutes_wheel.rotation_count(), minute);
                let hour = cmp::min(self.hours_wheel.rotation_count(), hour);
                let day = cmp::min(self.days_wheel.rotation_count(), day);

                let sec = self.seconds_wheel.interval_or_total(second);
                let min = self.minutes_wheel.interval_or_total(minute);
                let hr = self.hours_wheel.interval_or_total(hour);
                let day = self.days_wheel.interval_or_total(day);
                let week = self.days_wheel.interval(week);

                Self::reduce([sec, min, hr, day, week])
            }
            // dhms
            (None, None, Some(day), Some(hour), Some(minute), Some(second)) => {
                let second = cmp::min(self.seconds_wheel.rotation_count(), second);
                let minute = cmp::min(self.minutes_wheel.rotation_count(), minute);
                let hour = cmp::min(self.hours_wheel.rotation_count(), hour);

                let sec = self.seconds_wheel.interval_or_total(second);
                let min = self.minutes_wheel.interval_or_total(minute);
                let hr = self.hours_wheel.interval_or_total(hour);
                let day = self.days_wheel.interval(day);

                Self::reduce([sec, min, hr, day])
            }
            // dhm
            (None, None, Some(day), Some(hour), Some(minute), None) => {
                // Do not query below rotation count
                let minute = cmp::min(self.minutes_wheel.rotation_count(), minute);
                let hour = cmp::min(self.hours_wheel.rotation_count(), hour);

                let min = self.minutes_wheel.interval_or_total(minute);
                let hr = self.hours_wheel.interval_or_total(hour);
                let day = self.days_wheel.interval(day);
                Self::reduce([min, hr, day])
            }
            // dh
            (None, None, Some(day), Some(hour), None, None) => {
                let hour = cmp::min(self.hours_wheel.rotation_count(), hour);
                let hr = self.hours_wheel.interval_or_total(hour);
                let day = self.days_wheel.interval(day);
                Self::reduce([hr, day])
            }
            // d
            (None, None, Some(day), None, None, None) => self.days_wheel.interval(day),
            // hms
            (None, None, None, Some(hour), Some(minute), Some(second)) => {
                let second = cmp::min(self.seconds_wheel.rotation_count(), second);
                let minute = cmp::min(self.minutes_wheel.rotation_count(), minute);

                let sec = self.seconds_wheel.interval_or_total(second);
                let min = self.minutes_wheel.interval_or_total(minute);
                let hr = self.hours_wheel.interval(hour);
                Self::reduce([sec, min, hr])
            }
            // hm
            (None, None, None, Some(hour), Some(minute), None) => {
                let minute = cmp::min(self.minutes_wheel.rotation_count(), minute);
                let min = self.minutes_wheel.interval_or_total(minute);

                let hr = self.hours_wheel.interval(hour);
                Self::reduce([min, hr])
            }
            // yw
            (Some(year), Some(week), None, None, None, None) => {
                let week = cmp::min(self.weeks_wheel.rotation_count(), week);
                let week = self.weeks_wheel.interval_or_total(week);
                let year = self.years_wheel.interval(year);
                Self::reduce([year, week])
            }
            // y
            (Some(year), None, None, None, None, None) => self.years_wheel.interval_or_total(year),
            // w
            (None, Some(week), None, None, None, None) => self.weeks_wheel.interval_or_total(week),
            // h
            (None, None, None, Some(hour), None, None) => self.hours_wheel.interval_or_total(hour),
            // hs
            (None, None, None, Some(hour), None, Some(second)) => {
                let second = cmp::min(self.seconds_wheel.rotation_count(), second);
                let sec = self.seconds_wheel.interval_or_total(second);
                let hour = self.hours_wheel.interval_or_total(hour);
                Self::reduce([hour, sec])
            }
            // ms
            (None, None, None, None, Some(minute), Some(second)) => {
                let second = cmp::min(self.seconds_wheel.rotation_count(), second);
                let sec = self.seconds_wheel.interval_or_total(second);
                let min = self.minutes_wheel.interval(minute);
                Self::reduce([min, sec])
            }
            // m
            (None, None, None, None, Some(minute), None) => {
                self.minutes_wheel.interval_or_total(minute)
            }
            // s
            (None, None, None, None, None, Some(second)) => {
                self.seconds_wheel.interval_or_total(second)
            }
            t @ (_, _, _, _, _, _) => {
                panic!("combine_time was given invalid Time arguments {:?}", t);
            }
        }
    }

    /// Executes a Landmark Window that combines total partial aggregates across all wheels
    #[inline]
    pub fn landmark(&self) -> Option<A::PartialAggregate> {
        let wheels = [
            self.seconds_wheel.total(),
            self.minutes_wheel.total(),
            self.hours_wheel.total(),
            self.days_wheel.total(),
            self.weeks_wheel.total(),
            self.years_wheel.total(),
        ];
        Self::reduce(wheels)
    }
    /// Executes a Landmark Window that combines total partial aggregates across all wheels and lowers the result
    #[inline]
    pub fn landmark_and_lower(&self) -> Option<A::Aggregate> {
        self.landmark().map(|partial| A::lower(partial))
    }

    #[inline]
    fn reduce(
        partial_aggs: impl IntoIterator<Item = Option<A::PartialAggregate>>,
    ) -> Option<A::PartialAggregate> {
        partial_aggs
            .into_iter()
            .reduce(|acc, b| match (acc, b) {
                (Some(curr), Some(agg)) => Some(A::combine(curr, agg)),
                (None, Some(_)) => b,
                _ => acc,
            })
            .flatten()
    }

    /// Tick the wheel by a single unit (second)
    ///
    /// In the worst case, a tick may cause a rotation of all the wheels in the hierarchy.
    #[inline]
    fn tick(&mut self, waw: &mut WriteAheadWheel<A>) {
        self.watermark += Self::SECOND_AS_MS;

        let seconds = self.seconds_wheel.get_or_insert();

        // Tick the Write-ahead wheel, if new entry insert into head of seconds wheel
        if let Some(window) = waw.tick() {
            let partial_agg = A::freeze(window);
            seconds.insert_head(partial_agg)
        }

        // full rotation of seconds wheel
        if let Some(rot_data) = seconds.tick() {
            // insert 60 seconds worth of partial aggregates into minute wheel and then tick it
            let minutes = self.minutes_wheel.get_or_insert();

            minutes.insert_rotation_data(rot_data);

            // full rotation of minutes wheel
            if let Some(rot_data) = minutes.tick() {
                // insert 60 minutes worth of partial aggregates into hours wheel and then tick it
                let hours = self.hours_wheel.get_or_insert();

                hours.insert_rotation_data(rot_data);

                // full rotation of hours wheel
                if let Some(rot_data) = hours.tick() {
                    // insert 24 hours worth of partial aggregates into days wheel and then tick it
                    let days = self.days_wheel.get_or_insert();
                    days.insert_rotation_data(rot_data);

                    // full rotation of days wheel
                    if let Some(rot_data) = days.tick() {
                        // insert 7 days worth of partial aggregates into weeks wheel and then tick it
                        let weeks = self.weeks_wheel.get_or_insert();

                        weeks.insert_rotation_data(rot_data);

                        // full rotation of weeks wheel
                        if let Some(rot_data) = weeks.tick() {
                            // insert 1 years worth of partial aggregates into year wheel and then tick it
                            let years = self.years_wheel.get_or_insert();
                            years.insert_rotation_data(rot_data);

                            // tick but ignore full rotations as this is the last hierarchy
                            let _ = years.tick();
                        }
                    }
                }
            }
        }
    }

    /// Returns a reference to the seconds wheel
    pub fn seconds(&self) -> Option<&SecondsWheel<A>> {
        self.seconds_wheel.as_deref()
    }
    pub fn seconds_unchecked(&self) -> &SecondsWheel<A> {
        self.seconds_wheel.as_deref().unwrap()
    }

    /// Returns a reference to the minutes wheel
    pub fn minutes(&self) -> Option<&MinutesWheel<A>> {
        self.minutes_wheel.as_deref()
    }
    pub fn minutes_unchecked(&self) -> &MinutesWheel<A> {
        self.minutes_wheel.as_deref().unwrap()
    }
    /// Returns a reference to the hours wheel
    pub fn hours(&self) -> Option<&HoursWheel<A>> {
        self.hours_wheel.as_deref()
    }
    pub fn hours_unchecked(&self) -> &HoursWheel<A> {
        self.hours_wheel.as_deref().unwrap()
    }
    /// Returns a reference to the days wheel
    pub fn days(&self) -> Option<&DaysWheel<A>> {
        self.days_wheel.as_deref()
    }
    pub fn days_unchecked(&self) -> &DaysWheel<A> {
        self.days_wheel.as_deref().unwrap()
    }

    /// Returns a reference to the weeks wheel
    pub fn weeks(&self) -> Option<&WeeksWheel<A>> {
        self.weeks_wheel.as_deref()
    }
    pub fn weeks_unchecked(&self) -> &WeeksWheel<A> {
        self.weeks_wheel.as_deref().unwrap()
    }

    /// Returns a reference to the years wheel
    pub fn years(&self) -> Option<&YearsWheel<A>> {
        self.years_wheel.as_deref()
    }
    pub fn years_unchecked(&self) -> &YearsWheel<A> {
        self.years_wheel.as_deref().unwrap()
    }

    /// Merges two wheels
    ///
    /// Note that the time in `other` may be advanced and thus change state
    pub fn merge(&mut self, other: &mut Self) {
        let other_watermark = other.watermark();

        // make sure both wheels are aligned by time
        if self.watermark() > other_watermark {
            other.advance_to(self.watermark(), &mut WriteAheadWheel::default());
        } else {
            self.advance_to(other_watermark, &mut WriteAheadWheel::default());
        }

        // merge all aggregation wheels
        self.seconds_wheel.merge(&other.seconds_wheel);
        self.minutes_wheel.merge(&other.minutes_wheel);
        self.hours_wheel.merge(&other.hours_wheel);
        self.days_wheel.merge(&other.days_wheel);
        self.weeks_wheel.merge(&other.weeks_wheel);
        self.years_wheel.merge(&other.years_wheel);
    }

    #[cfg(feature = "rkyv")]
    /// Converts the wheel to bytes using the default serializer
    pub fn as_bytes(&self) -> AlignedVec
    where
        PartialAggregate<A>: Serialize<DefaultSerializer>,
    {
        let mut serializer = DefaultSerializer::default();
        serializer.serialize_value(self).unwrap();
        serializer.into_serializer().into_inner()
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a Wheel
    ///
    /// This function will deserialize the whole wheel.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SharedDeserializeMapError>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, SharedDeserializeMap>,
    {
        unsafe { rkyv::from_bytes_unchecked(bytes) }
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a seconds wheel
    ///
    /// For read-only operations that only target the seconds wheel,
    /// this function is more efficient as it will only deserialize a single wheel.
    pub fn seconds_wheel_from_bytes(bytes: &[u8]) -> Option<Box<SecondsWheel<A>>>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, Infallible>,
    {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let mut maybe: MaybeWheel<SECONDS_CAP, A> =
            archived.seconds_wheel.deserialize(&mut Infallible).unwrap();
        maybe.wheel.take()
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a minutes wheel
    ///
    /// For read-only operations that only target the minutes wheel,
    /// this function is more efficient as it will only deserialize a single wheel.
    pub fn minutes_wheel_from_bytes(bytes: &[u8]) -> Option<Box<MinutesWheel<A>>>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, Infallible>,
    {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let mut maybe: MaybeWheel<MINUTES_CAP, A> =
            archived.minutes_wheel.deserialize(&mut Infallible).unwrap();
        maybe.wheel.take()
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a hours wheel
    ///
    /// For read-only operations that only target the hours wheel,
    /// this function is more efficient as it will only deserialize a single wheel.
    pub fn hours_wheel_from_bytes(bytes: &[u8]) -> Option<Box<HoursWheel<A>>>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, Infallible>,
    {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let mut maybe: MaybeWheel<HOURS_CAP, A> =
            archived.hours_wheel.deserialize(&mut Infallible).unwrap();
        maybe.wheel.take()
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a days wheel
    ///
    /// For read-only operations that only target the days wheel,
    /// this function is more efficient as it will only deserialize a single wheel.
    pub fn days_wheel_from_bytes(bytes: &[u8]) -> Option<Box<DaysWheel<A>>>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, Infallible>,
    {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let mut maybe: MaybeWheel<DAYS_CAP, A> =
            archived.days_wheel.deserialize(&mut Infallible).unwrap();
        maybe.wheel.take()
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a weeks wheel
    ///
    /// For read-only operations that only target the weeks wheel,
    /// this function is more efficient as it will only deserialize a single wheel.
    pub fn weeks_wheel_from_bytes(bytes: &[u8]) -> Option<Box<WeeksWheel<A>>>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, Infallible>,
    {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let mut maybe: MaybeWheel<WEEKS_CAP, A> =
            archived.weeks_wheel.deserialize(&mut Infallible).unwrap();
        maybe.wheel.take()
    }

    #[cfg(feature = "rkyv")]
    /// Deserialise given bytes into a years wheel
    ///
    /// For read-only operations that only target the years wheel,
    /// this function is more efficient as it will only deserialize a single wheel.
    pub fn years_wheel_from_bytes(bytes: &[u8]) -> Option<Box<YearsWheel<A>>>
    where
        PartialAggregate<A>: Archive,
        Archived<A>: Deserialize<PartialAggregate<A>, Infallible>,
    {
        let archived = unsafe { rkyv::archived_root::<Self>(bytes) };
        let mut maybe: MaybeWheel<YEARS_CAP, A> =
            archived.years_wheel.deserialize(&mut Infallible).unwrap();
        maybe.wheel.take()
    }
}
#[cfg(feature = "sync")]
#[allow(unsafe_code)]
unsafe impl<A: Aggregator> Send for Haw<A> {}

#[cfg(feature = "sync")]
#[allow(unsafe_code)]
unsafe impl<A: Aggregator> Sync for Haw<A> {}
