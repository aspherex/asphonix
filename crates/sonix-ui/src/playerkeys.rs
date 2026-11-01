//! `lib/playerKeys.ts` `PLAYER_KEYS`: the single table behind the Now Playing legend and key handling.
pub struct PlayerKey {
    pub keys: &'static [&'static str],
    pub legend: &'static str,
    pub label: &'static str,
    pub in_legend: bool,
}

pub const PLAYER_KEYS: [PlayerKey; 10] = [
    PlayerKey { keys: &[" "], legend: "SPACE", label: "PLAY/PAUSE", in_legend: true },
    PlayerKey { keys: &["t"], legend: "T", label: "LYRICS", in_legend: true },
    PlayerKey { keys: &["m"], legend: "M", label: "MUTE", in_legend: true },
    PlayerKey { keys: &["l"], legend: "L", label: "LIKE", in_legend: true },
    PlayerKey { keys: &["a"], legend: "A", label: "A↔B LOOP", in_legend: true },
    PlayerKey { keys: &["j"], legend: "J", label: "-10S", in_legend: true },
    PlayerKey { keys: &["k"], legend: "K", label: "+10S", in_legend: true },
    PlayerKey { keys: &["z"], legend: "Z", label: "PREV", in_legend: true },
    PlayerKey { keys: &["b"], legend: "B", label: "NEXT", in_legend: true },
    PlayerKey { keys: &["escape"], legend: "ESC", label: "MINIMIZE", in_legend: true },
];
