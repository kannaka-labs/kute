use kute_events::{header, pack, unpack, Kind, Layout, TimeOrder};

fn layout() -> Layout {
    Layout { symbol_field: "event".into(), time_field: Some("t".into()), group_field: Some("id".into()) }
}

const LIMIT: u64 = 1 << 24;

#[test]
fn canonical_records_come_back_byte_for_byte() {
    let source = concat!(
        "{\"event\":\"open\",\"id\":\"a\",\"n\":1,\"t\":10}\n",
        "{\"event\":\"open\",\"id\":7,\"n\":2,\"t\":11}\n",
        "{\"event\":\"read\",\"id\":\"a\",\"n\":3,\"note\":{\"k\":[1,2.5,null,\"x\\ny\"]},\"t\":11}\n",
        "{\"event\":\"close\",\"id\":\"7\",\"t\":-4}\n",
        "{\"event\":\"close\",\"id\":true,\"n\":-9223372036854775808,\"t\":9223372036854775807}\n",
    )
    .as_bytes();
    let (container, report) = pack(source, &layout()).unwrap();
    assert!(report.header.byte_exact);
    assert_eq!(report.header.events, 5);
    // 7, "7", "a" and true are four entities.
    assert_eq!(report.entities, 4);
    assert_eq!(unpack(&container, LIMIT).unwrap(), source);
}

#[test]
fn records_that_are_not_canonical_come_back_canonical_and_the_header_says_so() {
    let source = b"{ \"t\": 3, \"id\": \"a\", \"event\": \"x\", \"z\": {\"b\": 1, \"a\": 2} }\r\n\n{\"event\":\"y\",\"id\":\"a\",\"t\":4}";
    let (container, report) = pack(source, &layout()).unwrap();
    assert!(!report.header.byte_exact);
    assert_eq!(
        unpack(&container, LIMIT).unwrap(),
        b"{\"event\":\"x\",\"id\":\"a\",\"t\":3,\"z\":{\"a\":2,\"b\":1}}\n{\"event\":\"y\",\"id\":\"a\",\"t\":4}\n"
    );
}

#[test]
fn numbers_keep_their_spelling() {
    // 5 and 5.0 in one field: the column cannot be stored as integers.
    let source = b"{\"event\":\"x\",\"v\":5}\n{\"event\":\"x\",\"v\":5.0}\n{\"event\":\"x\",\"v\":1e3}\n";
    let layout = Layout { symbol_field: "event".into(), time_field: None, group_field: None };
    let (container, report) = pack(source, &layout).unwrap();
    assert_eq!(report.header.columns.iter().find(|c| c.name == "f:v").unwrap().kind, Kind::Json);
    let back = unpack(&container, LIMIT).unwrap();
    assert_eq!(pack(&back, &layout).unwrap().1.header.sha256, report.header.sha256);
    assert!(std::str::from_utf8(&back).unwrap().contains("5.0"));
}

#[test]
fn layouts_without_time_or_entity_work() {
    let source = b"{\"event\":\"a\",\"x\":1}\n{\"event\":\"b\"}\n{\"event\":\"a\",\"x\":2}\n";
    for (time, group) in [(None, None), (Some("x"), None), (None, Some("event2"))] {
        let layout = Layout { symbol_field: "event".into(), time_field: time.map(str::to_string), group_field: group.map(str::to_string) };
        match pack(source, &layout) {
            Ok((container, _)) => assert_eq!(unpack(&container, LIMIT).unwrap(), source),
            // A named field that a record lacks is refused, not skipped.
            Err(e) => assert!(time.is_some() || group.is_some(), "{e}"),
        }
    }
    assert_eq!(unpack(&pack(b"", &layout()).unwrap().0, LIMIT).unwrap(), b"");
}

#[test]
fn times_use_whichever_order_is_smaller_and_both_come_back() {
    // Two entities far apart in time, interleaved: differences within an
    // entity are small, differences in source order are large.
    let mut far = String::new();
    // One clock for everything, a new entity per event.
    let mut near = String::new();
    for i in 0..4000u64 {
        far.push_str(&format!("{{\"event\":\"e\",\"id\":{},\"t\":{}}}\n", i % 2, (i % 2) * 1_000_000_007 + i * 3 + (i * i) % 5));
        near.push_str(&format!("{{\"event\":\"e\",\"id\":{},\"t\":{}}}\n", i, i * 3 + (i * i) % 5));
    }
    for (source, expect) in [(far, TimeOrder::Entity), (near, TimeOrder::Source)] {
        let (container, report) = pack(source.as_bytes(), &layout()).unwrap();
        assert_eq!(report.header.time_order, Some(expect));
        assert_eq!(unpack(&container, LIMIT).unwrap(), source.as_bytes());
    }
}

#[test]
fn what_cannot_be_kept_exactly_is_refused_with_its_line() {
    let cases: [(&[u8], &str); 8] = [
        (b"{\"event\":\"x\",\"id\":1,\"t\":1}\n[1]\n", "line 2"),
        (b"{\"id\":1,\"t\":1}\n", "no field \"event\""),
        (b"{\"event\":7,\"id\":1,\"t\":1}\n", "not a string"),
        (b"{\"event\":\"x\",\"id\":1,\"t\":1.5}\n", "whole number"),
        (b"{\"event\":\"x\",\"id\":1,\"t\":1.0}\n", "whole number"),
        (b"{\"event\":\"x\",\"id\":[1],\"t\":1}\n", "not a string, a number or a boolean"),
        (b"{\"event\":\"x\",\"id\":1,\"t\":1,\"t\":2}\n", "repeats the key"),
        (b"{\"event\":\"x\",\"id\":1,\"t\":1,\"v\":1e400}\n", "line 1"),
    ];
    for (source, expect) in cases {
        let error = pack(source, &layout()).unwrap_err().to_string();
        assert!(error.contains(expect), "{error}");
    }
    let same = Layout { symbol_field: "a".into(), time_field: Some("a".into()), group_field: None };
    assert!(pack(b"", &same).is_err());
}

#[test]
fn a_damaged_container_is_refused_and_never_gives_wrong_records() {
    let mut source = String::new();
    for i in 0..300u32 {
        source.push_str(&format!("{{\"event\":\"e{}\",\"id\":\"n{}\",\"t\":{},\"v\":\"{}\"}}\n", i % 5, i % 7, i * 3, i % 11));
    }
    let (container, _) = pack(source.as_bytes(), &layout()).unwrap();
    assert_eq!(unpack(&container, LIMIT).unwrap(), source.as_bytes());
    // Every single-byte change either is refused or (never) gives the source back changed.
    for at in 0..container.len() {
        for flip in [0x01u8, 0x80] {
            let mut bad = container.clone();
            bad[at] ^= flip;
            if let Ok(out) = unpack(&bad, LIMIT) {
                assert_eq!(out, source.as_bytes(), "byte {at} changed and different records came back");
            }
        }
    }
    for cut in 0..container.len() {
        assert!(unpack(&container[..cut], LIMIT).is_err(), "cut at {cut}");
    }
    let mut longer = container.clone();
    longer.push(0);
    assert!(unpack(&longer, LIMIT).is_err());
}

#[test]
fn the_limit_is_checked_before_anything_is_inflated() {
    let source = "{\"event\":\"e\",\"v\":0}\n".repeat(50_000);
    let layout = Layout { symbol_field: "event".into(), time_field: None, group_field: None };
    let (container, report) = pack(source.as_bytes(), &layout).unwrap();
    assert!(report.container_bytes < 2_000);
    assert!(unpack(&container, 1000).unwrap_err().to_string().contains("limit"));

    // A header that declares little and columns that inflate to much.
    let (h, start) = header(&container).unwrap();
    let mut lying = h.clone();
    lying.canonical_bytes = 100;
    lying.events = 5;
    let text = serde_json::to_vec(&lying).unwrap();
    let mut forged = Vec::new();
    forged.extend_from_slice(kute_events::MAGIC);
    forged.extend_from_slice(&(text.len() as u32).to_le_bytes());
    forged.extend_from_slice(&text);
    forged.extend_from_slice(&container[start..]);
    assert!(unpack(&forged, 1 << 30).is_err());
    // Columns that declare more than the budget are refused by their declared size.
    lying.columns[0].raw = u64::MAX / 2;
    let text = serde_json::to_vec(&lying).unwrap();
    let mut forged = Vec::new();
    forged.extend_from_slice(kute_events::MAGIC);
    forged.extend_from_slice(&(text.len() as u32).to_le_bytes());
    forged.extend_from_slice(&text);
    forged.extend_from_slice(&container[start..]);
    assert!(unpack(&forged, 1000).is_err());
}

/// A container written by hand, with whatever the columns say.
fn forged(events: u64, canonical: &[u8], columns: &[(&str, Kind, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut body = Vec::new();
    let mut described = Vec::new();
    for (name, kind, raw) in columns {
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(raw).unwrap();
        let stored = encoder.finish().unwrap();
        described.push(kute_events::Column { name: name.to_string(), kind: *kind, codec: kute_events::Codec::Zlib, raw: raw.len() as u64, stored: stored.len() as u64 });
        body.extend(stored);
    }
    let header = kute_events::Header {
        format: 1,
        layout: Layout { symbol_field: "s".into(), time_field: None, group_field: Some("g".into()) },
        events,
        canonical_bytes: canonical.len() as u64,
        sha256: kute_types::sha256_tagged(canonical),
        byte_exact: true,
        time_order: None,
        columns: described,
    };
    let header = serde_json::to_vec(&header).unwrap();
    let mut out = kute_events::MAGIC.to_vec();
    out.extend((header.len() as u32).to_le_bytes());
    out.extend(header);
    out.extend(body);
    out
}

/// What a container's tables may cost is bounded by its events, and a
/// shape is what `pack` would have written. A container made by hand that
/// breaks either is refused: it does not take memory far beyond its size,
/// and it does not stop the program.
#[test]
fn a_forged_container_cannot_cost_more_than_its_events_or_stop_the_program() {
    let record = b"{\"g\":1,\"s\":\"A\"}\n";
    let honest: [(&str, Kind, &[u8]); 6] = [
        ("@names", Kind::Json, b"\"A\""),
        ("@shapes", Kind::Json, b"[\"g\",\"s\"]"),
        ("@shape", Kind::Varint, &[0]),
        ("@entities", Kind::Json, b"1"),
        ("@entity", Kind::Varint, &[0]),
        ("@symbols", Kind::Varint, &[0]),
    ];
    assert_eq!(unpack(&forged(1, record, &honest), 1 << 20).unwrap(), record, "the hand-made container is a real one");

    let refused = |what: &str, column: usize, raw: &[u8]| {
        let mut columns: Vec<(&str, Kind, &[u8])> = honest.to_vec();
        columns[column].2 = raw;
        let error = unpack(&forged(1, record, &columns), 1 << 20).expect_err(what).to_string();
        assert!(error.contains("event container"), "{what}: {error}");
    };
    // The event name twice in one record: each use takes the next name of
    // the entity, and the second ran past the end of the column.
    refused("the name field twice in a shape", 1, b"[\"g\",\"s\",\"s\"]");
    refused("a shape out of order", 1, b"[\"s\",\"g\"]");
    // Tables of nothing but line breaks: sixteen bytes of table and more
    // for every byte of column, from a column that compresses a thousandfold.
    let breaks = vec![b'\n'; 1 << 20];
    refused("names nobody uses", 0, &breaks);
    refused("entities nobody belongs to", 3, &breaks);
    refused("shapes nobody has", 1, &breaks);
    // One shape naming far more fields than a container may have.
    let many = format!("[{}]", (0..10_000).map(|i| format!("\"k{i:05}\"")).collect::<Vec<_>>().join(","));
    refused("a shape of ten thousand fields", 1, many.as_bytes());
    // More names than the declared bytes could hold, within the count of events.
    let wide = format!("[{}]", (0..100).map(|i| format!("\"k{i:03}\"")).collect::<Vec<_>>().join(","));
    refused("a shape wider than the record", 1, wide.as_bytes());
}

/// Bytes that no record uses are not carried along unseen.
#[test]
fn a_container_with_something_left_over_is_refused() {
    let source = b"{\"g\":1,\"n\":5,\"s\":\"A\"}\n{\"g\":1,\"n\":6,\"s\":\"B\"}\n";
    let (container, packed) = pack(source, &Layout { symbol_field: "s".into(), time_field: None, group_field: Some("g".into()) }).unwrap();
    assert_eq!(unpack(&container, 1 << 20).unwrap(), source);

    // Bytes slipped in after one column's compressed data.
    let (mut header, start) = header(&container).unwrap();
    let first = header.columns[0].stored as usize;
    header.columns[0].stored += 3;
    let text = serde_json::to_vec(&header).unwrap();
    let old_header = container.len() - packed.header.columns.iter().map(|c| c.stored as usize).sum::<usize>();
    assert_eq!(old_header, start);
    let mut changed = kute_events::MAGIC.to_vec();
    changed.extend((text.len() as u32).to_le_bytes());
    changed.extend(text);
    changed.extend(&container[start..start + first]);
    changed.extend(b"xyz");
    changed.extend(&container[start + first..]);
    let error = unpack(&changed, 1 << 20).unwrap_err().to_string();
    assert!(error.contains("after its compressed data"), "{error}");
}
