//! Small text / time helpers shared by the exporter.

use chrono::{DateTime, Local};
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

/// Walk a JSON value by keys, returning the string at the end (if any, and non-empty).
pub fn jstr<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_str().filter(|s| !s.is_empty())
}

/// Walk a JSON value by keys.
pub fn jget<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

/// JSON array at path, or empty slice.
pub fn jarr<'a>(v: &'a Value, path: &[&str]) -> &'a [Value] {
    jget(v, path)
        .and_then(|x| x.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Decode the HTML entities Teams actually emits (named basics + numeric).
pub fn html_decode(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);").unwrap());
    re.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        let ch = if let Some(hex) = e.strip_prefix("#x").or_else(|| e.strip_prefix("#X")) {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        } else if let Some(dec) = e.strip_prefix('#') {
            dec.parse::<u32>().ok().and_then(char::from_u32)
        } else {
            match e {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => None,
            }
        };
        match ch {
            Some(ch) => ch.to_string(),
            None => c[0].to_string(),
        }
    })
    .into_owned()
}

/// Convert Teams message HTML to readable plain text.
pub fn html_to_text(html: &str) -> String {
    static RES: OnceLock<[Regex; 5]> = OnceLock::new();
    let [br, block, img, tag, blank] = RES.get_or_init(|| {
        [
            Regex::new(r"(?i)<br\s*/?>").unwrap(),
            Regex::new(r"(?i)</(p|div|li|tr)>").unwrap(),
            Regex::new(r"(?i)<img[^>]*>").unwrap(),
            Regex::new(r"<[^>]+>").unwrap(),
            Regex::new(r"(\r?\n){3,}").unwrap(),
        ]
    });
    if html.trim().is_empty() {
        return String::new();
    }
    let t = br.replace_all(html, "\n");
    let t = block.replace_all(&t, "\n");
    let t = img.replace_all(&t, "[image]");
    let t = tag.replace_all(&t, "");
    let t = html_decode(&t);
    blank.replace_all(&t, "\n\n").trim().to_string()
}

/// Make a string safe to use as a Windows file / folder name.
pub fn sanitize_filename(name: &str, max: usize) -> String {
    let mut n: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    n = n.split_whitespace().collect::<Vec<_>>().join(" ");
    if n.chars().count() > max {
        n = n.chars().take(max).collect();
    }
    let n = n.trim().trim_end_matches(['.', ' ']).to_string();
    // Reserved device names on Windows
    let upper = n.to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"];
    let is_reserved = reserved.contains(&upper.as_str())
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if n.is_empty() {
        "chat".to_string()
    } else if is_reserved {
        format!("_{n}")
    } else {
        n
    }
}

pub fn parse_time(s: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Local))
}

/// ISO-8601 duration like PT1H5M12.5S -> "01:05:12".
pub fn iso_duration(s: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"^P(?:(\d+)D)?(?:T(?:(\d+)H)?(?:(\d+)M)?(?:([\d.]+)S)?)?$").unwrap()
    });
    let c = re.captures(s)?;
    let num = |i: usize| {
        c.get(i)
            .and_then(|m| m.as_str().parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    let total = (num(1) * 86400.0 + num(2) * 3600.0 + num(3) * 60.0 + num(4)).round() as u64;
    Some(format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    ))
}

/// Stable colour per sender name, for the HTML transcript.
pub fn name_color(name: &str) -> String {
    let mut h: u32 = 0;
    for ch in name.chars() {
        h = (h.wrapping_mul(31).wrapping_add(ch as u32)) % 360;
    }
    format!("hsl({h},45%,35%)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_conversion() {
        assert_eq!(
            html_to_text("<p>Hi <at id=\"0\">Mary</at> &amp; team</p><p>line2<br>line3</p>"),
            "Hi Mary & team\nline2\nline3"
        );
        assert_eq!(html_to_text("see <img src=\"x\">"), "see [image]");
        assert_eq!(
            html_decode("&#128077; &#x41; &nbsp;&bogus;"),
            "👍 A  &bogus;"
        );
    }

    #[test]
    fn filenames() {
        assert_eq!(
            sanitize_filename("Weekly sync: Budget/Q3", 80),
            "Weekly sync_ Budget_Q3"
        );
        assert_eq!(sanitize_filename("  ...  ", 80), "chat");
        assert_eq!(sanitize_filename("CON", 80), "_CON");
        assert_eq!(sanitize_filename("john@x.com", 80), "john@x.com");
        assert_eq!(sanitize_filename("abcdef", 3), "abc");
    }

    #[test]
    fn durations() {
        assert_eq!(iso_duration("PT5M12S").as_deref(), Some("00:05:12"));
        assert_eq!(iso_duration("PT1H0M3.6S").as_deref(), Some("01:00:04"));
        assert_eq!(iso_duration("junk"), None);
    }
}
