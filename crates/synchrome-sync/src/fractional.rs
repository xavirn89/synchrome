//! Fractional indexing for sibling order (`docs/03` §3.1).
//!
//! A key is a base-62 fraction `0.d1d2…` written with the digits `0-9A-Za-z`, whose ASCII
//! order equals numeric order. Keys never end in `0`, so string comparison equals fraction
//! comparison and there is always room below a key.

const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

fn val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'A'..=b'Z' => Some(c - b'A' + 10),
        b'a'..=b'z' => Some(c - b'a' + 36),
        _ => None,
    }
}

/// Whether `k` is a well-formed key.
pub fn is_valid(k: &str) -> bool {
    !k.is_empty() && !k.ends_with('0') && k.bytes().all(|c| val(c).is_some())
}

fn digits(k: &str) -> Vec<u8> {
    k.bytes().map(|c| val(c).unwrap_or(0)).collect()
}

fn render(d: &[u8]) -> String {
    d.iter().map(|&x| DIGITS[x as usize] as char).collect()
}

/// Midpoint of `a` (empty = 0) and `b` (None = 1), as digit vectors. Requires a < b.
fn midpoint(a: &[u8], b: Option<&[u8]>) -> Vec<u8> {
    if let Some(b) = b {
        // shared prefix (a padded with zeros)
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(0) == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            let rest_a = if a.len() > n { &a[n..] } else { &[][..] };
            out.extend(midpoint(rest_a, Some(&b[n..])));
            return out;
        }
    }
    let da = a.first().copied().unwrap_or(0) as u16;
    let db = b.map(|b| b[0] as u16).unwrap_or(62);
    if db - da > 1 {
        vec![((da + db) / 2) as u8]
    } else if let Some(b) = b.filter(|b| b.len() > 1) {
        vec![b[0]]
    } else {
        let mut out = vec![da as u8];
        let rest = if a.len() > 1 { &a[1..] } else { &[][..] };
        out.extend(midpoint(rest, None));
        out
    }
}

/// A key strictly between `lo` and `hi` (either may be absent = open end).
/// Panics in debug builds if `lo >= hi`.
pub fn key_between(lo: Option<&str>, hi: Option<&str>) -> String {
    debug_assert!(
        matches!((lo, hi), (Some(l), Some(h)) if l < h) || lo.is_none() || hi.is_none(),
        "key_between requires lo < hi"
    );
    let a = lo.map(digits).unwrap_or_default();
    let b = hi.map(digits);
    render(&midpoint(&a, b.as_deref()))
}

/// `n` increasing keys strictly between `lo` and `hi`, by balanced bisection.
pub fn keys_between(lo: Option<&str>, hi: Option<&str>, n: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(n);
    fill(lo.map(str::to_string), hi.map(str::to_string), n, &mut out);
    out
}

fn fill(lo: Option<String>, hi: Option<String>, n: usize, out: &mut Vec<String>) {
    if n == 0 {
        return;
    }
    let mid = key_between(lo.as_deref(), hi.as_deref());
    let left = (n - 1) / 2;
    fill(lo, Some(mid.clone()), left, out);
    out.push(mid.clone());
    fill(Some(mid), hi, n - 1 - left, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn between_is_strict_and_valid() {
        let cases: &[(Option<&str>, Option<&str>)] = &[
            (None, None),
            (None, Some("V")),
            (Some("V"), None),
            (Some("V"), Some("W")),
            (Some("V"), Some("V1")),
            (Some("z"), None),
            (Some("zzz"), None),
            (None, Some("01")),
            (Some("A0B"), Some("A0C")),
        ];
        for &(lo, hi) in cases {
            let k = key_between(lo, hi);
            assert!(is_valid(&k), "{k:?} invalid for {lo:?}..{hi:?}");
            if let Some(l) = lo {
                assert!(l < k.as_str(), "{l} < {k}");
            }
            if let Some(h) = hi {
                assert!(k.as_str() < h, "{k} < {h}");
            }
        }
    }

    #[test]
    fn bulk_keys_are_sorted_unique_and_short() {
        let ks = keys_between(None, None, 10_000);
        assert_eq!(ks.len(), 10_000);
        assert!(ks.windows(2).all(|w| w[0] < w[1]));
        assert!(ks.iter().all(|k| is_valid(k)));
        let longest = ks.iter().map(String::len).max().unwrap();
        assert!(longest <= 6, "bisection keeps keys short, got {longest}");
    }

    #[test]
    fn repeated_insertion_at_the_front_stays_ordered() {
        let mut first = key_between(None, None);
        for _ in 0..200 {
            let k = key_between(None, Some(&first));
            assert!(k < first && is_valid(&k));
            first = k;
        }
    }
}
