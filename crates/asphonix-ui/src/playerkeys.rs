//! The single table behind the Now Playing legend and key handling.
use asphonix_i18n::Key;

pub struct PlayerKey {
    pub keys: &'static [&'static str],
    pub legend: &'static str,
    /// translated at render time (shown upper-cased)
    pub label: Key,
    pub in_legend: bool,
}

impl PlayerKey {
    /// The key cap as shown: letters as they are, Space in the UI language.
    pub fn legend_text(&self) -> String {
        if self.keys[0] == " " { asphonix_i18n::tr(Key::KbdSpace).to_uppercase() } else { self.legend.to_string() }
    }
}

pub const PLAYER_KEYS: [PlayerKey; 10] = [
    PlayerKey { keys: &[" "], legend: "SPACE", label: Key::HomeKeyPlayPause, in_legend: true },
    PlayerKey { keys: &["t"], legend: "T", label: Key::NpLyrics, in_legend: true },
    PlayerKey { keys: &["m"], legend: "M", label: Key::VolumeMute, in_legend: true },
    PlayerKey { keys: &["l"], legend: "L", label: Key::MenuLike, in_legend: true },
    PlayerKey { keys: &["a"], legend: "A", label: Key::NpLoop, in_legend: true },
    PlayerKey { keys: &["j"], legend: "J", label: Key::NpBack10, in_legend: true },
    PlayerKey { keys: &["k"], legend: "K", label: Key::NpForward10, in_legend: true },
    PlayerKey { keys: &["z"], legend: "Z", label: Key::NpPrev, in_legend: true },
    PlayerKey { keys: &["b"], legend: "B", label: Key::NpNext, in_legend: true },
    PlayerKey { keys: &["escape"], legend: "ESC", label: Key::NpMinimize, in_legend: true },
];
