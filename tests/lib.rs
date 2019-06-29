use tracing::*;
use tracing_timing::*;

// NOTE: leave this test first! it'll break if it moves
#[test]
fn by_name() {
    let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap())
        .spans(group::ByName)
        .events(group::ByName)
        .build();
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!("event");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("foo").unwrap();
        assert_eq!(hs.len(), 1);
        #[cfg(windows)]
        assert!(hs.contains_key("event tests\\lib.rs:17"));
        #[cfg(not(windows))]
        assert!(hs.contains_key("event tests/lib.rs:17"));
    })
}

#[test]
fn by_target() {
    let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap())
        .spans(group::ByTarget)
        .events(group::ByTarget)
        .build();
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!(target: "e", "event");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("lib").unwrap();
        assert_eq!(hs.len(), 1);
        assert!(hs.contains_key("e"));
    })
}

#[test]
fn by_default() {
    let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap()).build();
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || loop {
            trace_span!("foo").in_scope(|| {
                std::thread::sleep(std::time::Duration::from_millis(10));
                trace!("fast");
                std::thread::sleep(std::time::Duration::from_millis(100));
                trace!("slow");
            })
        })
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("foo").unwrap();
        assert_eq!(hs.len(), 2);

        let h = &mut hs.get_mut("fast").unwrap();
        h.refresh();
        // ~= 10ms
        assert!(h.value_at_quantile(0.5) > 1_000_000);
        assert!(h.value_at_quantile(0.5) < 20_000_000);

        let h = &mut hs.get_mut("slow").unwrap();
        h.refresh();
        // ~= 100ms
        assert!(h.value_at_quantile(0.5) > 10_000_000);
        assert!(h.value_at_quantile(0.5) < 200_000_000);
    })
}

#[test]
fn by_field() {
    let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap())
        .spans(group::ByField::from("sf"))
        .events(group::ByField::from("ef"))
        .build();
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo", sf = "span").in_scope(|| {
                trace!({ ef = "event1" }, "fast");
                trace!({ ef = "event2" }, "slow");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("span").unwrap();
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("event1"));
        assert!(hs.contains_key("event2"));
    })
}

#[test]
fn dupe_span() {
    let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap()).build();
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let span = dispatcher::with_default(&d, || trace_span!("foo"));
    let d1 = d.clone();
    let span1 = span.clone();
    let jh1 = std::thread::spawn(move || {
        dispatcher::with_default(&d1, || {
            span1.in_scope(|| {
                trace!("thread1");
            })
        })
    });
    let span2 = span.clone();
    let d2 = d.clone();
    let jh2 = std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            span2.in_scope(|| {
                trace!("thread2");
            })
        })
    });
    drop(span);
    jh1.join().unwrap();
    jh2.join().unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("foo").unwrap();
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("thread1"));
        assert!(hs.contains_key("thread2"));
    })
}

#[test]
fn by_field_typed() {
    let s = Builder::from(|| Histogram::new_with_max(200_000_000, 1).unwrap())
        .events(group::ByField::from("f"))
        .build();
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!({ f = 1u64 }, "");
                trace!({ f = true }, "");
                trace!({ f = -1i64 }, "");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &mut hs.get_mut("foo").unwrap();
        assert_eq!(hs.len(), 3);
        assert!(hs.contains_key("1"));
        assert!(hs.contains_key("true"));
        assert!(hs.contains_key("-1"));
    })
}
