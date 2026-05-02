use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use numerical::twosum;
use std::hint::black_box;

const INPUT_SIZES: [usize; 7] = [1, 10, 100, 500, 1_000, 10_000, 100_000];

fn make_input(len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let numerator = ((i % 97) as f32) - 48.0;
            let denominator = ((i % 7) + 1) as f32;
            numerator / denominator
        })
        .collect()
}

fn throughput_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("sum/f32");

    for &size in &INPUT_SIZES {
        let input = make_input(size);
        group.throughput(Throughput::Elements(size as u64));

        group.bench_with_input(BenchmarkId::new("iter_sum", size), &input, |b, input| {
            b.iter(|| black_box(input).iter().sum::<f32>())
        });

        group.bench_with_input(BenchmarkId::new("twosum", size), &input, |b, input| {
            b.iter(|| black_box(twosum::sum::<_, f32>(black_box(input).iter())))
        });
    }

    group.finish();
}

criterion_group!(benches, throughput_benchmark);
criterion_main!(benches);
