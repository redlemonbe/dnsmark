// Benchmark SIMD vs scalaire — inclus directement depuis src/ (crate binaire)
// On contourne l'absence de lib.rs avec un module inline limité.
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

// Inclure simd et wire directement depuis src/
#[path = "../src/simd.rs"]
mod simd;

#[path = "../src/query/wire.rs"]
mod wire;

use wire::WireQueryPool;

fn write_scalar(pool: &WireQueryPool, local_idx: usize, id: u16, buf: &mut [u8]) -> usize {
    let tmpl = pool.get_template(local_idx);
    let len = tmpl.len();
    buf[..len].copy_from_slice(tmpl);
    buf[0] = (id >> 8) as u8;
    buf[1] = id as u8;
    len
}

fn bench_write(c: &mut Criterion) {
    let entries: Vec<(String, u16)> = vec![
        ("a.t".to_string(),                                        1),
        ("longer.example.bench.test".to_string(),                  1),
        ("very.long.qname.around.forty.bytes.t.test".to_string(),  1),
    ];
    let pool = WireQueryPool::from_pairs(&entries);
    let mut buf = [0u8; 512];

    let mut group = c.benchmark_group("write_with_index");
    group.sample_size(200);

    for (label, idx) in &[("short", 0usize), ("medium", 1usize), ("long", 2usize)] {
        let tmpl_len = pool.get_template(*idx).len();
        let id_str = format!("{}({}B)", label, tmpl_len);

        group.bench_with_input(
            BenchmarkId::new("simd", &id_str), idx,
            |b, &i| b.iter(|| black_box(
                pool.write_with_index(black_box(i), black_box(0xBEEF), &mut buf)
            )),
        );
        group.bench_with_input(
            BenchmarkId::new("scalar", &id_str), idx,
            |b, &i| b.iter(|| black_box(
                write_scalar(&pool, black_box(i), black_box(0xBEEF), &mut buf)
            )),
        );
    }
    group.finish();
}

criterion_group!(benches, bench_write);
criterion_main!(benches);
