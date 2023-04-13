use criterion::{criterion_group, criterion_main, Bencher, Criterion};
use haw::{aggregator::U32SumAggregator, *};

pub fn criterion_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("merge-wheels");
    group.bench_function("merge-large-to-fresh-wheel", merge_large_to_fresh_wheel);
    group.bench_function("merge-small-to-fresh-wheel", merge_small_to_fresh_wheel);
    group.bench_function("merge-large-same-size", merge_same_size_large);
    group.bench_function("merge-small-same-size", merge_same_size_small);

    group.finish();
}
fn large_wheel() -> Wheel<U32SumAggregator> {
    let mut time = 0;
    let mut wheel = Wheel::new(time);
    for _ in 0..wheel.remaining_ticks() {
        wheel.advance_to(time);
        wheel.insert(Entry::new(1u32, time)).unwrap();
        time += 1000;
    }
    wheel
}

fn small_wheel() -> Wheel<U32SumAggregator> {
    let mut time = 0;
    let mut wheel = Wheel::new(time);
    for _ in 0..60 {
        wheel.advance_to(time);
        wheel.insert(Entry::new(1u32, time)).unwrap();
        time += 1000;
    }
    wheel
}

fn merge_large_to_fresh_wheel(bencher: &mut Bencher) {
    let mut wheel = large_wheel();
    bencher.iter(|| {
        let mut fresh_wheel = Wheel::new(0);
        fresh_wheel.merge(&mut wheel);
        fresh_wheel
    });
}

fn merge_small_to_fresh_wheel(bencher: &mut Bencher) {
    let mut wheel = small_wheel();
    bencher.iter(|| {
        let mut fresh_wheel = Wheel::new(0);
        fresh_wheel.merge(&mut wheel);
        fresh_wheel
    });
}

fn merge_same_size_small(bencher: &mut Bencher) {
    let mut wheel = small_wheel();
    let mut other_wheel = small_wheel();
    bencher.iter(|| wheel.merge(&mut other_wheel));
}

fn merge_same_size_large(bencher: &mut Bencher) {
    let mut wheel = large_wheel();
    let mut other_wheel = large_wheel();
    bencher.iter(|| wheel.merge(&mut other_wheel));
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);