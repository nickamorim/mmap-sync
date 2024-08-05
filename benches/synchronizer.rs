use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use mmap_sync::synchronizer::Synchronizer;
use pprof::criterion::PProfProfiler;
use prost::Message;

/// Example data-structure shared between writer and reader(s)
#[derive(Message, PartialEq)]
pub struct CacheEntry {
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(bytes = "bytes", tag = "2")]
    pub key: ::prost::bytes::Bytes,
    #[prost(bytes = "bytes", tag = "3")]
    pub value: ::prost::bytes::Bytes,
}

pub fn bench_synchronizer(c: &mut Criterion) {
    let mut synchronizer = Synchronizer::new("/tmp/hello_world".as_ref());
    let normal_data = CacheEntry {
        version: 7,
        key: "key".as_bytes().into(),
        value: "value".as_bytes().into(),
    };
    let big_data = CacheEntry {
        version: 7,
        key: "k".repeat(250).into(),
        value: "v".repeat(250).into(),
    };

    let mut group = c.benchmark_group("synchronizer");
    group.throughput(Throughput::Elements(1));

    group.bench_function("normal_data/write", |b| {
        b.iter(|| {
            synchronizer
                .write(black_box(&normal_data), Duration::from_nanos(10))
                .expect("failed to write data");
        })
    });

    group.bench_function("big_data/write", |b| {
        b.iter(|| {
            synchronizer
                .write(black_box(&big_data), Duration::from_nanos(10))
                .expect("failed to write data");
        })
    });

    group.bench_function("normal_data/read", |b| {
        b.iter(|| {
            let entry = unsafe { synchronizer.read::<CacheEntry>().unwrap() };
            assert_eq!(entry.version, normal_data.version);
        })
    });

    group.bench_function("big_data/read", |b| {
        b.iter(|| {
            let entry = unsafe { synchronizer.read::<CacheEntry>().unwrap() };
            assert_eq!(entry.version, big_data.version);
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().with_profiler(PProfProfiler::new(100, pprof::criterion::Output::Protobuf));
    targets = bench_synchronizer
}
criterion_main!(benches);
