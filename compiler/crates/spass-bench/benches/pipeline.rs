//! Benchmarks the compiler pipeline stages.

use criterion::{criterion_group, criterion_main, Criterion};
use spass_codegen::{generate_c, generate_rust};
use spass_parser::Parser;
use spass_semantic::analyze;

const BOOTSTRAP: &str = include_str!("../../../../examples/bootstrap/src/bootstrap.spass");
const TERRAIN: &str = include_str!("../../../../examples/terrain/src/terrain.spass");

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    group.bench_function("bootstrap", |b| {
        b.iter(|| Parser::parse_source(BOOTSTRAP, "<bench>").expect("parse"))
    });
    group.bench_function("terrain", |b| {
        b.iter(|| Parser::parse_source(TERRAIN, "<bench>").expect("parse"))
    });
    group.finish();
}

fn bench_analyze(c: &mut Criterion) {
    let module = Parser::parse_source(BOOTSTRAP, "<bench>").expect("parse");
    let mut group = c.benchmark_group("analyze");
    group.bench_function("bootstrap", |b| b.iter(|| analyze(&module).expect("analyse")));
    group.finish();
}

fn bench_codegen(c: &mut Criterion) {
    let module = Parser::parse_source(BOOTSTRAP, "<bench>").expect("parse");
    let typed = analyze(&module).expect("analyse");
    let mut group = c.benchmark_group("codegen");
    group.bench_function("c", |b| b.iter(|| generate_c(&typed).expect("c codegen")));
    group.bench_function("rust", |b| b.iter(|| generate_rust(&typed).expect("rust codegen")));
    group.finish();
}

/// The whole compiler-owned pipeline: parse, analyse, emit. This is the
/// part of `spass run --direct` that happens before `rustc` starts.
fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline");
    group.bench_function("bootstrap", |b| {
        b.iter(|| {
            let module = Parser::parse_source(BOOTSTRAP, "<bench>").expect("parse");
            let typed = analyze(&module).expect("analyse");
            let c_src = generate_c(&typed).expect("c codegen");
            let rs_src = generate_rust(&typed).expect("rust codegen");
            (c_src.len(), rs_src.len())
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_parse,
    bench_analyze,
    bench_codegen,
    bench_pipeline
);
criterion_main!(benches);
