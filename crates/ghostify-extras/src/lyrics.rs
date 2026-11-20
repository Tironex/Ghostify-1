//! Lyrics: LRC parsing, lookup of synced lines, local files, cache and the
//! optional lookup on lrclib.net.

use crate::player::Track;
use ghostify_core::{Error, Result};
use ghostify_db::repo::misc;
use ghostify_db::Db;
use ghostify_market::Fetcher;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_LINES: usize = 5_000;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_RESPONSE_BYTES: u64 = 512 * 1024;
const FOUND_TTL_SECS: u32 = 30 * 24 * 3600;
const MISSING_TTL_SECS: u32 = 3600;
pub const ONLINE_HOST: &str = "lrclib.net";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    pub time_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Local,
    Online,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    /// Lines with timestamps; empty if only plain text is known.
    pub synced: Vec<Line>,
    pub plain: Option<String>,
    pub source: Source,
}

/// Parses LRC text: `[mm:ss.xx]line`, several stamps per line, `[offset:ms]`.
/// Other tags (`[ar:...]`) are ignored. The result is sorted by time.
pub fn parse_lrc(text: &str) -> Vec<Line> {
    let mut offset: i64 = 0;
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some(end) = tail.find(']') else { break };
            let tag = &tail[..end];
            if let Some(ms) = parse_stamp(tag) {
                stamps.push(ms);
            } else if let Some(value) = tag.strip_prefix("offset:") {
                offset = value.trim().parse().unwrap_or(0);
            } else if !tag.contains(':') {
                break;
            }
            rest = tail[end + 1..].trim_start();
        }
        for ms in stamps {
            if lines.len() >= MAX_LINES {
                break;
            }
            // A positive offset makes the lyrics appear earlier, as in the LRC convention.
            let time = (ms as i64 - offset).max(0) as u64;
            lines.push(Line {
                time_ms: time,
                text: rest.trim().to_owned(),
            });
        }
    }
    lines.sort_by_key(|l| l.time_ms);
    lines
}

fn parse_stamp(tag: &str) -> Option<u64> {
    let (minutes, seconds) = tag.split_once(':')?;
    let minutes: u64 = minutes
        .parse()
        .ok()
        .filter(|_| !minutes.is_empty() && minutes.len() <= 4)?;
    let (whole, fraction) = match seconds.split_once(['.', ':']) {
        Some((whole, fraction)) => (whole, fraction),
        None => (seconds, ""),
    };
    let whole: u64 = whole.parse().ok().filter(|s| *s < 60)?;
    if !fraction.chars().all(|c| c.is_ascii_digit()) || fraction.len() > 3 {
        return None;
    }
    let millis = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction.parse::<u64>().ok()?,
    };
    Some(minutes * 60_000 + whole * 1000 + millis)
}

/// Index of the line to show at `position_ms`: the last one that has started.
pub fn active_index(lines: &[Line], position_ms: u64) -> Option<usize> {
    lines
        .partition_point(|l| l.time_ms <= position_ms)
        .checked_sub(1)
}

/// What the interface needs for one track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LyricsView {
    pub lyrics: Option<Lyrics>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    lyrics: Option<Lyrics>,
}

pub struct LyricsService {
    db: Db,
    dir: PathBuf,
    fetcher: Option<Arc<dyn Fetcher>>,
    online: bool,
}

fn cache_key(track: &Track) -> String {
    let mut hasher = Sha256::new();
    for part in [&track.artist, &track.title, &track.album] {
        hasher.update(part.to_lowercase().as_bytes());
        hasher.update([0]);
    }
    hasher.update(track.duration_ms.map(|d| d / 1000).unwrap_or(0).to_string());
    let digest: String = hasher
        .finalize()
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("lyrics:v1:{digest}")
}

/// File name for local lyrics: `Artist - Title`, without characters that are
/// invalid in file names.
pub fn file_stem(track: &Track) -> String {
    let raw = format!("{} - {}", track.artist, track.title);
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    cleaned.trim().chars().take(120).collect()
}

pub fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl LyricsService {
    pub fn new(
        db: Db,
        dir: PathBuf,
        fetcher: Option<Arc<dyn Fetcher>>,
        online: bool,
    ) -> LyricsService {
        LyricsService {
            db,
            dir,
            fetcher,
            online,
        }
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Looks lyrics up: local files first, then the cache, then (if allowed) the internet.
    pub fn get(&self, track: &Track) -> Result<Option<Lyrics>> {
        if let Some(local) = self.local(track)? {
            return Ok(Some(local));
        }
        if !self.online || self.fetcher.is_none() {
            return Ok(None);
        }
        let key = cache_key(track);
        if let Some(text) = self.db.with(|c| misc::cache_get(c, &key))? {
            if let Ok(cached) = serde_json::from_str::<Cached>(&text) {
                return Ok(cached.lyrics);
            }
        }
        let found = self.fetch_online(track).unwrap_or_else(|err| {
            tracing::debug!(%err, "lyrics lookup failed");
            None
        });
        let ttl = if found.is_some() {
            FOUND_TTL_SECS
        } else {
            MISSING_TTL_SECS
        };
        let text = serde_json::to_string(&Cached {
            lyrics: found.clone(),
        })?;
        self.db
            .with(|c| misc::cache_put(c, &key, &text, Some(ttl)))?;
        Ok(found)
    }

    fn local(&self, track: &Track) -> Result<Option<Lyrics>> {
        let stem = file_stem(track);
        if stem.is_empty() || stem == "-" {
            return Ok(None);
        }
        for extension in ["lrc", "txt"] {
            let path = self.dir.join(format!("{stem}.{extension}"));
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            let synced = parse_lrc(&text);
            let plain = synced
                .is_empty()
                .then(|| text.trim().to_owned())
                .filter(|t| !t.is_empty());
            if synced.is_empty() && plain.is_none() {
                continue;
            }
            return Ok(Some(Lyrics {
                synced,
                plain,
                source: Source::Local,
            }));
        }
        Ok(None)
    }

    fn fetch_online(&self, track: &Track) -> Result<Option<Lyrics>> {
        let Some(fetcher) = &self.fetcher else {
            return Ok(None);
        };
        let mut url = format!(
            "https://{ONLINE_HOST}/api/get?artist_name={}&track_name={}",
            percent_encode(&track.artist),
            percent_encode(&track.title)
        );
        if !track.album.is_empty() {
            url.push_str(&format!("&album_name={}", percent_encode(&track.album)));
        }
        if let Some(duration) = track.duration_ms {
            url.push_str(&format!("&duration={}", duration / 1000));
        }
        let body = fetcher.get(&url, MAX_RESPONSE_BYTES)?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|e| Error::Invalid(format!("unexpected lyrics response: {e}")))?;
        let synced = value["syncedLyrics"]
            .as_str()
            .map(parse_lrc)
            .unwrap_or_default();
        let plain = value["plainLyrics"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        if synced.is_empty() && plain.is_none() {
            return Ok(None);
        }
        Ok(Some(Lyrics {
            synced,
            plain,
            source: Source::Online,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn track() -> Track {
        Track {
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            artwork: None,
            duration_ms: Some(200_000),
        }
    }

    #[test]
    fn lrc_timestamps_and_tags_are_parsed() {
        let lines = parse_lrc("[ar:Someone]\n[ti:Song]\n[00:12.50]Second\n[00:01.5]First\n[01:02.123]Third\n[00:30]No fraction\n");
        let got: Vec<(u64, &str)> = lines.iter().map(|l| (l.time_ms, l.text.as_str())).collect();
        assert_eq!(
            got,
            [
                (1500, "First"),
                (12_500, "Second"),
                (30_000, "No fraction"),
                (62_123, "Third")
            ]
        );
    }

    #[test]
    fn several_stamps_share_a_line_and_offsets_apply() {
        let lines = parse_lrc("[offset:500]\n[00:10.00][00:20.00]Chorus\n[00:00.20]Early");
        let got: Vec<(u64, &str)> = lines.iter().map(|l| (l.time_ms, l.text.as_str())).collect();
        assert_eq!(got, [(0, "Early"), (9_500, "Chorus"), (19_500, "Chorus")]);
    }

    #[test]
    fn malformed_input_is_ignored_not_fatal() {
        assert!(parse_lrc("").is_empty());
        assert!(parse_lrc("just words\nno stamps").is_empty());
        for bad in [
            "[xx:yy]a",
            "[00:75.00]a",
            "[00:10.1234]a",
            "[:10]a",
            "[00:1a]a",
            "[00:10.x]a",
            "[00:10",
        ] {
            assert!(parse_lrc(bad).is_empty(), "{bad}");
        }
        let many: String = (0..6000)
            .map(|i| format!("[00:{:02}.00]x\n", i % 60))
            .collect();
        assert_eq!(parse_lrc(&many).len(), MAX_LINES);
    }

    #[test]
    fn empty_text_lines_are_kept_as_pauses() {
        let lines = parse_lrc("[00:05.00]\n[00:07.00]Words");
        assert_eq!(lines[0].text, "");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn the_active_line_follows_the_position() {
        let lines = parse_lrc("[00:05.00]a\n[00:10.00]b\n[00:15.00]c");
        assert_eq!(active_index(&lines, 0), None);
        assert_eq!(active_index(&lines, 5_000), Some(0));
        assert_eq!(active_index(&lines, 9_999), Some(0));
        assert_eq!(active_index(&lines, 10_000), Some(1));
        assert_eq!(active_index(&lines, 999_999), Some(2));
        assert_eq!(active_index(&[], 10), None);
    }

    #[test]
    fn file_names_are_safe() {
        let mut t = track();
        t.artist = "AC/DC".into();
        t.title = "Who Made Who?".into();
        assert_eq!(file_stem(&t), "AC_DC - Who Made Who_");
        t.title = "../../etc/passwd".into();
        assert!(!file_stem(&t).contains('/'));
        assert!(
            file_stem(&Track {
                title: "x".repeat(500),
                ..track()
            })
            .chars()
            .count()
                <= 120
        );
    }

    #[test]
    fn percent_encoding_covers_unicode_and_reserved_characters() {
        assert_eq!(percent_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(percent_encode("Ä"), "%C3%84");
        assert_eq!(percent_encode("safe-_.~09"), "safe-_.~09");
    }

    fn service(
        dir: &std::path::Path,
        fetcher: Option<Arc<dyn Fetcher>>,
        online: bool,
    ) -> LyricsService {
        LyricsService::new(
            Db::open_in_memory().unwrap(),
            dir.to_owned(),
            fetcher,
            online,
        )
    }

    #[test]
    fn local_files_are_found_by_artist_and_title() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Artist - Song.lrc"), "[00:01.00]Hello").unwrap();
        let found = service(tmp.path(), None, false)
            .get(&track())
            .unwrap()
            .unwrap();
        assert_eq!(found.source, Source::Local);
        assert_eq!(found.synced[0].text, "Hello");

        std::fs::remove_file(tmp.path().join("Artist - Song.lrc")).unwrap();
        std::fs::write(tmp.path().join("Artist - Song.txt"), "Plain words\nmore").unwrap();
        let plain = service(tmp.path(), None, false)
            .get(&track())
            .unwrap()
            .unwrap();
        assert!(plain.synced.is_empty());
        assert_eq!(plain.plain.as_deref(), Some("Plain words\nmore"));

        std::fs::write(tmp.path().join("Artist - Song.txt"), "   ").unwrap();
        assert!(service(tmp.path(), None, false)
            .get(&track())
            .unwrap()
            .is_none());
    }

    #[derive(Default)]
    struct FakeFetcher {
        body: Mutex<Option<Vec<u8>>>,
        urls: Mutex<Vec<String>>,
    }

    impl Fetcher for FakeFetcher {
        fn get(&self, url: &str, _max: u64) -> Result<Vec<u8>> {
            self.urls.lock().unwrap().push(url.to_owned());
            self.body
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| Error::NotFound("404".into()))
        }
    }

    #[test]
    fn online_lookup_is_off_unless_enabled_and_is_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::default());
        *fetcher.body.lock().unwrap() = Some(
            br#"{"syncedLyrics":"[00:02.00]Online line","plainLyrics":"Online line"}"#.to_vec(),
        );

        assert!(service(tmp.path(), Some(fetcher.clone()), false)
            .get(&track())
            .unwrap()
            .is_none());
        assert!(
            fetcher.urls.lock().unwrap().is_empty(),
            "no request without opt-in"
        );

        let svc = service(tmp.path(), Some(fetcher.clone()), true);
        let found = svc.get(&track()).unwrap().unwrap();
        assert_eq!(found.source, Source::Online);
        assert_eq!(found.synced[0].time_ms, 2000);
        assert_eq!(
            fetcher.urls.lock().unwrap()[0],
            "https://lrclib.net/api/get?artist_name=Artist&track_name=Song&album_name=Album&duration=200"
        );
        assert_eq!(svc.get(&track()).unwrap().unwrap(), found);
        assert_eq!(
            fetcher.urls.lock().unwrap().len(),
            1,
            "the second lookup comes from the cache"
        );
    }

    #[test]
    fn misses_and_errors_are_remembered_briefly_and_local_files_still_win() {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::default());
        let svc = service(tmp.path(), Some(fetcher.clone()), true);
        assert!(svc.get(&track()).unwrap().is_none());
        assert!(svc.get(&track()).unwrap().is_none());
        assert_eq!(fetcher.urls.lock().unwrap().len(), 1);

        *fetcher.body.lock().unwrap() = Some(br#"{"instrumental":true}"#.to_vec());
        let other = Track {
            title: "Instrumental".into(),
            ..track()
        };
        assert!(svc.get(&other).unwrap().is_none());

        std::fs::write(tmp.path().join("Artist - Song.lrc"), "[00:01.00]Mine").unwrap();
        assert_eq!(svc.get(&track()).unwrap().unwrap().source, Source::Local);
    }

    #[test]
    fn garbage_responses_do_not_break_playback() {
        let tmp = tempfile::tempdir().unwrap();
        let fetcher = Arc::new(FakeFetcher::default());
        *fetcher.body.lock().unwrap() = Some(b"<html>nope</html>".to_vec());
        assert!(service(tmp.path(), Some(fetcher), true)
            .get(&track())
            .unwrap()
            .is_none());
    }
}
