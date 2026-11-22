//! The `en-XA` pseudo-locale: generated from the English messages when it is first selected (never shipped as a
//! file). Fluent's transform hook accents the message *text* only (placeables, i.e. argument values, stay as
//! they are); [`wrap`] then brackets the whole message and pads it to ~140 % so truncation shows up.
use std::borrow::Cow;

/// Latin-1 / Latin Extended-A look-alikes only (Inter, Space Grotesk and JetBrains Mono all cover these).
fn accent_char(c: char) -> char {
    match c {
        'a' => 'å',
        'c' => 'ç',
        'd' => 'ð',
        'e' => 'é',
        'g' => 'ĝ',
        'h' => 'ĥ',
        'i' => 'î',
        'j' => 'ĵ',
        'k' => 'ķ',
        'l' => 'ļ',
        'n' => 'ñ',
        'o' => 'ö',
        'p' => 'þ',
        'r' => 'ŕ',
        's' => 'š',
        't' => 'ţ',
        'u' => 'û',
        'w' => 'ŵ',
        'y' => 'ý',
        'z' => 'ž',
        'A' => 'Å',
        'C' => 'Ç',
        'D' => 'Ð',
        'E' => 'É',
        'G' => 'Ĝ',
        'H' => 'Ĥ',
        'I' => 'Î',
        'J' => 'Ĵ',
        'K' => 'Ķ',
        'L' => 'Ļ',
        'N' => 'Ñ',
        'O' => 'Ö',
        'R' => 'Ŕ',
        'S' => 'Š',
        'T' => 'Ţ',
        'U' => 'Û',
        'W' => 'Ŵ',
        'Y' => 'Ý',
        'Z' => 'Ž',
        c => c,
    }
}

/// Fluent transform: accents every text element of a message.
pub fn accent(s: &str) -> Cow<'_, str> {
    if s.chars().any(|c| accent_char(c) != c) { Cow::Owned(s.chars().map(accent_char).collect()) } else { Cow::Borrowed(s) }
}

/// `[text~~~~]`: brackets plus `~` padding to about 140 % of the original length. Empty stays empty.
pub fn wrap(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let n = s.chars().count();
    let pad = (n * 2).div_ceil(5).max(1);
    let mut out = String::with_capacity(s.len() + pad + 2);
    out.push('[');
    out.push_str(s);
    out.extend(std::iter::repeat_n('~', pad));
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accents_and_wraps() {
        assert_eq!(accent("New playlist"), "Ñéŵ þļåýļîšţ");
        assert_eq!(accent("123 · --:--"), "123 · --:--");
        assert_eq!(wrap("Settings"), "[Settings~~~~]");
        assert_eq!(wrap(""), "");
    }
}
