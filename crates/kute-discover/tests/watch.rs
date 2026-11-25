//! Live discovery by event time (v0.7 specification, C2). All data here is
//! synthetic and seeded.

use kute_discover::live::{Live, LiveParams};
use kute_discover::watch::{Watch, WatchParams, WindowReport};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const CYCLE: [&str; 4] = ["MOTOR_START", "CURRENT_HIGH", "MOTOR_TRIP", "OPERATOR_ACK"];
const NOISE: [&str; 5] = ["IDLE", "TICK", "DOOR", "FAN", "LAMP"];

type Event = (String, String, i64);

/// `devices` devices emitting noise and, now and then, the cycle; one
/// event per step, `per_tick` events sharing each time. With `per_hour`
/// the device names differ in each 3,600 ticks, so no sequence crosses an
/// hour.
fn stream(seed: u64, events: usize, devices: u64, per_tick: usize, per_hour: bool) -> Vec<Event> {
    let mut rng = Rng(seed);
    let mut pending: Vec<Vec<&str>> = vec![Vec::new(); devices as usize];
    let mut out = Vec::with_capacity(events);
    for i in 0..events {
        let time = (i / per_tick) as i64;
        let device = rng.below(devices) as usize;
        if per_hour && i % per_tick == 0 && time % 3600 == 0 {
            pending.iter_mut().for_each(Vec::clear);
        }
        if pending[device].is_empty() {
            pending[device] = if rng.below(4) == 0 { CYCLE.iter().rev().copied().collect() } else { vec![NOISE[rng.below(5) as usize]] };
        }
        let name = if per_hour { format!("device-{device}@{}", time / 3600) } else { format!("device-{device}") };
        out.push((name, pending[device].pop().unwrap().to_string(), time));
    }
    out
}

fn params(pane_ticks: i64, panes: usize, lateness: i64) -> WatchParams {
    WatchParams { pane_ticks, panes, lateness, max_patterns: 10_000, ..WatchParams::default() }
}

fn run(params: WatchParams, events: &[Event]) -> Vec<WindowReport> {
    let mut watch = Watch::new(params).unwrap();
    let mut reports = Vec::new();
    for (entity, symbol, time) in events {
        reports.extend(watch.observe(entity, symbol, *time));
    }
    reports.extend(watch.finish().0);
    reports
}

fn live_patterns(events: &[Event]) -> Vec<(Vec<String>, u64)> {
    let mut live = Live::new(LiveParams::default()).unwrap();
    for (entity, symbol, _) in events {
        live.observe(entity, symbol);
    }
    let report = live.report(5, 1.0, 10_000);
    assert!(report.exact);
    let mut found: Vec<(Vec<String>, u64)> = report.patterns.into_iter().map(|p| (p.symbols, p.support)).collect();
    found.sort();
    found
}

fn window_patterns(report: &WindowReport) -> Vec<(Vec<String>, u64)> {
    let mut found: Vec<(Vec<String>, u64)> = report.patterns.iter().map(|p| (p.symbols.clone(), p.support)).collect();
    found.sort();
    found
}

#[test]
fn one_window_over_everything_is_the_stream_without_windows() {
    let events = stream(1, 30_000, 40, 3, false);
    let reports = run(params(1 << 40, 1, 0), &events);
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.exact && report.at_end, "{report:?}");
    assert_eq!(report.events, 30_000);
    let expected = live_patterns(&events);
    assert!(expected.iter().any(|(s, _)| s == &CYCLE.map(String::from)));
    assert_eq!(window_patterns(report), expected);
}

#[test]
fn each_window_holds_what_its_own_events_hold_tumbling_and_sliding() {
    // Six hours, ten events a second; device names change with the hour.
    let events = stream(2, 6 * 36_000, 30, 10, true);
    let hour = |h: i64| -> Vec<Event> { events.iter().filter(|e| e.2 / 3600 == h).cloned().collect() };

    let tumbling = run(params(3600, 1, 0), &events);
    assert_eq!(tumbling.len(), 6);
    for (h, report) in tumbling.iter().enumerate() {
        assert_eq!((report.start, report.end), (h as i64 * 3600, (h as i64 + 1) * 3600));
        assert!(report.exact);
        assert_eq!(report.at_end, h == 5);
        assert_eq!(window_patterns(report), live_patterns(&hour(h as i64)), "hour {h}");
    }

    // Three-hour windows moving an hour at a time.
    let sliding = run(params(3600, 3, 0), &events);
    assert_eq!(sliding.len(), 6);
    for (h, report) in sliding.iter().enumerate() {
        let h = h as i64;
        assert_eq!((report.start, report.end), ((h - 2) * 3600, (h + 1) * 3600));
        let inside: Vec<Event> = (h - 2..=h).filter(|x| *x >= 0).flat_map(hour).collect();
        assert_eq!(report.events, inside.len() as u64);
        assert_eq!(window_patterns(report), live_patterns(&inside), "window ending with hour {h}");
    }
}

#[test]
fn arrivals_out_of_order_within_the_lateness_count_as_if_in_order() {
    let events = stream(3, 60_000, 40, 5, false);
    // Each event delayed by up to 20 ticks' worth of arrivals.
    let mut rng = Rng(77);
    let mut keyed: Vec<(i64, usize)> = events.iter().enumerate().map(|(i, e)| (e.2 + rng.below(21) as i64, i)).collect();
    keyed.sort();
    let shuffled: Vec<Event> = keyed.iter().map(|&(_, i)| events[i].clone()).collect();
    assert_ne!(shuffled, events);
    // What "in order" means: by time, equal times in order of arrival.
    let mut sorted = shuffled.clone();
    sorted.sort_by_key(|e| e.2);

    // The bytes of state differ, since one of the two holds a buffer.
    let run = |p: WatchParams, events: &[Event]| -> Vec<WindowReport> {
        let mut reports = run(p, events);
        reports.iter_mut().for_each(|r| r.state_bytes = 0);
        reports
    };
    let in_order = run(params(2000, 2, 0), &sorted);
    let out_of_order = run(params(2000, 2, 25), &shuffled);
    assert!(in_order.len() >= 5);
    assert!(out_of_order.iter().all(|r| r.late_dropped == 0 && r.forced_releases == 0 && r.exact));
    assert_eq!(out_of_order, in_order);
    // And it is a function of the arrivals: the same again gives the same.
    assert_eq!(run(params(2000, 2, 25), &shuffled), out_of_order);
}

#[test]
fn what_is_too_late_is_dropped_and_what_overflows_is_released_and_both_are_counted() {
    let events = stream(4, 20_000, 10, 2, false);
    // No lateness allowed, one event in five arrives 50 ticks behind.
    let mut rng = Rng(5);
    let mut arrivals = Vec::new();
    let mut held: Vec<Event> = Vec::new();
    for (i, e) in events.iter().enumerate() {
        if rng.below(5) == 0 {
            held.push(e.clone());
        } else {
            arrivals.push(e.clone());
        }
        if i % 100 == 99 {
            arrivals.append(&mut held);
        }
    }
    let mut watch = Watch::new(params(1000, 1, 0)).unwrap();
    let mut reports = Vec::new();
    for (entity, symbol, time) in &arrivals {
        reports.extend(watch.observe(entity, symbol, *time));
    }
    let (rest, totals) = watch.finish();
    reports.extend(rest);
    assert!(totals.late_dropped > 1000, "{totals:?}");
    assert_eq!(totals.arrived, 20_000);
    assert_eq!(totals.counted + totals.late_dropped, totals.arrived);
    assert_eq!(reports.iter().map(|r| r.late_dropped).sum::<u64>(), totals.late_dropped);
    assert_eq!(reports.iter().map(|r| r.events).sum::<u64>(), totals.counted);
    assert!(reports.iter().any(|r| !r.exact));

    // A lateness that would hold 500 events, in a buffer of 50: events are
    // released early, in time order all the same, and nothing is lost
    // when the arrivals are in order.
    let tight = WatchParams { max_buffered: 50, ..params(1000, 1, 250) };
    let mut watch = Watch::new(tight).unwrap();
    for (entity, symbol, time) in &events {
        watch.observe(entity, symbol, *time);
        assert!(watch.buffered() <= 50);
    }
    let (_, totals) = watch.finish();
    assert!(totals.forced_releases > 5_000, "{totals:?}");
    assert_eq!((totals.counted, totals.late_dropped), (20_000, 0));
}

#[test]
fn old_windows_are_forgotten_and_state_stays_inside_its_bound() {
    // Small bounds, pressed from every side: a new entity for every few
    // events, symbols never seen before throughout, and the cycle only in
    // the first two hours.
    let tight = WatchParams {
        live: LiveParams { max_length: 4, max_symbols: 200, max_symbol_bytes: 32, max_candidates: 500, max_entities: 300 },
        pane_ticks: 3600,
        panes: 2,
        lateness: 5,
        max_buffered: 1000,
        min_support: 20,
        min_cohesion_bits: 1.0,
        max_patterns: 50,
        end_symbols: Vec::new(),
        idle_ticks: None,
    };
    let bound = tight.state_bytes_bound();
    let mut watch = Watch::new(tight).unwrap();
    let mut rng = Rng(9);
    let mut reports = Vec::new();
    let mut step = 0usize;
    for i in 0..400_000u64 {
        let time = (i / 20) as i64; // 20 events a second, five and a half hours
        let (entity, symbol) = if time < 7200 && i % 3 == 0 {
            step += 1;
            ("press-1".to_string(), CYCLE[step % 4].to_string())
        } else if rng.below(10) == 0 {
            (format!("visitor-{i}"), format!("NEVER_SEEN_{i}"))
        } else {
            (format!("sensor-{}", rng.below(5000)), NOISE[rng.below(5) as usize].to_string())
        };
        reports.extend(watch.observe(&entity, &symbol, time));
    }
    let (rest, totals) = watch.finish();
    reports.extend(rest);
    assert_eq!(reports.len(), 6);
    let cycle = CYCLE.map(String::from).to_vec();
    let has_cycle = |r: &WindowReport| r.patterns.iter().any(|p| p.symbols == cycle);
    assert!(has_cycle(&reports[0]) && has_cycle(&reports[1]) && has_cycle(&reports[2]));
    // The window of hours 2 and 3 no longer holds hours 0 and 1.
    assert!(!has_cycle(&reports[3]) && !has_cycle(&reports[5]));
    assert!(reports[3].vanished.contains(&cycle));
    assert_eq!(reports[3].previous_start, Some(reports[2].start));
    for report in &reports {
        assert!(report.state_bytes <= bound, "{} > {bound}", report.state_bytes);
        assert_eq!(report.state_bytes_bound, bound);
        assert!(!report.exact);
        assert!(report.alphabet <= 200 && report.entities_tracked <= 300);
        assert!(report.sequences_held.iter().all(|&held| held <= 2 * 500));
        assert!(report.events_folded > 0 && report.entities_evicted > 0);
    }
    assert_eq!(totals.counted, 400_000);
}

#[test]
fn a_restored_watch_goes_on_to_the_same_reports() {
    let events = stream(6, 80_000, 25, 4, false);
    let mut rng = Rng(11);
    let mut keyed: Vec<(i64, usize)> = events.iter().enumerate().map(|(i, e)| (e.2 + rng.below(8) as i64, i)).collect();
    keyed.sort();
    let arrivals: Vec<Event> = keyed.iter().map(|&(_, i)| events[i].clone()).collect();
    let p = params(1500, 3, 10);
    let whole = run(p.clone(), &arrivals);

    for cut in [1usize, 777, 30_001, 79_999] {
        let mut first = Watch::new(p.clone()).unwrap();
        let mut reports = Vec::new();
        for (entity, symbol, time) in &arrivals[..cut] {
            reports.extend(first.observe(entity, symbol, *time));
        }
        let saved = first.checkpoint().unwrap();
        assert_eq!(saved, first.clone().checkpoint().unwrap());
        drop(first);
        let mut second = Watch::restore(&saved).unwrap();
        assert_eq!(second.checkpoint().unwrap(), saved);
        assert_eq!(second.totals().arrived, cut as u64);
        for (entity, symbol, time) in &arrivals[cut..] {
            reports.extend(second.observe(entity, symbol, *time));
        }
        reports.extend(second.finish().0);
        assert_eq!(reports, whole, "cut at {cut}");
        assert!(whole.len() >= 10);
    }
}

#[test]
fn a_checkpoint_that_lies_is_refused() {
    let events = stream(7, 5_000, 10, 2, false);
    let mut watch = Watch::new(params(500, 2, 20)).unwrap();
    for (entity, symbol, time) in &events {
        watch.observe(entity, symbol, *time);
    }
    let saved: serde_json::Value = serde_json::from_slice(&watch.checkpoint().unwrap()).unwrap();
    assert!(Watch::restore(&serde_json::to_vec(&saved).unwrap()).is_ok());
    type Lie = Box<dyn Fn(&mut serde_json::Value)>;
    let lies: Vec<(&str, Lie)> = vec![
        ("format", Box::new(|v| v["format"] = 9.into())),
        ("panes", Box::new(|v| v["params"]["panes"] = 1.into())),
        ("pane_ticks", Box::new(|v| v["params"]["pane_ticks"] = 0.into())),
        ("max_candidates", Box::new(|v| v["params"]["max_candidates"] = 1.into())),
        ("count", Box::new(|v| v["panes"][0]["tables"][0]["counts"][0][1] = (u64::MAX / 2).into())),
        ("offered", Box::new(|v| v["panes"][0]["tables"][1]["offered"] = u64::MAX.into())),
        ("decrements", Box::new(|v| v["panes"][0]["tables"][1]["decrements"] = 5.into())),
        ("events", Box::new(|v| v["panes"][0]["events"] = u64::MAX.into())),
        ("arrived", Box::new(|v| v["totals"]["arrived"] = u64::MAX.into())),
        ("symbol id", Box::new(|v| v["buffer"][0][3] = 4_000_000.into())),
        ("buffered before the watermark", Box::new(|v| v["buffer"][0][0] = (-5).into())),
        ("watermark", Box::new(|v| v["watermark"] = i64::MAX.into())),
        ("pane order", Box::new(|v| v["panes"][1]["index"] = v["panes"][0]["index"].clone())),
        ("pane span", Box::new(|v| v["panes"][0]["index"] = (-900).into())),
        ("a pane after the watermark", Box::new(|v| {
            let last = v["panes"].as_array().unwrap().len() - 1;
            v["panes"][last]["index"] = (v["panes"][last]["index"].as_i64().unwrap() + 1).into();
            v["panes"].as_array_mut().unwrap().remove(0);
        })),
        ("tail", Box::new(|v| v["tails"][0][1] = serde_json::json!([1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]))),
        ("names", Box::new(|v| v["names"][0] = v["names"][1].clone())),
    ];
    for (what, lie) in lies {
        let mut changed = saved.clone();
        lie(&mut changed);
        assert_ne!(changed, saved, "{what}");
        assert!(Watch::restore(&serde_json::to_vec(&changed).unwrap()).is_err(), "{what} was accepted");
    }
    assert!(Watch::restore(b"{}").is_err());
}

#[test]
fn times_at_the_ends_of_the_range_and_bad_parameters_do_no_harm() {
    let mut watch = Watch::new(params(7, 3, 100)).unwrap();
    let mut reports = Vec::new();
    for (i, time) in [i64::MIN, i64::MIN + 1, -8, -7, -1, 0, 6, 7, i64::MAX - 1, i64::MAX, i64::MIN, 0].into_iter().enumerate() {
        reports.extend(watch.observe("e", &format!("s{}", i % 3), time));
    }
    let saved = watch.checkpoint().unwrap();
    assert!(Watch::restore(&saved).is_ok());
    let (rest, totals) = watch.finish();
    reports.extend(rest);
    assert_eq!(totals.late_dropped, 2);
    assert_eq!(totals.counted, 10);
    assert!(reports.iter().all(|r| r.start <= r.end));
    // Negative times fall in panes that start below them.
    assert!(reports.iter().any(|r| r.end == -7) && reports.iter().any(|r| r.end == 0));

    for bad in [
        WatchParams { pane_ticks: 0, ..WatchParams::default() },
        WatchParams { panes: 0, ..WatchParams::default() },
        WatchParams { panes: 65, ..WatchParams::default() },
        WatchParams { lateness: -1, ..WatchParams::default() },
        WatchParams { max_buffered: 0, ..WatchParams::default() },
        WatchParams { min_cohesion_bits: f64::NAN, ..WatchParams::default() },
    ] {
        assert!(Watch::new(bad).is_err());
    }
}

#[test]
fn an_ending_event_frees_its_entity_so_that_long_lived_ones_are_not_evicted() {
    // One job runs the whole time, a step every 40 ticks. Short jobs come
    // and go around it, three events each, never to be heard from again.
    let mut events: Vec<Event> = Vec::new();
    for t in 0..4000i64 {
        let job = t / 4;
        match t % 4 {
            0 => events.push((format!("job-{job}"), "START".into(), t)),
            1 => events.push((format!("job-{job}"), "WORK".into(), t)),
            2 => events.push((format!("job-{job}"), "END".into(), t)),
            _ if t % 40 == 3 => events.push(("long".into(), if (t / 40) % 2 == 0 { "PING".into() } else { "PONG".into() }, t)),
            _ => {}
        }
    }
    let bounds = kute_discover::live::LiveParams { max_entities: 8, ..Default::default() };
    let base = WatchParams { live: bounds, pane_ticks: 1_000_000, max_patterns: 10_000, ..WatchParams::default() };
    let support = |reports: &[WindowReport], symbols: &[&str]| reports.last().unwrap().patterns.iter().find(|p| p.symbols == symbols).map_or(0, |p| p.support);

    // Without an ending event the finished jobs hold the places, and the
    // long job, seen least recently, is the one pushed out between steps.
    let without = run(base.clone(), &events);
    assert!(without.last().unwrap().entities_evicted > 0);
    assert!(support(&without, &["PING", "PONG"]) < 40, "{}", support(&without, &["PING", "PONG"]));

    // With it nothing is evicted and every step of the long job is joined
    // to the one before.
    let mut watch = Watch::new(WatchParams { end_symbols: vec!["END".into()], ..base.clone() }).unwrap();
    let mut with = Vec::new();
    for (entity, symbol, time) in &events {
        with.extend(watch.observe(entity, symbol, *time));
    }
    let (rest, totals) = watch.finish();
    with.extend(rest);
    assert_eq!(with.last().unwrap().entities_evicted, 0);
    assert!(with.last().unwrap().exact);
    assert_eq!(totals.ended, 1000);
    assert_eq!(support(&with, &["PING", "PONG"]), 50);
    assert_eq!(support(&with, &["START", "WORK", "END"]), 1000);
    // The ending event is the last of its sequence: nothing runs on from it.
    assert_eq!(support(&with, &["END", "START"]), 0);

    // Arriving out of order within the lateness, the end still comes last:
    // it is applied in time order, not on arrival.
    let mut shuffled = events.clone();
    for pair in shuffled.chunks_mut(2) {
        pair.reverse();
    }
    let late = run(WatchParams { end_symbols: vec!["END".into()], lateness: 50, ..base.clone() }, &shuffled);
    assert_eq!(support(&late, &["START", "WORK", "END"]), 1000);
    assert_eq!(support(&late, &["PING", "PONG"]), 50);

    assert!(Watch::new(WatchParams { end_symbols: vec![String::new()], ..base.clone() }).is_err());
    assert!(Watch::new(WatchParams { end_symbols: (0..65).map(|i| i.to_string()).collect(), ..base }).is_err());
}

/// An id that comes back after a long silence is another entity's, or
/// the same one starting over: its events are not joined to the old
/// ones. And the silent give up their places before the active do.
#[test]
fn an_idle_limit_stops_joins_across_a_reused_id_and_frees_the_silent_first() {
    // Process ids 0 to 9 are reused: each run is LOGIN, WORK, LOGOUT a
    // tick apart, and the id comes back 1,000 ticks later.
    let mut events: Vec<Event> = Vec::new();
    for round in 0..50i64 {
        for pid in 0..10i64 {
            let t = round * 1000 + pid * 10;
            for (step, name) in ["LOGIN", "WORK", "LOGOUT"].iter().enumerate() {
                events.push((format!("pid-{pid}"), name.to_string(), t + step as i64));
            }
        }
    }
    let base = WatchParams { pane_ticks: 1_000_000, max_patterns: 10_000, min_cohesion_bits: -100.0, ..WatchParams::default() };
    let support = |reports: &[WindowReport], symbols: &[&str]| reports.last().unwrap().patterns.iter().find(|p| p.symbols == symbols).map_or(0, |p| p.support);

    // The most often any reported sequence runs from one session into the
    // next (a shorter one is left to the longer that extends it).
    let crossing = |reports: &[WindowReport]| reports.last().unwrap().patterns.iter().filter(|p| p.symbols.windows(2).any(|w| w[0] == "LOGOUT" && w[1] == "LOGIN")).map(|p| p.support).max().unwrap_or(0);
    // Without a limit, each new session is joined to the end of the last.
    let without = run(base.clone(), &events);
    assert!(crossing(&without) >= 480, "{}", crossing(&without));
    // With one, no session runs on into the next, and the sessions themselves are whole.
    let mut watch = Watch::new(WatchParams { idle_ticks: Some(100), ..base.clone() }).unwrap();
    let mut with = Vec::new();
    for (entity, symbol, time) in &events {
        with.extend(watch.observe(entity, symbol, *time));
    }
    // A checkpoint taken here carries the times: the restored watch must
    // still know how long each id has been silent.
    let saved = watch.checkpoint().unwrap();
    let (rest, totals) = watch.finish();
    with.extend(rest);
    assert_eq!(crossing(&with), 0);
    assert_eq!(support(&with, &["LOGIN", "WORK", "LOGOUT"]), 500);
    assert_eq!(totals.idle_ended, 490);
    assert!(with.last().unwrap().exact);
    let mut restored = Watch::restore(&saved).unwrap();
    assert_eq!(restored.checkpoint().unwrap(), saved);
    restored.observe("pid-3", "LOGIN", 60_000);
    let (reports, totals) = restored.finish();
    assert_eq!(totals.idle_ended, 491);
    assert_eq!(crossing(&reports), 0);
    // An interval inside the limit is still one sequence.
    let mut close = Watch::new(WatchParams { idle_ticks: Some(100), min_support: 1, ..base.clone() }).unwrap();
    for (i, name) in ["A", "B", "C"].iter().enumerate() {
        close.observe("x", name, i as i64 * 100);
    }
    let (reports, totals) = close.finish();
    assert_eq!((support(&reports, &["A", "B", "C"]), totals.idle_ended), (1, 0));

    // Eight places. One job pings every 40 ticks; a visitor comes every 8
    // and is never seen again. Without a limit the visitors are pushed
    // out as evictions, which a report must count as possible loss. With
    // one, a visitor silent past the limit is let go: by the rule its
    // next event would have started afresh anyway, so nothing is lost
    // and the report stays exact. (A limit cannot help when more
    // entities are active inside it than there are places.)
    let mut events: Vec<Event> = Vec::new();
    for t in 0..4000i64 {
        if t % 40 == 0 {
            events.push(("long".into(), if (t / 40) % 2 == 0 { "PING".into() } else { "PONG".into() }, t));
        } else if t % 8 == 1 {
            events.push((format!("visitor-{t}"), "HELLO".into(), t));
        }
    }
    let tight = WatchParams { live: LiveParams { max_entities: 8, ..Default::default() }, ..base };
    let without = run(tight.clone(), &events);
    assert!(without.last().unwrap().entities_evicted > 0 && !without.last().unwrap().exact);
    let with = run(WatchParams { idle_ticks: Some(45), ..tight.clone() }, &events);
    assert_eq!((with.last().unwrap().entities_evicted, with.last().unwrap().exact), (0, true));
    // And it counts what a watch with room for everyone counts.
    let roomy = run(WatchParams { idle_ticks: Some(45), live: LiveParams::default(), ..tight }, &events);
    assert_eq!(window_patterns(with.last().unwrap()), window_patterns(roomy.last().unwrap()));
    assert!(!window_patterns(with.last().unwrap()).is_empty());

    // A checkpoint whose times do not fit is refused, and so is a limit of nothing.
    let mut doc: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    doc["tail_times"].as_array_mut().unwrap().pop();
    assert!(Watch::restore(&serde_json::to_vec(&doc).unwrap()).is_err());
    let mut doc: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    doc["tail_times"][0] = serde_json::json!(i64::MAX);
    assert!(Watch::restore(&serde_json::to_vec(&doc).unwrap()).is_err());
    assert!(Watch::new(WatchParams { idle_ticks: Some(0), ..WatchParams::default() }).is_err());
}
