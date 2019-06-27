use tracing::*;
use tracing_timing::{Builder, Histogram};

fn main() {
    let s = Builder::from(|| Histogram::new_with_max(1_000_000, 2).unwrap()).build();
    let mut _type_of_s = if false { Some(&s) } else { None };
    let d = Dispatch::new(s);
    let d2 = d.clone();
    let mut trace = Histogram::<u64>::new_with_bounds(1_000, 16_000, 3)
        .unwrap()
        .into_sync();
    let mut r_trace = trace.recorder();
    std::thread::spawn(move || {
        use rand::prelude::*;
        let mut rng = thread_rng();
        let fast = rand::distributions::Normal::new(100_000.0, 50_000.0);
        let slow = rand::distributions::Normal::new(500_000.0, 50_000.0);
        let clock = quanta::Clock::new();
        dispatcher::with_default(&d2, || loop {
            let fast = std::time::Duration::from_nanos(fast.sample(&mut rng).max(0.0) as u64);
            let slow = std::time::Duration::from_nanos(slow.sample(&mut rng).max(0.0) as u64);
            trace_span!("request").in_scope(|| {
                std::thread::sleep(fast);
                let a = clock.now();
                trace!("fast");
                r_trace.saturating_record(clock.now() - a);
                std::thread::sleep(slow);
                let a = clock.now();
                trace!("slow");
                r_trace.saturating_record(clock.now() - a);
            });
        })
    });
    std::thread::sleep(std::time::Duration::from_secs(10));
    _type_of_s = d.downcast_ref();
    _type_of_s.unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("request").unwrap();
        assert_eq!(hs.len(), 2);

        hs.get_mut("fast").unwrap().refresh();
        hs.get_mut("slow").unwrap().refresh();

        println!("fast:");
        let h = &hs["fast"];
        for v in h
            .iter_linear(50_000)
            .take_while(|v| v.value_iterated_to() < 400_000)
        {
            println!(
                "{:4}µs | {}",
                (v.value_iterated_to() + 1) / 1_000,
                "*".repeat(
                    (v.count_since_last_iteration() as f64 * 40.0 / h.len() as f64).round()
                        as usize
                )
            );
        }

        println!("\nslow:");
        let h = &hs["slow"];
        for v in h
            .iter_linear(50_000)
            .skip_while(|v| v.value_iterated_to() < 400_000)
            .take_while(|v| v.value_iterated_to() < 800_000)
        {
            println!(
                "{:4}µs | {}",
                (v.value_iterated_to() + 1) / 1_000,
                "*".repeat(
                    (v.count_since_last_iteration() as f64 * 40.0 / h.len() as f64).round()
                        as usize
                )
            );
        }
    });

    trace.refresh();
    println!("\ntrace!:");
    for v in trace
        .iter_linear(1_000)
        .take_while(|v| v.value_iterated_to() < 16_000)
    {
        println!(
            "{:4}µs | {}",
            (v.value_iterated_to() + 1) / 1_000,
            "*".repeat(
                (v.count_since_last_iteration() as f64 * 40.0 / trace.len() as f64).round()
                    as usize
            )
        );
    }
}
