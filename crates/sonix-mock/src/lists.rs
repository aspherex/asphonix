//! Mock state and queries of the list routes, ported from `mock.ts` (Liked Songs, history,
//! smart playlists, settings, profiles, ratings). Columnar smart-rule attributes are built
//! lazily on first use, exactly like the JS `buildCols` (never 54k `Track` objects at rest).
use crate::library::{self, EPOCH_MS, Rng};
use sonix_api::format::local_day;
use sonix_api::*;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct Cols {
    n: usize,
    year: Vec<i32>,
    duration: Vec<f32>,
    bit_depth: Vec<u32>,
    rate: Vec<i32>,
    created: Vec<f64>,
    play_count: Vec<i32>,
    played: Vec<f64>,
    genre: Vec<u8>,
    suffix: Vec<u8>,
    artist: Vec<u16>,
    genres: Vec<String>,
    suffixes: Vec<String>,
    title_key: Vec<String>,
}

pub(crate) struct Stored {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub input: SmartInput,
}

pub(crate) struct ListState {
    pub ratings: HashMap<u32, u8>,
    /// newest first: (track index, unix ms)
    pub history: Vec<(u32, i64)>,
    pub smart: Vec<Stored>,
    pub smart_seq: u32,
    pub settings: Settings,
    pub profiles: Vec<(String, String)>,
    pub active: Option<usize>,
    pub cols: Option<Arc<Cols>>,
    /// bumped on star/rating changes (invalidates the eval memo)
    pub rev: u64,
    memo: HashMap<String, Arc<Vec<u32>>>,
}

pub const MOCK_VERSION: &str = "0.54.1";

impl ListState {
    pub fn new(total: usize) -> Self {
        // mock.ts: ~2.9k rated tracks (2-5 stars; no 1-star), every 17th of the first 49,000.
        let mut ratings = HashMap::new();
        let mut i = 0u32;
        while i < 49_000 {
            ratings.insert(i, 2 + ((i / 17) % 4) as u8);
            i += 17;
        }
        // mock.ts ensureMockHistory: ~100 plays over the last ~2 weeks.
        let history = (0..100i64)
            .map(|k| (((k * 137 + 11) % total as i64) as u32, EPOCH_MS - (k as f64 * 3.1 * 3_600_000.0) as i64 - 60_000))
            .collect();
        Self {
            ratings,
            history,
            smart: Vec::new(),
            smart_seq: 0,
            settings: Settings {
                scrobbling_enabled: true,
                auto_check_updates: true,
                spread_artists_in_shuffle: false,
                queue_window_size: 20,
                autoplay_enabled: true,
                sync_play_queue: true,
                play_threshold_sec: 30,
                free_memory_when_minimized: true,
                replay_gain_mode: ReplayGainMode::Off,
                track_overlay: true,
                track_overlay_corner: OverlayCorner::TopLeft,
                mcp_enabled: false,
                mcp_allow_control: false,
            },
            profiles: vec![("https://music.monolith.internal:4533".into(), "architect".into()), ("http://navidrome.lab:4533".into(), "guest".into())],
            active: Some(0),
            cols: None,
            rev: 0,
            memo: HashMap::new(),
        }
    }

    pub fn mcp_status(&self) -> McpStatus {
        let en = self.settings.mcp_enabled;
        McpStatus {
            enabled: en,
            allow_control: self.settings.mcp_allow_control,
            listening: en,
            bridge_path: Some("C:\\Program Files\\SONIX\\sonix-mcp.exe".into()),
            bridge_exists: true,
            register_command: Some("claude mcp add sonix -- \"C:\\Program Files\\SONIX\\sonix-mcp.exe\"".into()),
            last_error: None,
        }
    }

    pub fn apply(&mut self, ch: SettingChange) {
        let s = &mut self.settings;
        match ch {
            SettingChange::Scrobbling(v) => s.scrobbling_enabled = v,
            SettingChange::AutoCheckUpdates(v) => s.auto_check_updates = v,
            SettingChange::SpreadArtists(v) => s.spread_artists_in_shuffle = v,
            SettingChange::QueueWindowSize(v) => s.queue_window_size = v,
            SettingChange::Autoplay(v) => s.autoplay_enabled = v,
            SettingChange::SyncPlayQueue(v) => s.sync_play_queue = v,
            SettingChange::ReplayGain(v) => s.replay_gain_mode = v,
            SettingChange::PlayThreshold(v) => s.play_threshold_sec = v,
            SettingChange::TrackOverlay(v) => s.track_overlay = v,
            SettingChange::TrackOverlayCorner(v) => s.track_overlay_corner = v,
            SettingChange::FreeMemoryWhenMinimized(v) => s.free_memory_when_minimized = v,
            SettingChange::McpEnabled(v) => s.mcp_enabled = v,
            SettingChange::McpAllowControl(v) => s.mcp_allow_control = v,
        }
    }

    // ------------------------------------------------------------ profiles
    /// Opaque stable id (mock.ts uses an FNV hash too; the value is never shown).
    pub fn profile_id(url: &str, username: &str) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in format!("{url}\n{username}").bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        format!("{h:016x}")
    }
    pub fn profile_list(&self) -> Vec<Profile> {
        self.profiles
            .iter()
            .enumerate()
            .map(|(i, (u, n))| Profile { id: Self::profile_id(u, n), url: u.clone(), username: n.clone(), active: Some(i) == self.active })
            .collect()
    }
    pub fn upsert_profile(&mut self, url: &str, username: &str) {
        if let Some(i) = self.profiles.iter().position(|(u, n)| u == url && n == username) {
            self.profiles.remove(i);
        }
        self.profiles.insert(0, (url.to_string(), username.to_string()));
        self.active = Some(0);
    }

    // ------------------------------------------------------------ smart evaluation
    pub fn cols(&mut self, lib: &library::Library) -> Arc<Cols> {
        if let Some(c) = &self.cols {
            return c.clone();
        }
        let n = lib.total_tracks();
        let mut c = Cols {
            n,
            year: vec![0; n],
            duration: vec![0.0; n],
            bit_depth: vec![0; n],
            rate: vec![0; n],
            created: vec![0.0; n],
            play_count: vec![0; n],
            played: vec![0.0; n],
            genre: vec![0; n],
            suffix: vec![0; n],
            artist: vec![0; n],
            genres: Vec::new(),
            suffixes: Vec::new(),
            title_key: Vec::with_capacity(n),
        };
        fn intern(arr: &mut Vec<String>, v: &str) -> u8 {
            if let Some(i) = arr.iter().position(|x| x == v) {
                return i as u8;
            }
            arr.push(v.to_string());
            (arr.len() - 1) as u8
        }
        for i in 0..n {
            let t = lib.track(i);
            c.year[i] = t.year.map(|y| y as i32).unwrap_or(0);
            c.duration[i] = t.duration as f32;
            c.bit_depth[i] = t.format.bit_depth.unwrap_or(0);
            c.rate[i] = t.format.sampling_rate as i32;
            c.created[i] = lib.sort_fields(i).added as f64;
            c.genre[i] = intern(&mut c.genres, t.genre.as_deref().unwrap_or(""));
            c.suffix[i] = intern(&mut c.suffixes, &t.format.suffix);
            c.artist[i] = t.artist_id.as_deref().and_then(|a| a.strip_prefix("ar_")).and_then(|a| a.parse().ok()).unwrap_or(0);
            c.title_key.push(t.title.to_lowercase());
            let mut r = Rng::for_seed(i as u64 * 13 + 5);
            let pc = if r.next() < 0.4 { 0 } else { (r.next() * 30.0).floor() as i32 };
            c.play_count[i] = pc;
            c.played[i] = if pc > 0 { EPOCH_MS as f64 - ((r.next() * 400.0).floor() * 86_400_000.0) } else { 0.0 };
        }
        let c = Arc::new(c);
        self.cols = Some(c.clone());
        c
    }

    fn rule_matches(&self, i: usize, r: &SmartRule, c: &Cols, starred: &HashMap<String, String>, now: f64) -> bool {
        let num = match &r.value {
            Some(SmartValue::Num(n)) => *n,
            _ => 0.0,
        };
        let txt = match &r.value {
            Some(SmartValue::Text(t)) => t.as_str(),
            _ => "",
        };
        let num_cmp = |a: f64| match r.op {
            SmartRuleOp::Eq => a == num,
            SmartRuleOp::Gte => a >= num,
            _ => a <= num,
        };
        let text_match = |hay: &str| {
            let h = hay.to_lowercase();
            let nd = txt.trim().to_lowercase();
            match r.op {
                SmartRuleOp::Is => h == nd,
                SmartRuleOp::IsNot => h != nd,
                _ => h.contains(&nd),
            }
        };
        match r.field {
            SmartRuleField::Rating => {
                let rt = self.ratings.get(&(i as u32));
                if r.op == SmartRuleOp::Unrated { rt.is_none() } else { rt.is_some_and(|v| num_cmp(*v as f64)) }
            }
            SmartRuleField::Starred => {
                let s = starred.contains_key(&format!("tr_{i}"));
                if r.op == SmartRuleOp::Yes { s } else { !s }
            }
            SmartRuleField::PlayCount => num_cmp(c.play_count[i] as f64),
            SmartRuleField::LastPlayed => {
                if r.op == SmartRuleOp::Never {
                    return c.play_count[i] == 0 && c.played[i] == 0.0;
                }
                if c.played[i] == 0.0 {
                    return false;
                }
                let cutoff = now - num * 86_400_000.0;
                if r.op == SmartRuleOp::WithinDays { c.played[i] >= cutoff } else { c.played[i] < cutoff }
            }
            SmartRuleField::DateAdded => {
                if c.created[i] == 0.0 {
                    return false;
                }
                let cutoff = now - num * 86_400_000.0;
                if r.op == SmartRuleOp::WithinDays { c.created[i] >= cutoff } else { c.created[i] < cutoff }
            }
            SmartRuleField::Genre => text_match(&c.genres[c.genre[i] as usize]),
            SmartRuleField::Artist => text_match(&library::artist_name(c.artist[i] as usize)),
            SmartRuleField::Format => text_match(&c.suffixes[c.suffix[i] as usize]),
            SmartRuleField::Year => c.year[i] > 0 && num_cmp(c.year[i] as f64),
            SmartRuleField::Duration => num_cmp(c.duration[i] as f64),
            SmartRuleField::BitDepth => c.bit_depth[i] > 0 && num_cmp(c.bit_depth[i] as f64),
            SmartRuleField::SampleRate => c.rate[i] > 0 && num_cmp(c.rate[i] as f64),
        }
    }

    /// Matching track indices in the definition's sort order, capped by `limit` (`evalSmart`).
    pub fn eval(&mut self, lib: &library::Library, starred: &HashMap<String, String>, def: &SmartInput, seed: u32) -> Arc<Vec<u32>> {
        let key = format!("{}|{}|{:?}", self.rev, seed, def);
        if let Some(h) = self.memo.get(&key) {
            return h.clone();
        }
        let c = self.cols(lib);
        let now = EPOCH_MS as f64;
        let mut idx: Vec<u32> = Vec::new();
        for i in 0..c.n {
            let ok = if def.rules.is_empty() {
                true
            } else if def.match_mode == SmartMatchMode::All {
                def.rules.iter().all(|r| self.rule_matches(i, r, &c, starred, now))
            } else {
                def.rules.iter().any(|r| self.rule_matches(i, r, &c, starred, now))
            };
            if ok {
                idx.push(i as u32);
            }
        }
        let rt = |i: u32| self.ratings.get(&i).copied().unwrap_or(0) as i64;
        match def.sort {
            SmartSort::Random => {
                let m = ((seed as f64 * 2_654_435_761.0) % 2_147_483_645.0) as u64 + 2;
                idx.sort_by_key(|i| ((*i as u64 + 1) * m) % 2_147_483_647);
            }
            SmartSort::RecentlyAdded => idx.sort_by(|a, b| c.created[*b as usize].total_cmp(&c.created[*a as usize]).then(a.cmp(b))),
            SmartSort::MostPlayed => idx.sort_by(|a, b| c.play_count[*b as usize].cmp(&c.play_count[*a as usize]).then(a.cmp(b))),
            SmartSort::RecentlyPlayed => idx.sort_by(|a, b| c.played[*b as usize].total_cmp(&c.played[*a as usize]).then(a.cmp(b))),
            SmartSort::Rating => idx.sort_by(|a, b| rt(*b).cmp(&rt(*a)).then(a.cmp(b))),
            SmartSort::Artist => idx.sort_by(|a, b| {
                library::artist_name(c.artist[*a as usize] as usize)
                    .to_lowercase()
                    .cmp(&library::artist_name(c.artist[*b as usize] as usize).to_lowercase())
                    .then(a.cmp(b))
            }),
            SmartSort::Title => idx.sort_by(|a, b| c.title_key[*a as usize].cmp(&c.title_key[*b as usize]).then(a.cmp(b))),
        }
        if let Some(l) = def.limit.filter(|l| *l > 0) {
            idx.truncate(l as usize);
        }
        if self.memo.len() > 12 {
            self.memo.clear();
        }
        let a = Arc::new(idx);
        self.memo.insert(key, a.clone());
        a
    }

    pub fn duration_of(&mut self, lib: &library::Library, idx: &[u32]) -> f64 {
        let c = self.cols(lib);
        idx.iter().map(|i| c.duration[*i as usize] as f64).sum()
    }
}

pub fn validate(def: &SmartInput) -> ApiResult<()> {
    if def.name.trim().is_empty() {
        return Err(ApiError::new(ErrorCode::Unknown, "smart playlist name must be 1-100 characters"));
    }
    if def.rules.len() > 20 {
        return Err(ApiError::new(ErrorCode::Unknown, "at most 20 rules"));
    }
    if let Some(l) = def.limit
        && !(1..=10_000).contains(&l)
    {
        return Err(ApiError::new(ErrorCode::Unknown, "limit must be 1-10000"));
    }
    Ok(())
}

/// `MOCK_PRESETS`
pub fn presets() -> Vec<SmartPreset> {
    let p = |key: &str, name: &str, desc: &str, rule: SmartRule, sort: SmartSort, limit: Option<u32>| SmartPreset {
        key: key.into(),
        description: desc.into(),
        input: SmartInput { name: name.into(), match_mode: SmartMatchMode::All, rules: vec![rule], sort, limit },
    };
    let r = |field, op, v: Option<f64>| SmartRule { field, op, value: v.map(SmartValue::Num) };
    vec![
        p("hi-res", "Hi-Res only", "Sampling rate of 88.2 kHz or higher.", r(SmartRuleField::SampleRate, SmartRuleOp::Gte, Some(88200.0)), SmartSort::Artist, None),
        p("24-bit", "24-bit only", "Tracks with a 24-bit or deeper sample format.", r(SmartRuleField::BitDepth, SmartRuleOp::Gte, Some(24.0)), SmartSort::Artist, None),
        p("top-rated", "Top rated (4\u{2605}+)", "Rated four or five stars, best first.", r(SmartRuleField::Rating, SmartRuleOp::Gte, Some(4.0)), SmartSort::Rating, None),
        p(
            "stale",
            "Not played in 6 months",
            "Played before, but not within the last 180 days. 200 random picks.",
            r(SmartRuleField::LastPlayed, SmartRuleOp::OlderThanDays, Some(180.0)),
            SmartSort::Random,
            Some(200),
        ),
        p(
            "recent",
            "Recently added",
            "Added to the library in the last 30 days, newest first.",
            r(SmartRuleField::DateAdded, SmartRuleOp::WithinDays, Some(30.0)),
            SmartSort::RecentlyAdded,
            Some(500),
        ),
        p("never-played", "Never played", "No recorded plays. 500 random picks.", r(SmartRuleField::LastPlayed, SmartRuleOp::Never, None), SmartSort::Random, Some(500)),
    ]
}

/// History as a flat day-grouped list (`mockHistoryItems`): days newest first, rows in stored order.
pub(crate) fn history_items(history: &[(u32, i64)], track: &dyn Fn(u32) -> Track) -> Vec<HistoryItem> {
    let mut groups: Vec<(String, Vec<(u32, i64)>)> = Vec::new();
    for e in history {
        let d = local_day(e.1);
        match groups.iter_mut().find(|(k, _)| *k == d) {
            Some((_, g)) => g.push(*e),
            None => groups.push((d, vec![*e])),
        }
    }
    groups.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = Vec::new();
    for (d, g) in groups {
        out.push(HistoryItem::Day { day: d, count: g.len() as u32 });
        for (ix, at) in g {
            out.push(HistoryItem::Track { track: Box::new(track(ix)), played_at: at });
        }
    }
    out
}
