use std::{thread, time::Duration};

use hdrhistogram::SyncHistogram;
use itertools::Itertools;
use rand_distr::Normal;
use tracing::{dispatcher, trace, trace_span, Dispatch};
use tracing_timing::{Builder, Histogram, TimingSubscriber};

fn main() {
    let subscriber =
        Builder::default().build(|| Histogram::new_with_bounds(10_000, 1_000_000, 3).unwrap());
    let dispatch = Dispatch::new(subscriber);

    emulate_work(dispatch.clone());

    let subscriber = dispatch.downcast_ref::<TimingSubscriber>().unwrap();
    subscriber.with_histograms(|histograms_by_span| {
        let request_histograms = histograms_by_span.get_mut("request").unwrap();

        let fast = &mut request_histograms["fast"];
        fast.refresh();
        println!("fast:");
        print_histogram(fast);

        let slow = &mut request_histograms["slow"];
        slow.refresh();
        println!("\nslow:");
        print_histogram(slow);
    });
}

fn emulate_work(dispatch: Dispatch) {
    thread::spawn(move || {
        use rand::prelude::*;
        let mut rng = rand::rng();
        let fast = Normal::<f64>::new(100_000.0, 50_000.0).unwrap();
        let slow = Normal::<f64>::new(500_000.0, 50_000.0).unwrap();
        dispatcher::with_default(&dispatch, || loop {
            let fast = Duration::from_nanos(fast.sample(&mut rng).max(0.0) as u64);
            let slow = Duration::from_nanos(slow.sample(&mut rng).max(0.0) as u64);
            trace_span!("request").in_scope(|| {
                thread::sleep(fast); // emulate some work
                trace!("fast");
                thread::sleep(slow); // emulate some more work
                trace!("slow");
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
