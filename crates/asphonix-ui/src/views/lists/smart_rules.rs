//! Field/operator metadata of the smart-playlist rule builder and the human-readable
//! rule descriptions ("Rating ≥ 4★ AND Liked · by rating · max 200"), in the active UI language.
use asphonix_api::*;
use asphonix_i18n::tr;

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

/// Labels and units come from the active catalog (`&'static str`, a table lookup), so the editor re-labels on a
/// language switch.
pub fn meta(f: SmartRuleField) -> FieldMeta {
    let m = |label, kind, ops: &'static [SmartRuleOp], unit, placeholder, def| FieldMeta { label, kind, ops, unit, placeholder, def };
    match f {
        SmartRuleField::Rating => m(tr!(SmartFieldRating), Kind::Num, &[O::Gte, O::Lte, O::Eq, O::Unrated], "★ (1-5)", "", Some(4.0)),
        SmartRuleField::Starred => m(tr!(SmartFieldStarred), Kind::None, &[O::Yes, O::No], "", "", None),
        SmartRuleField::PlayCount => m(tr!(SmartFieldPlayCount), Kind::Num, &[O::Eq, O::Gte, O::Lte], tr!(SmartUnitPlays), "", Some(0.0)),
        SmartRuleField::LastPlayed => m(tr!(SmartFieldLastPlayed), Kind::Num, &[O::Never, O::OlderThanDays, O::WithinDays], tr!(SmartUnitDays), "", Some(180.0)),
        SmartRuleField::DateAdded => m(tr!(SmartFieldDateAdded), Kind::Num, &[O::WithinDays, O::OlderThanDays], tr!(SmartUnitDays), "", Some(30.0)),
        SmartRuleField::Genre => m(tr!(InfoGenre), Kind::Text, &[O::Is, O::Contains, O::IsNot], "", tr!(SmartPlaceholderGenre), None),
        SmartRuleField::Year => m(tr!(InfoYear), Kind::Num, &[O::Gte, O::Lte, O::Eq], "", "", Some(2000.0)),
        SmartRuleField::Artist => m(tr!(ColArtist), Kind::Text, &[O::Is, O::Contains, O::IsNot], "", tr!(SmartPlaceholderArtist), None),
        SmartRuleField::Duration => m(tr!(InfoDuration), Kind::Num, &[O::Gte, O::Lte], tr!(SmartUnitSeconds), "", Some(300.0)),
        SmartRuleField::Format => m(tr!(InfoFormat), Kind::Text, &[O::Is, O::IsNot], "", "flac, mp3, m4a…", None),
        SmartRuleField::BitDepth => m(tr!(InfoBitDepth), Kind::Num, &[O::Gte, O::Eq, O::Lte], "bit", "", Some(24.0)),
        SmartRuleField::SampleRate => m(tr!(InfoSampleRate), Kind::Num, &[O::Gte, O::Eq, O::Lte], "Hz", "", Some(88200.0)),
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
        O::Eq => tr!(SmartOpEq),
        O::Gte => {
            if field == SmartRuleField::DateAdded {
                tr!(SmartOpAfter)
            } else {
                tr!(SmartOpGte)
            }
        }
        O::Lte => tr!(SmartOpLte),
        O::Unrated => tr!(SmartOpUnrated),
        O::Yes => tr!(SmartOpYes),
        O::No => tr!(SmartOpNo),
        O::Never => tr!(SmartOpNever),
        O::OlderThanDays => tr!(SmartOpOlderThanDays),
        O::WithinDays => tr!(SmartOpWithinDays),
        O::Is => tr!(SmartOpIs),
        O::IsNot => tr!(SmartOpIsNot),
        O::Contains => tr!(SmartOpContains),
    }
}

pub fn sort_label(s: SmartSort) -> &'static str {
    match s {
        SmartSort::Random => tr!(SortRandom),
        SmartSort::RecentlyAdded => tr!(SmartSortRecentlyAdded),
        SmartSort::MostPlayed => tr!(ArtistMostPlayed),
        SmartSort::RecentlyPlayed => tr!(SmartSortRecentlyPlayed),
        SmartSort::Title => tr!(ColTitle),
        SmartSort::Artist => tr!(ColArtist),
        SmartSort::Rating => tr!(SmartFieldRating),
    }
}

/// The sort as the description's tail ("by rating" / "nach Bewertung").
fn sort_phrase(s: SmartSort) -> &'static str {
    match s {
        SmartSort::Random => tr!(SmartDescRandom),
        SmartSort::RecentlyAdded => tr!(SmartDescByRecentlyAdded),
        SmartSort::MostPlayed => tr!(SmartDescByMostPlayed),
        SmartSort::RecentlyPlayed => tr!(SmartDescByRecentlyPlayed),
        SmartSort::Title => tr!(SmartDescByTitle),
        SmartSort::Artist => tr!(SmartDescByArtist),
        SmartSort::Rating => tr!(SmartDescByRating),
    }
}

pub const SORT_ORDER: [SmartSort; 7] =
    [SmartSort::Random, SmartSort::RecentlyAdded, SmartSort::MostPlayed, SmartSort::RecentlyPlayed, SmartSort::Title, SmartSort::Artist, SmartSort::Rating];

/// Number formatting for rule descriptions: whole numbers without a fraction (locale decimal separator otherwise),
/// a missing value as "undefined".
fn num_str(v: &Option<SmartValue>) -> String {
    match v {
        Some(SmartValue::Num(n)) if n.fract() == 0.0 => format!("{}", *n as i64),
        Some(SmartValue::Num(n)) => asphonix_i18n::fmt::decimal(&format!("{n}")),
        Some(SmartValue::Text(t)) => t.clone(),
        None => tr!(SmartUndefined).into(),
    }
}

pub fn describe_rule(r: &SmartRule) -> String {
    let f = meta(r.field);
    let v = num_str(&r.value);
    let label = f.label;
    match r.op {
        O::Unrated => tr!(SmartDescUnrated).into(),
        O::Yes => if r.field == SmartRuleField::Starred { tr!(SmartDescLiked) } else { tr!(SmartDescYes) }.into(),
        O::No => tr!(SmartDescNotLiked).into(),
        O::Never => tr!(SmartDescNeverPlayed).into(),
        O::OlderThanDays => tr!(SmartDescOlderThan, field = label, value = v),
        O::WithinDays => tr!(SmartDescWithin, field = label, value = v),
        O::Is => tr!(SmartDescIs, field = label, value = v),
        O::IsNot => tr!(SmartDescIsNot, field = label, value = v),
        O::Contains => tr!(SmartDescContains, field = label, value = v),
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
            format!("{label} {cmp} {v}{unit}")
        }
    }
}

pub fn describe_smart(d: &SmartInput) -> String {
    let rules = if d.rules.is_empty() {
        tr!(SmartDescAllTracks).to_string()
    } else {
        let join = if d.match_mode == SmartMatchMode::All { tr!(SmartDescAnd) } else { tr!(SmartDescOr) };
        d.rules.iter().map(describe_rule).collect::<Vec<_>>().join(&format!(" {join} "))
    };
    let limit = d.limit.map(|l| format!(" · {}", tr!(SmartDescMax, count = l))).unwrap_or_default();
    format!("{rules} · {}{limit}", sort_phrase(d.sort))
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
