//! Field/operator metadata of the smart-playlist rule builder and the human-readable
//! rule descriptions ("Rating ≥ 4★ AND Liked · by rating · max 200").
use asphonix_api::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Num,
    Text,
    None,
}

pub struct FieldMeta {
    pub label: &'static str,
    pub kind: Kind,
    pub ops: &'static [SmartRuleOp],
    pub unit: &'static str,
    pub placeholder: &'static str,
    /// default numeric value (None: empty / text)
    pub def: Option<f64>,
}

use SmartRuleOp as O;

pub fn meta(f: SmartRuleField) -> FieldMeta {
    let m = |label, kind, ops: &'static [SmartRuleOp], unit, placeholder, def| FieldMeta { label, kind, ops, unit, placeholder, def };
    match f {
        SmartRuleField::Rating => m("Rating", Kind::Num, &[O::Gte, O::Lte, O::Eq, O::Unrated], "★ (1-5)", "", Some(4.0)),
        SmartRuleField::Starred => m("Liked (starred)", Kind::None, &[O::Yes, O::No], "", "", None),
        SmartRuleField::PlayCount => m("Play count", Kind::Num, &[O::Eq, O::Gte, O::Lte], "plays", "", Some(0.0)),
        SmartRuleField::LastPlayed => m("Last played", Kind::Num, &[O::Never, O::OlderThanDays, O::WithinDays], "days", "", Some(180.0)),
        SmartRuleField::DateAdded => m("Added to library", Kind::Num, &[O::WithinDays, O::OlderThanDays], "days", "", Some(30.0)),
        SmartRuleField::Genre => m("Genre", Kind::Text, &[O::Is, O::Contains, O::IsNot], "", "e.g. Jazz", None),
        SmartRuleField::Year => m("Year", Kind::Num, &[O::Gte, O::Lte, O::Eq], "", "", Some(2000.0)),
        SmartRuleField::Artist => m("Artist", Kind::Text, &[O::Is, O::Contains, O::IsNot], "", "e.g. Aphex Twin", None),
        SmartRuleField::Duration => m("Duration", Kind::Num, &[O::Gte, O::Lte], "seconds", "", Some(300.0)),
        SmartRuleField::Format => m("Format", Kind::Text, &[O::Is, O::IsNot], "", "flac, mp3, m4a…", None),
        SmartRuleField::BitDepth => m("Bit depth", Kind::Num, &[O::Gte, O::Eq, O::Lte], "bit", "", Some(24.0)),
        SmartRuleField::SampleRate => m("Sample rate", Kind::Num, &[O::Gte, O::Eq, O::Lte], "Hz", "", Some(88200.0)),
    }
}

pub const FIELD_ORDER: [SmartRuleField; 12] = [
    SmartRuleField::Rating,
    SmartRuleField::Starred,
    SmartRuleField::PlayCount,
    SmartRuleField::LastPlayed,
    SmartRuleField::DateAdded,
    SmartRuleField::Genre,
    SmartRuleField::Year,
    SmartRuleField::Artist,
    SmartRuleField::Duration,
    SmartRuleField::Format,
    SmartRuleField::BitDepth,
    SmartRuleField::SampleRate,
];

pub fn op_label(field: SmartRuleField, op: SmartRuleOp) -> &'static str {
    match op {
        O::Eq => "is exactly",
        O::Gte => {
            if field == SmartRuleField::DateAdded {
                "after"
            } else {
                "at least (≥)"
            }
        }
        O::Lte => "at most (≤)",
        O::Unrated => "is unrated",
        O::Yes => "yes",
        O::No => "no",
        O::Never => "never",
        O::OlderThanDays => "more than … days ago",
        O::WithinDays => "within the last … days",
        O::Is => "is",
        O::IsNot => "is not",
        O::Contains => "contains",
    }
}

pub fn sort_label(s: SmartSort) -> &'static str {
    match s {
        SmartSort::Random => "Random",
        SmartSort::RecentlyAdded => "Recently added",
        SmartSort::MostPlayed => "Most played",
        SmartSort::RecentlyPlayed => "Recently played",
        SmartSort::Title => "Title",
        SmartSort::Artist => "Artist",
        SmartSort::Rating => "Rating",
    }
}

pub const SORT_ORDER: [SmartSort; 7] =
    [SmartSort::Random, SmartSort::RecentlyAdded, SmartSort::MostPlayed, SmartSort::RecentlyPlayed, SmartSort::Title, SmartSort::Artist, SmartSort::Rating];

/// Number formatting for rule descriptions: whole numbers without a fraction, a missing value as "undefined".
fn num_str(v: &Option<SmartValue>) -> String {
    match v {
        Some(SmartValue::Num(n)) if n.fract() == 0.0 => format!("{}", *n as i64),
        Some(SmartValue::Num(n)) => format!("{n}"),
        Some(SmartValue::Text(t)) => t.clone(),
        None => "undefined".into(),
    }
}

pub fn describe_rule(r: &SmartRule) -> String {
    let f = meta(r.field);
    let v = num_str(&r.value);
    match r.op {
        O::Unrated => "Unrated".into(),
        O::Yes => if r.field == SmartRuleField::Starred { "Liked" } else { "Yes" }.into(),
        O::No => "Not liked".into(),
        O::Never => "Never played".into(),
        O::OlderThanDays => format!("{}: more than {v} days ago", f.label),
        O::WithinDays => format!("{}: within {v} days", f.label),
        O::Is => format!("{} is {v}", f.label),
        O::IsNot => format!("{} is not {v}", f.label),
        O::Contains => format!("{} contains \"{v}\"", f.label),
        op => {
            let unit = match r.field {
                SmartRuleField::Rating => "★",
                SmartRuleField::BitDepth => "-bit",
                SmartRuleField::SampleRate => " Hz",
                SmartRuleField::Duration => " s",
                _ => "",
            };
            let cmp = match op {
                O::Eq => "=",
                O::Gte => "≥",
                _ => "≤",
            };
            format!("{} {cmp} {v}{unit}", f.label)
        }
    }
}

pub fn describe_smart(d: &SmartInput) -> String {
    let rules = if d.rules.is_empty() {
        "All tracks".to_string()
    } else {
        d.rules.iter().map(describe_rule).collect::<Vec<_>>().join(if d.match_mode == SmartMatchMode::All { " AND " } else { " OR " })
    };
    let sort = if d.sort == SmartSort::Random { "random".to_string() } else { format!("by {}", sort_label(d.sort).to_lowercase()) };
    let limit = d.limit.map(|l| format!(" · max {}", asphonix_api::format::fmt_int(l as u64))).unwrap_or_default();
    format!("{rules} · {sort}{limit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn describes_like_the_ts() {
        let d = SmartInput {
            name: "x".into(),
            match_mode: SmartMatchMode::All,
            rules: vec![SmartRule { field: SmartRuleField::Rating, op: O::Gte, value: Some(SmartValue::Num(4.0)) }, SmartRule { field: SmartRuleField::Starred, op: O::Yes, value: None }],
            sort: SmartSort::Rating,
            limit: Some(200),
        };
        assert_eq!(describe_smart(&d), "Rating ≥ 4★ AND Liked · by rating · max 200");
    }
}
