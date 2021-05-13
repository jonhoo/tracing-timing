use tracing::*;
use tracing_timing::*;

// NOTE: leave this test first! it'll break if it moves
#[test]
fn by_name() {
    let s = Builder::default()
        .spans(group::ByName)
        .events(group::ByName)
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
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
        let hs = &hs["foo"];
        assert_eq!(hs.len(), 1);
        #[cfg(windows)]
        assert!(hs.contains_key("event tests\\subscriber.rs:17"));
        #[cfg(not(windows))]
        assert!(hs.contains_key("event tests/subscriber.rs:17"));
    })
}

#[test]
fn downcaster() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let sid2 = sid.clone();
    let d = Dispatch::new(s);

    // can downcast through a Downcaster
    sid.downcast(&d).unwrap();

    // can downcast through a cloned Downcaster
    sid2.downcast(&d).unwrap();
}

#[test]
fn by_target() {
    let s = Builder::default()
        .spans(group::ByTarget)
        .events(group::ByTarget)
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
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
        let hs = &hs["subscriber"];
        assert_eq!(hs.len(), 1);
        assert!(hs.contains_key("e"));
    })
}

#[test]
fn by_default() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!("fast");
                trace!("slow");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs["foo"];
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("fast"));
        assert!(hs.contains_key("slow"));
    })
}

#[test]
fn by_field() {
    let s = Builder::default()
        .spans(group::ByField::from("sf"))
        .events(group::ByField::from("ef"))
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
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
        let hs = &hs["span"];
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("event1"));
        assert!(hs.contains_key("event2"));
    })
}

#[test]
fn custom_time() {
    let (time, mock) = quanta::Clock::mock();
    let s = Builder::default()
        .time(time)
        .build(|| Histogram::new_with_bounds(1, 16, 3).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("span").in_scope(|| {
                mock.increment(1);
                trace!("event1");
                mock.increment(2);
                trace!("event2");
                mock.decrement(1);
                trace!("event3");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs["span"];
        assert_eq!(hs.len(), 3);
        assert!(hs.contains_key("event1"));
        assert!(hs.contains_key("event2"));
        assert_eq!(hs["event1"].max(), 1);
        assert_eq!(hs["event2"].max(), 2);
        // event3 "raced" with event2, and ended up recording "negative" time
        // in this case, event3's sample should be dropped.
        assert!(hs.contains_key("event3"));
        assert_eq!(hs["event3"].len(), 0);
    })
}

#[test]
fn event_order() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("span").in_scope(|| {
                trace!("first");
                trace!("second");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs["span"];
        let mut it = hs.keys();
        assert_eq!(it.next().map(|s| &**s), Some("first"));
        assert_eq!(it.next().map(|s| &**s), Some("second"));
        assert_eq!(it.next(), None);
    })
}

#[test]
fn samples() {
    let s = Builder::default().build(|| Histogram::new_with_max(1_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    let n = 100;
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            for _ in 0..n {
                trace_span!("foo").in_scope(|| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    trace!("fast");
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    trace!("slow");
                })
            }
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs["foo"];
        assert_eq!(hs.len(), 2);

        let h = &hs["fast"];
        // ~= 100µs
        assert_eq!(h.len(), n);
        assert!(h.value_at_quantile(0.5) > 200_000);
        assert!(h.value_at_quantile(0.5) < 5_000_000);

        let h = &hs["slow"];
        // ~= 500µs
        assert_eq!(h.len(), n);
        assert!(h.value_at_quantile(0.5) > 1_000_000);
        assert!(h.value_at_quantile(0.5) < 25_000_000);
    })
}

#[test]
fn dupe_span() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
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
        let hs = &hs["foo"];
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("thread1"));
        assert!(hs.contains_key("thread2"));
    })
}

#[test]
fn same_event_two_threads() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d1 = d.clone();
    let jh1 = std::thread::spawn(move || {
        dispatcher::with_default(&d1, || {
            trace_span!("span").in_scope(|| {
                trace!("event1");
                trace!("event2");
            })
        })
    });
    let d2 = d.clone();
    let jh2 = std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("span").in_scope(|| {
                trace!("event1");
                trace!("event2");
            })
        })
    });
    jh1.join().unwrap();
    jh2.join().unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs["span"];
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("event1"));
        assert!(hs.contains_key("event2"));
    })
}

#[test]
fn by_field_typed() {
    let s = Builder::default()
        .events(group::ByField::from("f"))
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
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
        let hs = &hs["foo"];
        assert_eq!(hs.len(), 3);
        assert!(hs.contains_key("1"));
        assert!(hs.contains_key("true"));
        assert!(hs.contains_key("-1"));
    })
}

#[test]
fn informed_histogram_constructor() {
    let s = Builder::default().build_informed(|s: &_, e: &_| {
        assert_eq!(*s, "span");
        assert_eq!(e, "event");
        Histogram::new_with_max(200_000_000, 1).unwrap()
    });
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("span").in_scope(|| {
                trace!("event");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs["span"];
        assert_eq!(hs.len(), 1);
        assert!(hs.contains_key("event"));
    })
}

#[test]
fn by_closure() {
    let s = Builder::default()
        .spans(|_: &span::Attributes| ())
        .events(|_: &Event| ())
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!({ f = 1u64 }, "");
                trace!("foo");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        let hs = &hs[&()];
        assert_eq!(hs.len(), 1);
        assert!(hs.contains_key(&()));
    })
}

#[test]
fn free_standing_event() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace!("event");
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        // free-standing events should not be recorded
        assert_eq!(hs.len(), 0);
    })
}

#[test]
fn nested() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                std::thread::sleep(std::time::Duration::from_millis(1));
                trace_span!("bar").in_scope(|| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    trace_span!("baz").in_scope(|| {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        trace!("event1");
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        trace!("event2");
                        // now do an event twice so we check the re-use path
                        trace!("event3");
                        trace!("event3");
                    })
                })
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 3);
        for &s in &["baz", "bar", "foo"] {
            assert!(hs.contains_key(s));
            assert!(hs[s].contains_key("event1"));
            assert!(hs[s].contains_key("event2"));
            assert_eq!(hs[s].len(), 3);
            for &e in &["event1", "event2"] {
                assert_eq!(hs[s][e].len(), 1);
            }
            assert_eq!(hs[s]["event3"].len(), 2);
        }

        // timing should differ between the different spans for event1
        // since it is measured relative to span start time
        let foo_e1 = &hs["foo"]["event1"].max();
        let bar_e1 = &hs["bar"]["event1"].max();
        let baz_e1 = &hs["baz"]["event1"].max();
        assert!(foo_e1 > bar_e1);
        assert!(bar_e1 > baz_e1);
        // for event2 however, they should all be relative to event1
        // note, however, that event1.end is _later_ for foo than for bar/baz
        // because events are recorded for spans in reverse span order
        let foo_e2 = &hs["foo"]["event2"].max();
        let bar_e2 = &hs["bar"]["event2"].max();
        let baz_e2 = &hs["baz"]["event2"].max();
        assert!(foo_e2 <= bar_e2);
        assert!(foo_e2 <= baz_e2);
        assert!(bar_e2 <= baz_e2);
    })
}

#[test]
fn nested_diff() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!("foo_event");
                std::thread::sleep(std::time::Duration::from_millis(1));
                trace_span!("bar").in_scope(|| {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    trace!("bar_event");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    trace_span!("baz").in_scope(|| {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        trace!("baz_event");
                    })
                })
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 3);
        assert!(hs.contains_key("foo"));
        assert!(hs.contains_key("bar"));
        assert!(hs.contains_key("baz"));

        // all spans see baz_event
        assert!(hs["foo"].contains_key("baz_event"));
        assert!(hs["bar"].contains_key("baz_event"));
        assert!(hs["baz"].contains_key("baz_event"));
        // baz does not see any the other events, but the others do
        assert!(hs["foo"].contains_key("bar_event"));
        assert!(hs["bar"].contains_key("bar_event"));
        assert_eq!(hs["baz"].len(), 1);
        // bar does not see foo_event, but foo does
        assert!(hs["foo"].contains_key("foo_event"));
        assert_eq!(hs["bar"].len(), 2);
        // foo doesn't somehow see any other events
        assert_eq!(hs["foo"].len(), 3);

        // in foo, bar_event should be measured relative to foo_event
        // in bar, bar_event should be measured relative to the start of bar
        // therefore, bar should see a lower time for bar_event than foo should
        let foo_bar_e = &hs["foo"]["bar_event"].max();
        let bar_bar_e = &hs["bar"]["bar_event"].max();
        assert!(foo_bar_e > bar_bar_e);

        // in both foo and bar, baz_event should be measured relative to bar_event
        // in baz, baz_event should be measured relative to the start of baz
        // therefore, baz should see a lower time for baz_event than foo or bar
        // and foo and bar should see the _same_ time.
        // note, however, that bar_event.end is _later_ for foo than for bar
        // because events are recorded for spans in reverse span order, so
        // baz will appear to have happened _sooner_ to foo.
        let foo_baz_e = &hs["foo"]["baz_event"].max();
        let bar_baz_e = &hs["bar"]["baz_event"].max();
        let baz_baz_e = &hs["baz"]["baz_event"].max();
        assert!(foo_baz_e <= bar_baz_e);
        assert!(foo_baz_e > baz_baz_e);
    })
}

#[test]
fn nested_no_bubble() {
    let s = Builder::default()
        .no_span_recursion()
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!("foo_start");
                std::thread::sleep(std::time::Duration::from_millis(1));
                trace_span!("bar").in_scope(|| {
                    trace!("bar_start");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    trace!("bar_end");
                });
                trace!("foo_end");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("foo"));
        assert!(hs.contains_key("bar"));

        // foo membership
        assert!(hs["foo"].contains_key("foo_start"));
        assert!(hs["foo"].contains_key("foo_end"));
        assert!(!hs["foo"].contains_key("bar_start"));
        assert!(!hs["foo"].contains_key("bar_end"));

        // bar membership
        assert!(hs["bar"].contains_key("bar_start"));
        assert!(hs["bar"].contains_key("bar_end"));

        // because of no_span_recursion(), bar_end - bar_start < foo_end - foo_start
        let bar_t = hs["bar"]["bar_end"].max();
        let foo_t = hs["foo"]["foo_end"].max();
        assert!(foo_t > bar_t);
    })
}

#[test]
fn nested_bubble() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                trace!("foo_start");
                std::thread::sleep(std::time::Duration::from_millis(1));
                trace_span!("bar").in_scope(|| {
                    trace!("bar_start");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    trace!("bar_end");
                });
                trace!("foo_end");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("foo"));
        assert!(hs.contains_key("bar"));

        // foo membership
        assert!(hs["foo"].contains_key("foo_start"));
        assert!(hs["foo"].contains_key("foo_end"));
        // foo span additionally contains bar events because of no no_span_recursion()
        assert!(hs["foo"].contains_key("bar_start"));
        assert!(hs["foo"].contains_key("bar_end"));

        // bar membership
        assert!(hs["bar"].contains_key("bar_start"));
        assert!(hs["bar"].contains_key("bar_end"));

        // because of no no_span_recursion(), bar_end - bar_start < foo_end - foo_start
        let bar_t = hs["bar"]["bar_end"].max();
        let foo_t = hs["foo"]["foo_end"].max();
        assert!(foo_t < bar_t);
    })
}

#[test]
fn span_close_event() {
    let s = Builder::default()
        .span_close_events()
        .build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            trace_span!("foo").in_scope(|| {
                std::thread::sleep(std::time::Duration::from_millis(1));
                trace!("foo_start");
                trace_span!("bar").in_scope(|| {
                    trace!("bar_end");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                });
                trace!("foo_end");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 2);
        assert!(hs.contains_key("foo"));
        assert!(hs.contains_key("bar"));

        // foo membership
        assert!(hs["foo"].contains_key("foo_start"));
        assert!(hs["foo"].contains_key("foo_end"));
        assert!(hs["foo"].contains_key("close"));

        // bar membership
        assert!(hs["bar"].contains_key("bar_end"));
        assert!(hs["bar"].contains_key("close"));

        let foo_start = hs["foo"]["foo_start"].max();
        let bar_end = hs["bar"]["bar_end"].max();
        assert!(foo_start > bar_end);

        let foo_end = hs["foo"]["foo_end"].max();
        let bar_close = hs["bar"]["close"].max();
        assert!(bar_close > foo_end);
    })
}

#[test]
fn explicit_span_parent() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            let s1 = trace_span!("foo");
            std::thread::sleep(std::time::Duration::from_millis(1));
            trace_span!(parent: &s1, "bar").in_scope(|| {
                trace!("event");
            })
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().force_synchronize();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 2);
        for &s in &["bar", "foo"] {
            assert!(hs.contains_key(s));
            assert!(hs[s].contains_key("event"));
            assert_eq!(hs[s].len(), 1);
            assert_eq!(hs[s]["event"].len(), 1);
        }

        // timing should differ between the different spans for event
        // since it is measured relative to span start time
        let foo_e = &hs["foo"]["event"].max();
        let bar_e = &hs["bar"]["event"].max();
        assert!(foo_e > bar_e);
    })
}

#[test]
fn explicit_event_parent() {
    let s = Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap());
    let sid = s.downcaster();
    let d = Dispatch::new(s);
    let d2 = d.clone();
    std::thread::spawn(move || {
        dispatcher::with_default(&d2, || {
            let span = trace_span!("foo");
            trace!(parent: &span, "event");
        })
    })
    .join()
    .unwrap();
    sid.downcast(&d).unwrap().with_histograms(|hs| {
        assert_eq!(hs.len(), 1);
        assert!(hs.contains_key("foo"));
        assert!(hs["foo"].contains_key("event"));
        assert_eq!(hs["foo"].len(), 1);
    })
}

#[test]
fn tracks_current_span() {
    let d = Dispatch::new(
        Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap()),
    );
    dispatcher::with_default(&d, || {
        let span = trace_span!("span");
        span.in_scope(|| {
            assert_eq!(span, Span::current());
        })
    });
}

#[test]
fn tracks_current_span_deep() {
    let d = Dispatch::new(
        Builder::default().build(|| Histogram::new_with_max(200_000_000, 1).unwrap()),
    );
    dispatcher::with_default(&d, || {
        trace_span!("span1").in_scope(|| {
            trace_span!("span2").in_scope(|| {
                let span = trace_span!("span3");
                span.in_scope(|| {
                    assert_eq!(span, Span::current());
                })
            });
        });
    });
}
