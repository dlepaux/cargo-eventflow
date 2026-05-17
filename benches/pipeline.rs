// Pipeline benchmarks — placeholder so `cargo bench --no-run` succeeds in CI.
// Real benches land in story 07.

use criterion::{criterion_group, criterion_main, Criterion};

fn placeholder(c: &mut Criterion) {
    c.bench_function("placeholder", |b| {
        b.iter(|| {
            // No-op until story 07 wires the real pipeline benches.
            std::hint::black_box(1 + 1)
        });
    });
}

criterion_group!(benches, placeholder);
criterion_main!(benches);
