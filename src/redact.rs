//! Text made safe to send to Jev: terminal output goes out only after secrets are blanked and
//! it is cut to size. Shared by every Jev feature that sends screen text.

/// The first `n` chars.
pub fn clip(t: &str, n: usize) -> String {
    t.chars().take(n).collect()
}

/// The last `n` chars.
pub fn tail(t: &str, n: usize) -> String {
    let len = t.chars().count();
    t.chars().skip(len.saturating_sub(n)).collect()
}

const REDACTED: &str = "[redacted]";
const PREFIXES: [&str; 9] = ["sk-", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxp-"];

fn tokenish(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

/// Blank out token-shaped secrets inside one whitespace-free word.
fn redact_tokens(w: &str) -> String {
    let cs: Vec<char> = w.chars().collect();
    let mut o = String::with_capacity(w.len());
    let mut i = 0;
    while i < cs.len() {
        let boundary = i == 0 || !cs[i - 1].is_ascii_alphanumeric();
        let hit = boundary.then(|| PREFIXES.iter().find(|p| cs[i..].iter().take(p.len()).copied().eq(p.chars()))).flatten();
        if let Some(p) = hit {
            let body = cs[i + p.len()..].iter().take_while(|c| tokenish(**c)).count();
            if body >= 8 {
                o.push_str(REDACTED);
                i += p.len() + body;
                continue;
            }
        }
        o.push(cs[i]);
        i += 1;
    }
    o
}

/// `NAME=value` shaped like an environment variable or a secret's name.
fn assignment(w: &str) -> Option<usize> {
    let eq = w.find('=')?;
    let name = w[..eq].trim_start_matches('-');
    if name.is_empty() || eq + 1 >= w.len() {
        return None;
    }
    let env = name.len() >= 2 && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && name.chars().any(|c| c.is_ascii_uppercase());
    let lower = name.to_lowercase();
    let secret = name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) && ["token", "secret", "password", "passwd", "key", "auth", "credential"].iter().any(|k| lower.contains(k));
    (env || secret).then_some(eq)
}

/// Terminal text made safe to send: `sk-…`, `ghp_…`, `github_pat_…`, `Bearer …` and anything
/// shaped like `KEY=value` are blanked.
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.lines().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut after_bearer = false;
        let mut chars = line.char_indices().peekable();
        while let Some(&(i, c)) = chars.peek() {
            if c.is_whitespace() {
                out.push(c);
                chars.next();
                continue;
            }
            let mut end = line.len();
            while let Some(&(j, d)) = chars.peek() {
                if d.is_whitespace() {
                    end = j;
                    break;
                }
                chars.next();
            }
            let w = &line[i..end];
            if after_bearer {
                out.push_str(REDACTED);
            } else if let Some(eq) = assignment(w) {
                out.push_str(&w[..=eq]);
                out.push_str(REDACTED);
            } else {
                out.push_str(&redact_tokens(w));
            }
            after_bearer = w.eq_ignore_ascii_case("bearer");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_redacted_before_anything_is_sent() {
        let t = "token sk-abcdefghijklmnop and ghp_ABCDEFGH12345678 / github_pat_11ABCDEFG0123456789_xyz\nAuthorization: Bearer eyJhbGciOi.payload\nexport OPENAI_API_KEY=abc123 --api-key=zzz FOO_BAR=1\ndb_password=hunter2 normal=thing";
        let r = redact(t);
        for leak in ["abcdefghijklmnop", "ABCDEFGH12345678", "11ABCDEFG", "eyJhbGciOi", "abc123", "zzz", "hunter2", "FOO_BAR=1"] {
            assert!(!r.contains(leak), "{leak} leaked: {r}");
        }
        assert!(r.contains("OPENAI_API_KEY=[redacted]") && r.contains("normal=thing") && r.contains("Bearer [redacted]"));
        assert_eq!(r.lines().count(), 4);
        // plain words and short look-alikes stay
        assert_eq!(redact("the task-list for sk-learn, risk-based"), "the task-list for sk-learn, risk-based");
        assert_eq!(redact("see https://x.test/?a=b"), "see https://x.test/?a=b");
    }

    #[test]
    fn clip_and_tail_count_chars() {
        assert_eq!(clip("héllo", 3), "hél");
        assert_eq!(tail("héllo", 3), "llo");
        assert_eq!(tail("ab", 9), "ab");
    }
}
