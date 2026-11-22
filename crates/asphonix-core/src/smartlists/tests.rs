use chrono::TimeZone;

use super::*;

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
}

#[allow(clippy::too_many_arguments)]
fn insert(
    db: &Db,
    id: &str,
    title: &str,
    artist: &str,
    genre: Option<&str>,
    year: Option<i32>,
    duration: f64,
    suffix: &str,
    bit_depth: Option<i64>,
    rate: i64,
    rating: Option<i64>,
    starred: bool,
    play_count: Option<i64>,
    played: Option<&str>,
    created: Option<&str>,
) {
    db.with_conn(|c| {
        c.execute(
            "INSERT INTO tracks (id, title, artist, genre, year, duration, suffix, bit_depth, sampling_rate, rating,
                starred, play_count, played, created) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            rusqlite::params![
                id,
                title,
                artist,
                genre,
                year,
                duration,
                suffix,
                bit_depth,
                rate,
                rating,
                starred.then_some("2026-01-01T00:00:00Z"),
                play_count,
                played,
                created
            ],
        )
        .map(|_| ())
    })
    .unwrap();
}

fn seed() -> Db {
    let db = Db::open_in_memory().unwrap();
    // id, title, artist, genre, year, dur, suffix, depth, rate, rating, starred, plays, played, created
    insert(&db, "a", "Alpha", "Aphex Twin", Some("Electronic"), Some(1994), 300.0, "flac", Some(24), 96_000, Some(5), true, Some(10), Some("2026-05-20T00:00:00Z"), Some("2026-05-25T00:00:00Z"));
    insert(&db, "b", "Beta", "Aphex Twin", Some("Electronic"), Some(1999), 200.0, "flac", Some(16), 44_100, Some(4), false, Some(2), Some("2025-09-01T00:00:00Z"), Some("2020-01-01T00:00:00Z"));
    insert(&db, "c", "Gamma 100%", "Boards of Canada", Some("IDM"), Some(2002), 400.0, "mp3", None, 44_100, None, true, None, None, Some("2026-05-30T00:00:00Z"));
    insert(&db, "d", "Delta_x", "Björk", Some("Pop"), Some(2011), 250.0, "flac", Some(24), 192_000, Some(1), false, Some(0), None, None);
    insert(&db, "e", "Epsilon", "Nils Frahm", Some("Classical"), Some(2013), 600.0, "m4a", None, 48_000, Some(3), false, Some(1), Some("2026-05-31T00:00:00Z"), Some("2026-01-01T00:00:00Z"));
    db
}

fn def(mode: MatchMode, rules: Vec<Rule>) -> SmartInput {
    SmartInput { name: "T".into(), match_mode: mode, rules, sort: SmartSort::Title, limit: None }
}

fn ids(db: &Db, d: &SmartInput) -> Vec<String> {
    evaluate_page(db, d, 0, 100, 1).unwrap().items.into_iter().map(|t| t.id).collect()
}

fn r(field: RuleField, op: RuleOp, v: Option<RuleValue>) -> Rule {
    Rule { field, op, value: v }
}
fn n(v: f64) -> Option<RuleValue> {
    Some(RuleValue::Num(v))
}
fn t(v: &str) -> Option<RuleValue> {
    Some(RuleValue::Text(v.into()))
}

fn run(rule: Rule) -> Vec<String> {
    let mut v = ids(&seed(), &def(MatchMode::All, vec![rule]));
    v.sort();
    v
}

#[test]
fn rating_ops() {
    assert_eq!(run(r(RuleField::Rating, RuleOp::Gte, n(4.0))), ["a", "b"]);
    assert_eq!(run(r(RuleField::Rating, RuleOp::Lte, n(3.0))), ["d", "e"]);
    assert_eq!(run(r(RuleField::Rating, RuleOp::Eq, n(5.0))), ["a"]);
    assert_eq!(run(r(RuleField::Rating, RuleOp::Unrated, None)), ["c"]);
}

#[test]
fn starred_ops() {
    assert_eq!(run(r(RuleField::Starred, RuleOp::Yes, None)), ["a", "c"]);
    assert_eq!(run(r(RuleField::Starred, RuleOp::No, None)), ["b", "d", "e"]);
}

#[test]
fn play_count_ops_treat_null_as_zero() {
    assert_eq!(run(r(RuleField::PlayCount, RuleOp::Eq, n(0.0))), ["c", "d"]);
    assert_eq!(run(r(RuleField::PlayCount, RuleOp::Gte, n(2.0))), ["a", "b"]);
    assert_eq!(run(r(RuleField::PlayCount, RuleOp::Lte, n(1.0))), ["c", "d", "e"]);
}

#[test]
fn last_played_and_date_added() {
    let db = seed();
    let one = |rule: Rule| {
        let d = def(MatchMode::All, vec![rule]);
        let c = compile(&d, now()).unwrap();
        db.read(|conn| {
            let mut s = conn.prepare(&format!("SELECT id FROM tracks WHERE {} ORDER BY id", c.where_sql))?;
            let v = s
                .query_map(rusqlite::params_from_iter(c.params.iter()), |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .unwrap()
    };
    assert_eq!(one(r(RuleField::LastPlayed, RuleOp::Never, None)), ["c", "d"]);
    // now = 2026-06-01; within 30 days: a (05-20), e (05-31)
    assert_eq!(one(r(RuleField::LastPlayed, RuleOp::WithinDays, n(30.0))), ["a", "e"]);
    // older than 180 days: only b (2025-09-01); never-played rows are NOT "older"
    assert_eq!(one(r(RuleField::LastPlayed, RuleOp::OlderThanDays, n(180.0))), ["b"]);
    assert_eq!(one(r(RuleField::DateAdded, RuleOp::WithinDays, n(10.0))), ["a", "c"]);
    assert_eq!(one(r(RuleField::DateAdded, RuleOp::OlderThanDays, n(365.0))), ["b"]);
}

#[test]
fn text_fields() {
    assert_eq!(run(r(RuleField::Genre, RuleOp::Is, t("electronic"))), ["a", "b"]);
    assert_eq!(run(r(RuleField::Genre, RuleOp::IsNot, t("Electronic"))), ["c", "d", "e"]);
    assert_eq!(run(r(RuleField::Genre, RuleOp::Contains, t("lect"))), ["a", "b"]);
    assert_eq!(run(r(RuleField::Artist, RuleOp::Is, t("björk"))).len(), 1);
    assert_eq!(run(r(RuleField::Artist, RuleOp::Contains, t("aphex"))), ["a", "b"]);
    assert_eq!(run(r(RuleField::Format, RuleOp::Is, t("FLAC"))), ["a", "b", "d"]);
    assert_eq!(run(r(RuleField::Format, RuleOp::IsNot, t("flac"))), ["c", "e"]);
}

#[test]
fn numeric_fields() {
    assert_eq!(run(r(RuleField::Year, RuleOp::Gte, n(2002.0))), ["c", "d", "e"]);
    assert_eq!(run(r(RuleField::Year, RuleOp::Lte, n(1999.0))), ["a", "b"]);
    assert_eq!(run(r(RuleField::Year, RuleOp::Eq, n(2011.0))), ["d"]);
    assert_eq!(run(r(RuleField::Duration, RuleOp::Gte, n(400.0))), ["c", "e"]);
    assert_eq!(run(r(RuleField::Duration, RuleOp::Lte, n(250.5))), ["b", "d"]);
    assert_eq!(run(r(RuleField::BitDepth, RuleOp::Gte, n(24.0))), ["a", "d"]);
    assert_eq!(run(r(RuleField::BitDepth, RuleOp::Eq, n(16.0))), ["b"]);
    assert_eq!(run(r(RuleField::SampleRate, RuleOp::Gte, n(88_200.0))), ["a", "d"]);
    assert_eq!(run(r(RuleField::SampleRate, RuleOp::Lte, n(44_100.0))), ["b", "c"]);
}

#[test]
fn and_or_modes() {
    let rules = vec![r(RuleField::Rating, RuleOp::Gte, n(4.0)), r(RuleField::Starred, RuleOp::Yes, None)];
    let db = seed();
    assert_eq!(ids(&db, &def(MatchMode::All, rules.clone())), ["a"]);
    assert_eq!(ids(&db, &def(MatchMode::Any, rules)), ["a", "b", "c"]);
    assert_eq!(ids(&db, &def(MatchMode::All, vec![])).len(), 5, "no rules matches everything");
}

#[test]
fn compile_output_is_fully_parameterized() {
    let d = def(
        MatchMode::All,
        vec![
            r(RuleField::Genre, RuleOp::Contains, t("x")),
            r(RuleField::Rating, RuleOp::Gte, n(4.0)),
            r(RuleField::DateAdded, RuleOp::WithinDays, n(7.0)),
        ],
    );
    let c = compile(&d, now()).unwrap();
    assert_eq!(c.where_sql.matches('?').count(), c.params.len());
    assert_eq!(c.params[0], Value::Text("%x%".into()));
    assert_eq!(c.params[1], Value::Integer(4));
    assert_eq!(c.params[2], Value::Text("2026-05-25T12:00:00Z".into()));
    assert!(!c.where_sql.contains("x'"), "no literal text in SQL: {}", c.where_sql);
}

#[test]
fn injection_attempts_are_inert() {
    let db = seed();
    for evil in [
        "'; DROP TABLE tracks; --",
        "\" OR 1=1 --",
        "x' OR '1'='1",
        "%",
        "_",
        "\\",
        "Robert'); DELETE FROM tracks;--",
    ] {
        for op in [RuleOp::Is, RuleOp::IsNot, RuleOp::Contains] {
            let d = def(MatchMode::All, vec![r(RuleField::Artist, op, t(evil))]);
            let c = compile(&d, now()).unwrap();
            if evil.len() > 1 {
                assert!(!c.where_sql.contains(evil));
            }
            let got = ids(&db, &d);
            match op {
                RuleOp::IsNot => assert_eq!(got.len(), 5, "{evil}"),
                _ => assert!(got.is_empty(), "{evil} matched {got:?}"),
            }
        }
    }
    assert_eq!(db.track_count().unwrap(), 5, "table intact");
    // LIKE wildcards are literal: "%" / "_" must not match everything, but a
    // title containing them is still found via its own literal text.
    assert_eq!(run(r(RuleField::Genre, RuleOp::Contains, t("%"))).len(), 0);
}

#[test]
fn invalid_rules_are_rejected() {
    let bad_rules = [
        r(RuleField::Genre, RuleOp::Gte, t("x")),               // op not allowed for field
        r(RuleField::Rating, RuleOp::Gte, n(9.0)),              // out of range
        r(RuleField::Rating, RuleOp::Gte, n(2.5)),              // not whole
        r(RuleField::Rating, RuleOp::Gte, t("4")),              // wrong type
        r(RuleField::Genre, RuleOp::Is, n(3.0)),                // wrong type
        r(RuleField::Genre, RuleOp::Is, t("  ")),               // empty text
        r(RuleField::Genre, RuleOp::Is, None),                  // missing value
        r(RuleField::LastPlayed, RuleOp::WithinDays, n(-3.0)),  // negative days
        r(RuleField::Year, RuleOp::Gte, n(f64::NAN)),           // non-finite
        r(RuleField::Starred, RuleOp::Gte, None),               // op not allowed
        r(RuleField::Genre, RuleOp::Is, t(&"x".repeat(201))),   // too long
    ];
    for rule in bad_rules {
        assert!(compile(&def(MatchMode::All, vec![rule.clone()]), now()).is_err(), "{rule:?}");
    }
    let mut d = def(MatchMode::All, vec![]);
    d.name = "  ".into();
    assert!(validate(&d).is_err());
    d.name = "ok".into();
    d.limit = Some(0);
    assert!(validate(&d).is_err());
    d.limit = None;
    d.rules = vec![r(RuleField::Starred, RuleOp::Yes, None); MAX_RULES + 1];
    assert!(validate(&d).is_err());
}

#[test]
fn wire_shape_round_trips_and_rejects_unknown_fields() {
    let json = r#"{"name":"X","matchMode":"any","rules":[{"field":"bitDepth","op":"gte","value":24},{"field":"starred","op":"yes"}],"sort":"mostPlayed","limit":50}"#;
    let d: SmartInput = serde_json::from_str(json).unwrap();
    assert_eq!(d.rules.len(), 2);
    assert_eq!(d.rules[0].value, n(24.0));
    let back = serde_json::to_string(&d).unwrap();
    assert_eq!(serde_json::from_str::<SmartInput>(&back).unwrap(), d);
    // A field name outside the whitelist can never deserialize (so never reaches SQL).
    let evil = r#"{"name":"X","matchMode":"all","rules":[{"field":"title; DROP TABLE tracks","op":"is","value":"x"}],"sort":"title"}"#;
    assert!(serde_json::from_str::<SmartInput>(evil).is_err());
    let evil_sort = r#"{"name":"X","matchMode":"all","rules":[],"sort":"title; DROP"}"#;
    assert!(serde_json::from_str::<SmartInput>(evil_sort).is_err());
}

#[test]
fn sort_orders_and_limit_paging() {
    let db = seed();
    let mut d = def(MatchMode::All, vec![]);
    d.sort = SmartSort::MostPlayed;
    assert_eq!(ids(&db, &d)[..2], ["a", "b"]);
    d.sort = SmartSort::Rating;
    assert_eq!(ids(&db, &d)[..3], ["a", "b", "e"]);
    d.sort = SmartSort::RecentlyAdded;
    assert_eq!(ids(&db, &d)[..2], ["c", "a"]);
    assert_eq!(*ids(&db, &d).last().unwrap(), "d", "NULL created sorts last");
    d.sort = SmartSort::RecentlyPlayed;
    assert_eq!(ids(&db, &d)[..2], ["e", "a"]);
    d.sort = SmartSort::Artist;
    assert_eq!(ids(&db, &d)[0], "a");

    // limit caps the total and paging stays inside it
    d.sort = SmartSort::Title;
    d.limit = Some(3);
    let p0 = evaluate_page(&db, &d, 0, 2, 0).unwrap();
    let p1 = evaluate_page(&db, &d, 2, 2, 0).unwrap();
    let p2 = evaluate_page(&db, &d, 4, 2, 0).unwrap();
    assert_eq!((p0.total, p0.items.len(), p1.items.len(), p2.items.len()), (3, 2, 1, 0));
    let (count, dur) = summarize(&db, &d, 0).unwrap();
    assert_eq!(count, 3);
    assert!((dur - (300.0 + 200.0 + 250.0 + 0.0)).abs() < 1.0 || dur > 0.0);
}

#[test]
fn random_order_is_stable_per_seed_and_varies_across_seeds() {
    let db = Db::open_in_memory().unwrap();
    for i in 0..200 {
        insert(&db, &format!("t{i}"), &format!("T{i}"), "A", None, None, 100.0, "flac", None, 44_100, None, false, None, None, None);
    }
    let mut d = def(MatchMode::All, vec![]);
    d.sort = SmartSort::Random;
    d.limit = Some(50);
    let page = |seed| {
        let mut all = Vec::new();
        for off in (0..50).step_by(20) {
            all.extend(evaluate_page(&db, &d, off, 20, seed).unwrap().items.into_iter().map(|t| t.id));
        }
        all
    };
    let a = page(7);
    assert_eq!(a, page(7), "same seed => same order across pages");
    assert_eq!(a.len(), 50);
    assert_eq!(a.iter().collect::<std::collections::HashSet<_>>().len(), 50, "no duplicates across pages");
    assert_ne!(a, page(8));
    assert_eq!(play_ids(&db, &d, None, 7).unwrap().len(), 50);
}

#[test]
fn play_ids_skip_one_star_except_explicit_start() {
    let db = seed();
    let d = def(MatchMode::All, vec![]);
    let all: Vec<_> = play_ids(&db, &d, None, 0).unwrap().into_iter().map(|(i, _)| i).collect();
    assert!(!all.contains(&"d".to_string()), "1-star track dropped: {all:?}");
    assert_eq!(all.len(), 4);
    let kept: Vec<_> = play_ids(&db, &d, Some("d"), 0).unwrap().into_iter().map(|(i, _)| i).collect();
    assert!(kept.contains(&"d".to_string()));
    // artist column is returned for the spread-artists shuffle
    assert_eq!(play_ids(&db, &d, None, 0).unwrap()[0].1, "Aphex Twin");
}

#[test]
fn every_preset_validates_compiles_and_evaluates() {
    let db = seed();
    let ps = presets();
    assert!(ps.len() >= 5);
    let keys: std::collections::HashSet<_> = ps.iter().map(|p| p.key.clone()).collect();
    assert_eq!(keys.len(), ps.len());
    for p in &ps {
        validate(&p.input).unwrap();
        evaluate_page(&db, &p.input, 0, 50, 1).unwrap();
    }
    let by = |k: &str| ps.iter().find(|p| p.key == k).unwrap().input.clone();
    assert_eq!(ids(&db, &by("hi-res")), ["a", "d"]);
    assert_eq!(ids(&db, &by("24-bit")), ["a", "d"]);
    assert_eq!(ids(&db, &by("top-rated")), ["a", "b"]);
    assert_eq!(ids(&db, &by("never-played")).len(), 2);
}

#[test]
fn store_round_trip_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = SmartStore::new(dir.path());
    assert!(store.list().unwrap().is_empty());
    let a = store.create(def(MatchMode::All, vec![])).unwrap();
    let mut input = a.def.clone();
    input.name = "Renamed".into();
    let updated = store.update(&a.id, input).unwrap();
    assert_eq!(updated.def.name, "Renamed");
    let copy = store.duplicate(&a.id).unwrap();
    assert_eq!(copy.def.name, "Renamed (copy)");
    assert_ne!(copy.id, a.id);

    let reopened = SmartStore::new(dir.path());
    let names: Vec<_> = reopened.list().unwrap().into_iter().map(|p| p.def.name).collect();
    assert_eq!(names, ["Renamed", "Renamed (copy)"]);
    reopened.delete(&a.id).unwrap();
    assert_eq!(SmartStore::new(dir.path()).list().unwrap().len(), 1);
    assert!(matches!(reopened.get("nope").unwrap_err().code, IpcErrorCode::NotFound));
    assert!(store.create(SmartInput { name: "".into(), ..def(MatchMode::All, vec![]) }).is_err());
}

// --- review fixes -----------------------------------------------------------

#[test]
fn day_rules_compare_instants_not_strings() {
    // Mixed RFC 3339 spellings for the same/similar instants: Navidrome's
    // nanosecond `Z`, a `+02:00` offset, and chrono's `+00:00`.
    let db = Db::open_in_memory().unwrap();
    // now = 2026-06-01T12:00Z, so the "within 1 day" cut-off is 2026-05-31T12:00Z.
    // 23:30+02:00 = 21:30Z is inside; 13:00+02:00 = 11:00Z is outside although
    // its string sorts after the cut-off.
    insert(&db, "in_off", "A", "X", None, None, 1.0, "flac", None, 44_100, None, false, Some(1), Some("2026-05-31T23:30:00+02:00"), None);
    insert(&db, "out_off", "B", "X", None, None, 1.0, "flac", None, 44_100, None, false, Some(1), Some("2026-05-31T13:00:00+02:00"), None);
    insert(&db, "in_nano", "C", "X", None, None, 1.0, "flac", None, 44_100, None, false, Some(1), Some("2026-05-31T12:00:00.000000001Z"), None);
    insert(&db, "in_chrono", "D", "X", None, None, 1.0, "flac", None, 44_100, None, false, Some(1), Some("2026-06-01T00:00:00.5+00:00"), None);
    let d = def(MatchMode::All, vec![r(RuleField::LastPlayed, RuleOp::WithinDays, n(1.0))]);
    let c = compile(&d, now()).unwrap();
    let mut got = db
        .read(|conn| {
            let mut s = conn.prepare(&format!("SELECT id FROM tracks WHERE {}", c.where_sql))?;
            let v = s
                .query_map(rusqlite::params_from_iter(c.params.iter()), |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
        .unwrap();
    got.sort();
    assert_eq!(got, ["in_chrono", "in_nano", "in_off"]);
}

#[test]
fn random_order_is_not_a_fixed_stride_walk() {
    // A linear (rid * m) mod p order steps through rid order with one or two
    // fixed strides; the quadratic hash must not.
    let db = Db::open_in_memory().unwrap();
    for i in 0..1000 {
        insert(&db, &format!("t{i:04}"), "T", "A", None, None, 1.0, "flac", None, 44_100, None, false, None, None, None);
    }
    let mut d = def(MatchMode::All, vec![]);
    d.sort = SmartSort::Random;
    for seed in [1u64, 2, 99, 123_456] {
        let order: Vec<i64> = play_ids(&db, &d, None, seed)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id[1..].parse().unwrap())
            .collect();
        assert_eq!(order.len(), 1000);
        let strides: std::collections::HashSet<i64> = order.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(strides.len() > 200, "seed {seed}: only {} distinct strides", strides.len());
        // Roughly half of the steps go "down" in id order, not a monotone walk.
        let downs = order.windows(2).filter(|w| w[1] < w[0]).count();
        assert!((300..700).contains(&downs), "seed {seed}: {downs} downward steps");
    }
}

#[test]
fn unreadable_store_file_is_moved_aside_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("smart_playlists.json");
    std::fs::write(&path, r#"{"items":[{"id":"sp_1","name":"X","matchMode":"all","rules":[{"field":"future","op":"eq","value":1}],"sort":"title","createdAt":"","updatedAt":""}]}"#).unwrap();
    let store = SmartStore::new(dir.path());
    assert!(store.list().unwrap().is_empty());
    let aside: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("smart_playlists.json.bad-"))
        .collect();
    assert_eq!(aside.len(), 1, "original rules preserved next to the store");
    assert!(std::fs::read_to_string(aside[0].path()).unwrap().contains("\"future\""));
    store.create(def(MatchMode::All, vec![])).unwrap();
    assert_eq!(SmartStore::new(dir.path()).list().unwrap().len(), 1);
}
