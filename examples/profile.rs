use std::{thread, time::Duration};

use hdrhistogram::{sync::Recorder, SyncHistogram};
use itertools::Itertools;
use quanta::Clock;
use rand_distr::Normal;
use tracing::{dispatcher, trace, trace_span, Dispatch};
use tracing_timing::{group::ByName, Builder, Histogram};

fn main() {
    let subscriber = Builder::default()
        .events(ByName)
        .build(|| Histogram::new_with_max(1_000_000, 2).unwrap());
    let downcaster = subscriber.downcaster();
    let dispatch = Dispatch::new(subscriber);

    let mut trace = Histogram::new_with_bounds(100, 8_000, 3)
        .unwrap()
        .into_sync();

    emulate_work(dispatch.clone(), trace.recorder());

    // get trace info first, because refresh in with_histograms will slow down record in trace!
    trace.refresh();

    let subscriber = downcaster.downcast(&dispatch).unwrap();
    subscriber.with_histograms(|histograms_by_span| {
        let request_histograms = histograms_by_span.get_mut("request").unwrap();

        println!("fast:");
        let fast = &mut request_histograms["event examples/profile.rs:58"];
        fast.refresh();
        print_histogram(fast);

        println!("\nslow:");
        let slow = &mut request_histograms["event examples/profile.rs:63"];
        slow.refresh();
        print_histogram(slow);
    });

    println!("\ntrace!:");
    print_histogram(&trace);
}

fn emulate_work(dispatch: Dispatch, mut recorder: Recorder<u64>) {
    thread::spawn(move || {
        use rand::prelude::*;
        let mut rng = rand::rng();
        let fast = Normal::<f64>::new(100_000.0, 50_000.0).unwrap();
        let slow = Normal::<f64>::new(500_000.0, 50_000.0).unwrap();
        let clock = Clock::new();
        dispatcher::with_default(&dispatch, || loop {
            let fast = Duration::from_nanos(fast.sample(&mut rng).max(0.0) as u64);
            let slow = Duration::from_nanos(slow.sample(&mut rng).max(0.0) as u64);
            trace_span!("request").in_scope(|| {
                thread::sleep(fast);
                let a = clock.start();
                trace!("fast");
                let b = clock.end();
                recorder.saturating_record(b - a);
                thread::sleep(slow);
                let a = clock.start();
                trace!("slow");
                let b = clock.end();
                recorder.saturating_record(b - a);
            });
        })
    });
    thread::sleep(Duration::from_secs(5));
}

fn print_histogram(histogram: &SyncHistogram<u64>) {
    println!(
        "count: {}, mean: {:.1}µs, p50: {}µs, p90: {}µs, p99: {}µs, p999: {}µs, max: {}µs",
        histogram.len(),
        histogram.mean() / 1000.0,
        histogram.value_at_quantile(0.5) / 1_000,
        histogram.value_at_quantile(0.9) / 1_000,
        histogram.value_at_quantile(0.99) / 1_000,
        histogram.value_at_quantile(0.999) / 1_000,
        histogram.max() / 1_000,
    );
    for value in histogram
        .iter_linear(25_000)
        .skip_while(|v| v.quantile() < 0.01)
        .take_while_inclusive(|v| v.quantile() <= 0.95)
    {
        let bucket = (value.value_iterated_to() + 1) / 1_000;
        let count = value.count_since_last_iteration() as f64 * 40.0 / histogram.len() as f64;
        let stars = "*".repeat(count.ceil() as usize);
        let percentile = value.percentile();
        println!("{bucket:4}µs | {stars:40} | {percentile:4.1}th %-ile",);
    }
}
