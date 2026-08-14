use anyhow::{Result, anyhow, bail};
use reqwest::{Client, ClientBuilder, StatusCode};
use serde_json::Value;
use std::time::Duration;
use tokio::task::JoinSet;

const INVIDIOUS_INSTANCE_LIST: [&str; 7] = [
    "https://inv.bp.projectsegfau.lt",
    "https://invidious.projectsegfau.lt",
    "https://y.com.sb",
    "https://inv.nadeko.net",
    "https://invidious.nerdvpn.de",
    "https://yewtu.be",
    "https://yt.artemislena.eu",
];

#[derive(Clone, Debug)]
pub struct Instance {
    pub domain: Option<String>,
    client: Client,
    query: Option<String>,
}

impl PartialEq for Instance {
    fn eq(&self, other: &Self) -> bool {
        self.domain == other.domain
    }
}

impl Eq for Instance {}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct YoutubeVideo {
    pub title: String,
    pub length_seconds: u64,
    pub video_id: String,
}

impl Default for Instance {
    fn default() -> Self {
        let client = Client::new();
        let domain = Some(String::new());
        let query = Some(String::new());

        Self {
            domain,
            client,
            query,
        }
    }
}

impl Instance {
    pub async fn new(query: &str) -> Result<(Self, Vec<YoutubeVideo>)> {
        let client = ClientBuilder::new()
            .timeout(Duration::from_secs(5))
            .build()?;

        let instances: Vec<&str> = INVIDIOUS_INSTANCE_LIST.to_vec();
        let query_owned = query.to_string();

        // Try all instances concurrently — first to respond wins
        let mut set = JoinSet::new();
        for v in instances {
            let url = format!("{v}/api/v1/search");
            let domain = v.to_string();
            let client = client.clone();
            let q = query_owned.clone();
            set.spawn(async move {
                let query_vec = vec![
                    ("q", q.as_str()),
                    ("page", "1"),
                    ("type", "video"),
                    ("sort_by", "relevance"),
                ];
                if let Ok(result) = client.get(&url).query(&query_vec).send().await
                    && result.status() == 200
                    && let Ok(text) = result.text().await
                    && let Some(vr) = Instance::parse_youtube_options(&text)
                {
                    return Some((domain, vr));
                }
                None::<(String, Vec<YoutubeVideo>)>
            });
        }

        let mut domain = String::new();
        let mut video_result: Vec<YoutubeVideo> = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some((d, vr))) = res {
                domain = d;
                video_result = vr;
                break;
            }
        }

        if video_result.is_empty() {
            bail!("All invidious servers are down. Try again later.")
        }

        let instance = Self {
            domain: Some(domain),
            client,
            query: Some(query.to_string()),
        };
        Ok((instance, video_result))
    }

    // GetSearchQuery fetches query result from an Invidious instance.
    pub async fn get_search_query(&self, page: u32) -> Result<Vec<YoutubeVideo>> {
        if self.domain.is_none() {
            bail!("No server available");
        }
        let url = format!(
            "{}/api/v1/search",
            self.domain
                .as_ref()
                .ok_or(anyhow!("error in domain name"))?
        );

        let Some(query) = &self.query else {
            bail!("No query string found")
        };

        let result = self
            .client
            .get(url)
            .query(&[("q", query), ("page", &page.to_string())])
            .send()
            .await?;

        match result.status() {
            StatusCode::OK => match result.text().await {
                Ok(text) => Self::parse_youtube_options(&text).ok_or_else(|| anyhow!("None Error")),
                Err(e) => bail!("Error during search: {e}"),
            },
            _ => bail!("Error during search"),
        }
    }

    // GetSuggestions returns video suggestions based on prefix strings. This is the
    // same result as youtube search autocomplete.
    pub async fn get_suggestions(&self, prefix: &str) -> Result<Vec<YoutubeVideo>> {
        let url = format!(
            "http://suggestqueries.google.com/complete/search?client=firefox&ds=yt&q={prefix}"
        );
        let result = self.client.get(url).send().await?;
        match result.status() {
            StatusCode::OK => match result.text().await {
                Ok(text) => Self::parse_youtube_options(&text).ok_or_else(|| anyhow!("None Error")),
                Err(e) => bail!("Error during search: {e}"),
            },
            _ => bail!("Error during search"),
        }
    }

    // GetTrendingMusic fetch music trending based on region.
    // Region (ISO 3166 country code) can be provided in the argument.
    pub async fn get_trending_music(&self, region: &str) -> Result<Vec<YoutubeVideo>> {
        if self.domain.is_none() {
            bail!("No server available");
        }
        let url = format!(
            "{}/api/v1/trending?type=music&region={region}",
            self.domain
                .as_ref()
                .ok_or(anyhow!("error in domain names"))?
        );

        let result = self.client.get(url).send().await?;

        match result.status() {
            StatusCode::OK => match result.text().await {
                Ok(text) => Self::parse_youtube_options(&text).ok_or_else(|| anyhow!("None Error")),
                _ => bail!("Error during search"),
            },
            _ => bail!("Error during search"),
        }
    }
}

/// Search `YouTube` using yt-dlp directly. Falls back to this when Invidious servers are down.
/// Spawns `yt-dlp --dump-json --flat-playlist "ytsearch{N}:{query}"` and parses JSON lines.
pub fn ytdlp_search(query: &str, limit: u32) -> Result<Vec<YoutubeVideo>> {
    let search_str = format!("ytsearch{limit}:{query}");
    let output = std::process::Command::new("yt-dlp")
        .arg("--dump-json")
        .arg("--flat-playlist")
        .arg(&search_str)
        .output()
        .map_err(|e| anyhow!("Failed to run yt-dlp: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("yt-dlp search failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut results = Vec::new();

    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(line) {
            let video_id = match value.get("id").and_then(|v| v.as_str()) {
                Some(id) => id.to_owned(),
                None => continue,
            };
            let title = match value.get("title").and_then(|v| v.as_str()) {
                Some(t) => t.to_owned(),
                None => continue,
            };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let length_seconds = value
                .get("duration")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0) as u64;

            results.push(YoutubeVideo {
                title,
                length_seconds,
                video_id,
            });
        }
    }

    if results.is_empty() {
        bail!("yt-dlp returned no results for query: {query}");
    }

    Ok(results)
}

impl Instance {
    fn parse_youtube_options(data: &str) -> Option<Vec<YoutubeVideo>> {
        if let Ok(value) = serde_json::from_str::<Value>(data) {
            let mut vec: Vec<YoutubeVideo> = Vec::new();
            // below two lines are left for debug purpose
            // let mut file = std::fs::File::create("data.txt").expect("create failed");
            // file.write_all(data.as_bytes()).expect("write failed");
            if let Some(array) = value.as_array() {
                for v in array {
                    if let Some((title, video_id, length_seconds)) = Self::parse_youtube_item(v) {
                        vec.push(YoutubeVideo {
                            title,
                            length_seconds,
                            video_id,
                        });
                    }
                }
                return Some(vec);
            }
        }
        None
    }

    fn parse_youtube_item(value: &Value) -> Option<(String, String, u64)> {
        let title = value.get("title")?.as_str()?.to_owned();
        let video_id = value.get("videoId")?.as_str()?.to_owned();
        let length_seconds = value.get("lengthSeconds")?.as_u64()?;
        Some((title, video_id, length_seconds))
    }
}

use std::sync::{Arc, LazyLock};
use std::thread;

use regex::Regex;

const LRCLIB_USER_AGENT: &str = "termusic (https://github.com/tramhao/termusic)";
const LRCLIB_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FILTER_CONCURRENCY: usize = 6;

/// Prefixed to a search result's title when caelestia will show lyrics for it
/// (LRCLIB exact match or `NetEase`, using the tags the downloader will write).
const HAS_LYRICS_MARK: char = '\u{f00c}'; // 
/// Prefixed to a search result's title when no lyrics are available.
const NO_LYRICS_MARK: char = '\u{f073a}'; // 󰜺

static RE_TITLE_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s*\([^)]*(?:official|lyrics|music|audio|video|hd|visualizer|remastered|4k|edit|version)[^)]*\)\s*$")
        .unwrap()
});

fn lrclib_blocking_client() -> &'static reqwest::blocking::Client {
    static CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
        reqwest::blocking::Client::builder()
            .user_agent(LRCLIB_USER_AGENT)
            .timeout(LRCLIB_TIMEOUT)
            .build()
            .expect("failed to build reqwest blocking client")
    });
    &CLIENT
}

fn netease_blocking_client() -> &'static reqwest::blocking::Client {
    static CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(
                "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0",
            ),
        );
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://music.163.com/"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/plain, */*"),
        );
        reqwest::blocking::Client::builder()
            .default_headers(headers)
            .timeout(LRCLIB_TIMEOUT)
            .build()
            .expect("failed to build reqwest blocking client")
    });
    &CLIENT
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// Returns `true` when the desktop shell (caelestia) will show lyrics for this
/// search result's download. The shell's Auto backend queries LRCLIB with the
/// file's exact artist + title tags; when that misses it falls back to a
/// `NetEase` search. This replicates that chain, using the artist/title that the
/// downloader's `--parse-metadata` will write into the file.
#[must_use]
pub fn is_lyrics_viable(item: &YoutubeVideo) -> bool {
    let (artist, title) = parse_artist_title(&item.title);
    if artist.is_empty() || title.is_empty() {
        return false;
    }
    lrclib_has_synced(&artist, &title) || netease_has(&artist, &title)
}

/// Prefix every search result's title with a mark indicating whether caelestia
/// will show lyrics for it (see [`is_lyrics_viable`]): a check when it will, a
/// cross when it will not. All items are kept; the marks run concurrently with
/// a bounded number of threads. Returns the annotated items and how many will
/// have lyrics.
#[must_use]
pub fn annotate_lyrics_availability(items: Vec<YoutubeVideo>) -> (Vec<YoutubeVideo>, usize) {
    let counter = Arc::new((std::sync::Mutex::new(0usize), std::sync::Condvar::new()));
    thread::scope(|scope| {
        let handles: Vec<_> = items
            .into_iter()
            .map(|item| {
                let counter = Arc::clone(&counter);
                scope.spawn(move || {
                    let (lock, cvar) = &*counter;
                    let mut in_flight = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    while *in_flight >= MAX_FILTER_CONCURRENCY {
                        in_flight = cvar
                            .wait(in_flight)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                    }
                    *in_flight += 1;
                    drop(in_flight);

                    let viable = is_lyrics_viable(&item);

                    let mut in_flight = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    *in_flight -= 1;
                    cvar.notify_one();
                    drop(in_flight);

                    let title = if viable {
                        format!("{HAS_LYRICS_MARK} {}", item.title)
                    } else {
                        format!("{NO_LYRICS_MARK} {}", item.title)
                    };
                    (viable, YoutubeVideo { title, ..item })
                })
            })
            .collect();
        let results: Vec<(bool, YoutubeVideo)> = handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect();
        let viable_count = results.iter().filter(|(viable, _)| *viable).count();
        (
            results.into_iter().map(|(_, item)| item).collect(),
            viable_count,
        )
    })
}

/// Mirrors the shell's `NetEase` fallback: search `NetEase` for "<title> <artist>",
/// keep the first song whose first artist's name substring-matches either
/// direction, then require a non-empty LRC lyric for that song id.
fn netease_has(artist: &str, title: &str) -> bool {
    let query = format!("{title} {artist}");
    let Ok(resp) = netease_blocking_client()
        .get("https://music.163.com/api/search/get")
        .query(&[("s", query.as_str()), ("type", "1"), ("limit", "5")])
        .send()
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(json) = resp.json::<Value>() else {
        return false;
    };
    let Some(songs) = json
        .get("result")
        .and_then(|result| result.get("songs"))
        .and_then(Value::as_array)
    else {
        return false;
    };
    let Some(id) = songs.iter().find_map(|song| {
        let song_artist = song
            .get("artists")
            .and_then(Value::as_array)
            .and_then(|artists| artists.first())
            .and_then(|artist| artist.get("name"))
            .and_then(Value::as_str)?;
        if contains_ci(artist, song_artist) || contains_ci(song_artist, artist) {
            song.get("id").and_then(Value::as_u64)
        } else {
            None
        }
    }) else {
        return false;
    };

    let id_str = id.to_string();
    let Ok(resp) = netease_blocking_client()
        .get("https://music.163.com/api/song/lyric")
        .query(&[
            ("id", id_str.as_str()),
            ("lv", "1"),
            ("kv", "1"),
            ("tv", "-1"),
        ])
        .send()
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(json) = resp.json::<Value>() else {
        return false;
    };
    json.get("lrc")
        .and_then(|lrc| lrc.get("lyric"))
        .and_then(Value::as_str)
        .is_some_and(|lrc| !lrc.trim().is_empty())
}

fn lrclib_has_synced(artist: &str, title: &str) -> bool {
    let Ok(resp) = lrclib_blocking_client()
        .get("https://lrclib.net/api/get")
        .query(&[("artist_name", artist), ("track_name", title)])
        .send()
    else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    let Ok(value) = resp.json::<Value>() else {
        return false;
    };
    value
        .get("syncedLyrics")
        .and_then(Value::as_str)
        .is_some_and(|lyrics| !lyrics.trim().is_empty())
}

fn parse_artist_title(raw: &str) -> (String, String) {
    let cleaned = RE_TITLE_SUFFIX.replace(raw, "").trim().to_owned();
    match cleaned.split_once(" - ") {
        Some((artist, title)) => (artist.trim().to_owned(), title.trim().to_owned()),
        None => (String::new(), cleaned),
    }
}
