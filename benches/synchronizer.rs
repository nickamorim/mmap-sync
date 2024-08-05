use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use mmap_sync::synchronizer::Synchronizer;
use pprof::criterion::PProfProfiler;
use prost::Message;

/// Example data-structure shared between writer and reader(s)
#[derive(Message, PartialEq)]
pub struct HelloWorld {
    #[prost(uint32, tag = "1")]
    pub version: u32,
    #[prost(string, repeated, tag = "2")]
    pub messages: Vec<String>,
}

pub fn bench_synchronizer(c: &mut Criterion) {
    let mut synchronizer = Synchronizer::new("/tmp/hello_world".as_ref());
    let data = HelloWorld {
        version: 7,
        messages: vec!["Hello".to_string(), "World".to_string(), "!".to_string()],
    };

    let mut group = c.benchmark_group("synchronizer");
    group.throughput(Throughput::Elements(1));

    group.bench_function("write", |b| {
        b.iter(|| {
            synchronizer
                .write(black_box(&data), Duration::from_nanos(10))
                .expect("failed to write data");
        })
    });

    group.bench_function("read", |b| {
        b.iter(|| {
            let archived = unsafe { synchronizer.read::<HelloWorld>().unwrap() };
            assert_eq!(archived.version, data.version);
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().with_profiler(PProfProfiler::new(100, pprof::criterion::Output::Protobuf));
    targets = bench_synchronizer
}
criterion_main!(benches);
