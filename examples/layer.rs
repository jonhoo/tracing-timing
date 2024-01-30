use rand_distr::Normal;
use tracing::*;
use tracing_subscriber::{fmt, layer::SubscriberExt, Layer};
use tracing_timing::{Builder, Histogram};

fn main() {
    let timing_layer =
        Builder::default().layer(|| Histogram::new_with_bounds(1_000_000, 100_000_000, 3).unwrap());
    let downcaster = timing_layer.downcaster();

    let subscriber = tracing_subscriber::registry()
        .with(
            fmt::Layer::new()
                .with_writer(std::io::stdout)
                .with_filter(tracing::metadata::LevelFilter::INFO),
        )
        .with(timing_layer);

    let dispatch = Dispatch::new(subscriber);
    tracing::dispatcher::set_global_default(dispatch.clone()).unwrap();

    std::thread::spawn(move || {
        info!(
            "{:?}: Logging tracing-timing results",
            std::thread::current().id()
        );
        loop {
            // wait for first traces
            std::thread::sleep(std::time::Duration::from_secs(1));
            downcaster
                .downcast(&dispatch)
                .unwrap()
                .with_histograms(|hs| {
                    for (span_group, hs) in &mut *hs {
                        for (event_group, h) in hs {
                            // make sure we see the latest samples:
                            h.refresh();
                            info!(
                                "[{}:{}] Mean: {:.1}ms, Min: {:.1}ms, Max: {:.1}ms",
                                span_group,
                                event_group,
                                h.mean() as f32 / 1e6,
                                h.min() as f32 / 1e6,
                                h.max() as f32 / 1e6,
                            )
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(5));
                });
        }
    });

    std::thread::spawn(move || {
        info!(
            "{:?}: Emulating work and creating traces",
            std::thread::current().id()
        );
        use rand::prelude::*;
        let mut rng = thread_rng();
        let fast = Normal::<f64>::new(10_000_000.0, 5_000_000.0).unwrap();
        let slow = Normal::<f64>::new(50_000_000.0, 5_000_000.0).unwrap();
        loop {
            let fast = std::time::Duration::from_nanos(fast.sample(&mut rng).max(0.0) as u64);
            let slow = std::time::Duration::from_nanos(slow.sample(&mut rng).max(0.0) as u64);
            trace_span!("request").in_scope(|| {
                std::thread::sleep(fast); // emulate some work
                trace!("fast");
                std::thread::sleep(slow); // emulate some more work
                trace!("slow");
            });
        }
    });

    std::thread::sleep(std::time::Duration::from_secs(15));
}
