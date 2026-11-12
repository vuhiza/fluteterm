//! Claude Code usage across *all* sessions on this Mac (fluteterm tabs, other
//! terminals, Orca agents): reads `~/.claude/projects/**/*.jsonl`
//! incrementally and reports today's tokens and the current 5-hour usage
//! window (the window Claude subscription limits are counted in).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::util;

pub const BLOCK_SECS: u64 = 5 * 3600;

#[derive(Clone, Debug, Default)]
pub struct UsageSummary {
    /// Current 5h window: (start, end) unix seconds, if one is active.
    pub block: Option<(u64, u64)>,
    pub block_tokens: u64,
    pub block_messages: u64,
    pub today_tokens: u64,
    pub today_output: u64,
    pub last_activity: Option<u64>,
}

struct Entry {
    ts: u64,
    total: u64,
    output: u64,
}

#[derive(Default)]
pub struct Scanner {
    offsets: HashMap<PathBuf, u64>,
    entries: Vec<Entry>,
    seen: HashMap<String, u64>,
}

/// "2026-11-11T21:34:41.183Z" → unix seconds (UTC).
pub fn parse_iso(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hh * 3600 + mm * 60 + ss;
    (secs >= 0).then_some(secs as u64)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Unix time of the most recent local midnight.
fn local_midnight(now: u64) -> u64 {
    unsafe {
        let t = now as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        now - (tm.tm_hour as u64 * 3600 + tm.tm_min as u64 * 60 + tm.tm_sec as u64)
    }
}

fn walk(dir: &Path, depth: u8, since: SystemTime, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, since, out);
            }
        } else if p.extension().map(|x| x == "jsonl").unwrap_or(false)
            && meta.modified().map(|m| m >= since).unwrap_or(false)
        {
            out.push(p);
        }
    }
}

impl Scanner {
    pub fn scan(&mut self) -> UsageSummary {
        let now = now();
        let keep_from = local_midnight(now).min(now.saturating_sub(BLOCK_SECS * 2));
        let since = UNIX_EPOCH + Duration::from_secs(keep_from);
        let mut files = vec![];
        walk(&util::home().join(".claude").join("projects"), 4, since, &mut files);
        for f in files {
            self.read_new(&f);
        }
        self.entries.retain(|e| e.ts >= keep_from);
        self.seen.retain(|_, ts| *ts >= keep_from);
        self.entries.sort_by_key(|e| e.ts);
        self.summarise(now)
    }

    fn read_new(&mut self, path: &Path) {
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let mut off = self.offsets.get(path).copied().unwrap_or(0);
        if off > len {
            off = 0; // file was rewritten
        }
        if off == len {
            return;
        }
        if f.seek(SeekFrom::Start(off)).is_err() {
            return;
        }
        let mut buf = Vec::with_capacity((len - off) as usize);
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        // Only consume complete lines; a partially written line is re-read next time.
        let end = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
        for line in buf[..end].split(|&b| b == b'\n') {
            if line.len() < 20 || !contains(line, b"\"usage\"") {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            let msg = &v["message"];
            let usage = &msg["usage"];
            if !usage.is_object() {
                continue;
            }
            let Some(ts) = v["timestamp"].as_str().and_then(parse_iso) else { continue };
            // Streaming writes the same response several times: count it once.
            let id = format!("{}:{}", msg["id"].as_str().unwrap_or(""), v["requestId"].as_str().unwrap_or(""));
            if id.len() > 1 && self.seen.insert(id, ts).is_some() {
                continue;
            }
            let g = |k: &str| usage[k].as_u64().unwrap_or(0);
            let output = g("output_tokens");
            let total = g("input_tokens") + output + g("cache_creation_input_tokens") + g("cache_read_input_tokens");
            self.entries.push(Entry { ts, total, output });
        }
        self.offsets.insert(path.to_path_buf(), off + end as u64);
    }

    fn summarise(&self, now: u64) -> UsageSummary {
        let mut s = UsageSummary::default();
        let midnight = local_midnight(now);
        // 5h windows: start at the hour of the first message, end 5h later;
        // a message after the end (or after a 5h gap) starts a new window.
        let mut block: Option<(u64, u64)> = None;
        let mut block_tokens = 0;
        let mut block_messages = 0;
        let mut last = 0;
        for e in &self.entries {
            if e.ts >= midnight {
                s.today_tokens += e.total;
                s.today_output += e.output;
            }
            let new_block = match block {
                None => true,
                Some((_, end)) => e.ts >= end || e.ts.saturating_sub(last) >= BLOCK_SECS,
            };
            if new_block {
                let start = e.ts - e.ts % 3600;
                block = Some((start, start + BLOCK_SECS));
                block_tokens = 0;
                block_messages = 0;
            }
            block_tokens += e.total;
            block_messages += 1;
            last = e.ts;
        }
        if let Some((start, end)) = block {
            if now < end {
                s.block = Some((start, end));
                s.block_tokens = block_tokens;
                s.block_messages = block_messages;
            }
        }
        s.last_activity = (last > 0).then_some(last);
        s
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parse() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-11-11T21:34:41.183Z"), Some(1_794_432_881));
        assert_eq!(parse_iso("nope"), None);
    }

    #[test]
    fn blocks_and_dedupe() {
        let mut s = Scanner::default();
        let base = 1_794_400_000 - 1_794_400_000 % 3600;
        for (i, ts) in [base + 60, base + 120, base + 3 * 3600, base + 9 * 3600].iter().enumerate() {
            s.entries.push(Entry { ts: *ts, total: 100 * (i as u64 + 1), output: 1 });
        }
        let sum = s.summarise(base + 9 * 3600 + 10);
        // The 4th message is > 5h after the window start → its own window.
        assert_eq!(sum.block, Some((base + 9 * 3600, base + 14 * 3600)));
        assert_eq!(sum.block_tokens, 400);
    }
}
