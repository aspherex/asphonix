//! Secrets out of log lines. Every place that logs a URL or request goes through [`redact_url`].
//!
//! Subsonic authenticates every request in the query string (`u` user, `t` token = md5(password + salt),
//! `s` salt, legacy `p` password, OpenSubsonic `apiKey`), so a logged request URL is a credential.

/// Query keys whose values never reach a log.
const SECRET_KEYS: &[&str] = &["u", "t", "s", "p", "apiKey", "token"];

pub const REDACTED: &str = "REDACTED";

/// `url` with the values of [`SECRET_KEYS`] and any `user:password@` userinfo replaced by `REDACTED`.
/// Works on any string (also unparsable URLs); everything else is kept, so the endpoint and harmless
/// parameters (`id`, `format`, `size`, ...) stay useful.
pub fn redact_url(url: &str) -> String {
    let (head, fragment) = match url.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (url, None),
    };
    let (base, query) = match head.split_once('?') {
        Some((b, q)) => (b, Some(q)),
        None => (head, None),
    };
    let mut out = redact_userinfo(base);
    if let Some(q) = query {
        out.push('?');
        let pairs: Vec<String> = q
            .split('&')
            .map(|pair| {
                let key = pair.split_once('=').map_or(pair, |(k, _)| k);
                if SECRET_KEYS.contains(&key) { format!("{key}={REDACTED}") } else { pair.to_string() }
            })
            .collect();
        out.push_str(&pairs.join("&"));
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// `scheme://user:pass@host/...` -> `scheme://REDACTED@host/...`
fn redact_userinfo(base: &str) -> String {
    let Some(scheme_end) = base.find("://").map(|i| i + 3) else { return base.to_string() };
    let rest = &base[scheme_end..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{}{REDACTED}{}", &base[..scheme_end], &rest[at..]),
        None => base.to_string(),
    }
}

/// Test helper: `text` (captured log output) contains none of `secrets`, and every `u`/`t`/`s`/`p` query
/// parameter in it is redacted.
#[cfg(test)]
pub(crate) fn assert_redacted(text: &str, secrets: &[&str]) {
    for s in secrets {
        assert!(!text.contains(s), "secret {s:?} in log output:
{text}");
    }
    for key in ["u", "t", "s", "p"] {
        for sep in ['?', '&'] {
            let pat = format!("{sep}{key}=");
            for (i, _) in text.match_indices(&pat) {
                assert!(text[i + pat.len()..].starts_with(REDACTED), "unredacted `{pat}` in log output:
{text}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_params_and_userinfo_are_redacted_the_rest_is_kept() {
        let u = "https://alice:hunter2@music.example.com:4533/rest/stream?u=alice&t=26719a1196d2a940705a59634eb18eab&s=c19b2d&v=1.16.1&c=sonix&f=json&id=tr-1&format=raw#x";
        let r = redact_url(u);
        assert_eq!(r, "https://REDACTED@music.example.com:4533/rest/stream?u=REDACTED&t=REDACTED&s=REDACTED&v=1.16.1&c=sonix&f=json&id=tr-1&format=raw#x");
        for secret in ["alice", "hunter2", "26719a1196d2a940705a59634eb18eab", "c19b2d"] {
            assert!(!r.contains(secret), "{secret} in {r}");
        }
    }

    #[test]
    fn legacy_password_api_key_and_odd_inputs() {
        assert_eq!(redact_url("http://h/rest/ping?p=enc:6869&apiKey=k1&token=x"), "http://h/rest/ping?p=REDACTED&apiKey=REDACTED&token=REDACTED");
        assert_eq!(redact_url("http://h/rest/ping"), "http://h/rest/ping");
        assert_eq!(redact_url("not a url ?t=abc"), "not a url ?t=REDACTED");
        assert_eq!(redact_url("http://h/a@b?id=1"), "http://h/a@b?id=1", "an @ in the path is not userinfo");
        assert_eq!(redact_url("http://h/x?t"), "http://h/x?t=REDACTED");
        // `st=` / `ts=` are not `s`/`t`
        assert_eq!(redact_url("http://h/x?st=1&ts=2"), "http://h/x?st=1&ts=2");
    }
}
