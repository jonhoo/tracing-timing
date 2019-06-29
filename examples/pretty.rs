use rand_distr::Normal;
use tracing::*;
use tracing_timing::{Builder, Histogram};

fn main() {
    let s = Builder::default().build(|| Histogram::new_with_bounds(10_000, 1_000_000, 3).unwrap());
    let sid = s.downcaster();

    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        use rand::prelude::*;
        let mut rng = thread_rng();
        let fast = Normal::<f64>::new(100_000.0, 50_000.0).unwrap();
        let slow = Normal::<f64>::new(500_000.0, 50_000.0).unwrap();
        dispatcher::with_default(&d2, || loop {
            let fast = std::time::Duration::from_nanos(fast.sample(&mut rng).max(0.0) as u64);
            let slow = std::time::Duration::from_nanos(slow.sample(&mut rng).max(0.0) as u64);
            trace_span!("request").in_scope(|| {
                std::thread::sleep(fast); // emulate some work
                trace!("fast");
                std::thread::sleep(slow); // emulate some more work
                trace!("slow");
            });
        })
    });
    std::thread::sleep(std::time::Duration::from_secs(15));

    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("request").unwrap();
        assert_eq!(hs.len(), 2);

        hs.get_mut("fast").unwrap().refresh();
        hs.get_mut("slow").unwrap().refresh();

        println!("fast:");
        let h = &hs["fast"];
        println!(
            "mean: {:.1}µs, p50: {}µs, p90: {}µs, p99: {}µs, p999: {}µs, max: {}µs",
            h.mean() / 1000.0,
            h.value_at_quantile(0.5) / 1_000,
            h.value_at_quantile(0.9) / 1_000,
            h.value_at_quantile(0.99) / 1_000,
            h.value_at_quantile(0.999) / 1_000,
            h.max() / 1_000,
        );
        for v in break_once(
            h.iter_linear(25_000).skip_while(|v| v.quantile() < 0.01),
            |v| v.quantile() > 0.95,
        ) {
            println!(
                "{:4}µs | {:40} | {:4.1}th %-ile",
                (v.value_iterated_to() + 1) / 1_000,
                "*".repeat(
                    (v.count_since_last_iteration() as f64 * 40.0 / h.len() as f64).ceil() as usize
                ),
                v.percentile(),
            );
        }

        println!("\nslow:");
        let h = &hs["slow"];
        println!(
            "mean: {:.1}µs, p50: {}µs, p90: {}µs, p99: {}µs, p999: {}µs, max: {}µs",
            h.mean() / 1000.0,
            h.value_at_quantile(0.5) / 1_000,
            h.value_at_quantile(0.9) / 1_000,
            h.value_at_quantile(0.99) / 1_000,
            h.value_at_quantile(0.999) / 1_000,
            h.max() / 1_000,
        );
        for v in break_once(
            h.iter_linear(25_000).skip_while(|v| v.quantile() < 0.01),
            |v| v.quantile() > 0.95,
        ) {
            println!(
                "{:4}µs | {:40} | {:4.1}th %-ile",
                (v.value_iterated_to() + 1) / 1_000,
                "*".repeat(
                    (v.count_since_last_iteration() as f64 * 40.0 / h.len() as f64).ceil() as usize
                ),
                v.percentile(),
            );
        }
    });
}

// until we have https://github.com/rust-lang/rust/issues/62208
fn break_once<I, F>(it: I, mut f: F) -> impl Iterator<Item = I::Item>
where
    I: IntoIterator,
    F: FnMut(&I::Item) -> bool,
{
    let mut got_true = false;
    it.into_iter().take_while(move |i| {
        if got_true {
            // we've already yielded when f was true
            return false;
        }
        if f(i) {
            // this must be the first time f returns true
            // we should yield i, and then no more
            got_true = true;
        }
        // f returned false, so we should keep yielding
        true
    })
}
