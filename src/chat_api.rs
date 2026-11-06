//! Invoke's HTTP API, for the Chat layout: the same threads the phone app
//! reads, so a chat started on either continues on the other.
//!
//! Tailscale Serve in front of Invoke adds the caller's identity, so there is
//! no token. Every call blocks, so callers run it off the UI thread. It goes
//! through the system `curl` (TLS and the tailnet's certs included) rather
//! than a new HTTP crate: gpui's own client is a stub until an app installs one.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context as _, Result};
use serde::{de::DeserializeOwned, Deserialize};

/// Shown in the Chat view when no Invoke server URL is configured.
pub const NO_URL_MESSAGE: &str = "Chat needs an Invoke server URL. Set the SEANCE_INVOKE_URL environment variable, or put the URL on one line in ~/.config/seance/invoke-url, then reopen Chat.";
/// Plain requests give up here; the long-poll gets its own, longer limit.
const REQUEST_SECS: u64 = 10;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Circles {
    #[serde(default)]
    pub circles: Vec<CircleRow>,
    #[serde(default)]
    pub v: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct CircleRow {
    pub ws: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub unread: u32,
    #[serde(default)]
    pub working: bool,
    /// Its crew, as [`Thread::crew`]: two or more dance in its list row.
    #[serde(default)]
    pub crew: Vec<u32>,
    #[serde(default)]
    pub last: Option<Msg>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Msg {
    #[serde(default)]
    pub id: i64,
    /// "me", "lead", or anything else (shown as a system note).
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub ts: u64,
    /// File name under `/api/file/`: the first picture of the note.
    #[serde(default)]
    pub image: Option<String>,
    /// Every picture of a note that has more than one (`image` is the first).
    #[serde(default)]
    pub images: Vec<String>,
    /// Answers a lead offers (from its `choice:` lines); the first is its pick.
    #[serde(default)]
    pub choices: Vec<String>,
    /// Set on a spec card: the title, which is also the first line of `text`.
    #[serde(default)]
    pub spec: Option<String>,
    /// The choice that was tapped, once one has been.
    #[serde(default)]
    pub picked: Option<String>,
    /// How far one of the user's notes has got: sending, got, answered or failed.
    #[serde(default)]
    pub status: String,
    /// A document sent with the note (pdf, csv and the like).
    #[serde(default)]
    pub attachment: Option<Attachment>,
}

/// A document on a note: the file name under `/api/file/` and what to call it.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub title: String,
}

impl Attachment {
    /// Where the document opens (the title becomes the saved file's name).
    pub fn url(&self) -> String {
        format!(
            "{}/api/file/{}?name={}",
            base(),
            enc(&self.file),
            enc(&self.title)
        )
    }
}

impl Msg {
    /// The names of all the message's pictures, in order.
    pub fn image_names(&self) -> Vec<&str> {
        if self.images.is_empty() {
            self.image.as_deref().into_iter().collect()
        } else {
            self.images.iter().map(String::as_str).collect()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Thread {
    #[serde(default)]
    pub messages: Vec<Msg>,
    /// Highest message id already read.
    #[serde(default)]
    pub read: i64,
    /// Claude plan usage, which every thread answer carries.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// The circle's lead is mid-turn, so Stop has something to stop.
    #[serde(default)]
    pub lead_busy: bool,
    /// Percent of the lead's Claude context used; none when unknown (asleep,
    /// no status line, or Alerts).
    #[serde(default)]
    pub context: Option<u32>,
    /// Who dances in the circle's crew: its busy panes by their place in the
    /// circle, then running subagents numbered after every pane. Two or more
    /// dance as a crew in the header.
    #[serde(default)]
    pub crew: Vec<u32>,
}

/// One `/api/search` result: a message, and the circle it is in.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Hit {
    #[serde(default)]
    pub ws: String,
    #[serde(default)]
    pub circle: String,
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub ts: u64,
    #[serde(default)]
    pub text: String,
}

#[derive(Deserialize)]
struct Hits {
    #[serde(default)]
    results: Vec<Hit>,
}

/// `/api/panes`: a pane of the circle that the user can @-mention.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct PaneRow {
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    /// The handle that goes after the `@`.
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub lead: bool,
    /// A pane worth an @-mention (the lead and bare terminals are not).
    #[serde(default)]
    pub helper: bool,
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub asleep: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub five_hour: Option<UsageWindow>,
    #[serde(default)]
    pub seven_day: Option<UsageWindow>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct UsageWindow {
    /// Percent of the window used.
    #[serde(default)]
    pub used: f64,
    /// When the window resets, unix ms.
    #[serde(default)]
    pub resets: Option<u64>,
}

/// `/api/tasks`: only what the thread header shows.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Progress {
    #[serde(default)]
    pub busy: bool,
    /// The lead's progress line while it works.
    #[serde(default)]
    pub doing: Option<String>,
}

/// `/api/tasks`: what the phone's to-do list shows for a circle's lead.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct Tasks {
    /// The lead's name; none when the circle has no lead.
    #[serde(default)]
    pub lead: Option<String>,
    #[serde(default)]
    pub busy: bool,
    #[serde(default)]
    pub doing: Option<String>,
    #[serde(default)]
    pub now: Vec<TaskItem>,
    #[serde(default)]
    pub waiting: Vec<TaskItem>,
    #[serde(default)]
    pub parked: Vec<TaskItem>,
    #[serde(default)]
    pub done: Vec<TaskItem>,
    /// Other panes in the circle that are working.
    #[serde(default)]
    pub helpers: Vec<HelperItem>,
    /// The lead's background agents (reviews and the like).
    #[serde(default)]
    pub agents: Vec<AgentItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct TaskItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    /// the user's words.
    #[serde(default)]
    pub text: String,
    /// Sent from the phone (else from a pane).
    #[serde(default)]
    pub phone: bool,
    #[serde(default)]
    pub start: Option<u64>,
    #[serde(default)]
    pub end: Option<u64>,
    /// The lead's own name for the task.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct HelperItem {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub doing: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct AgentItem {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub doing: Option<String>,
    /// How long it has been running, as Invoke words it ("5 min").
    #[serde(default, rename = "for")]
    pub running: String,
}

#[derive(Deserialize)]
struct Poll {
    #[serde(default)]
    v: u64,
}

/// The Invoke URL: the env var wins, then the first non-empty line of the
/// config file. Empty when neither is set.
fn resolve_base(env: Option<&str>, file: Option<&str>) -> String {
    let clean = |s: &str| s.trim().to_string();
    if let Some(e) = env.map(clean).filter(|e| !e.is_empty()) {
        return e;
    }
    file.and_then(|f| f.lines().map(clean).find(|l| !l.is_empty()))
        .unwrap_or_default()
}

fn invoke_url_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("seance/invoke-url");
        }
    }
    PathBuf::from(shellexpand::tilde("~/.config/seance/invoke-url").as_ref())
}

fn base() -> String {
    let env = std::env::var("SEANCE_INVOKE_URL").ok();
    let file = std::fs::read_to_string(invoke_url_path()).ok();
    resolve_base(env.as_deref(), file.as_deref())
}

/// Percent-encode a query value (circle slugs are plain, but be safe).
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `scheme://host[:port]` of the base URL: what Invoke's POST handler wants
/// in `Origin` (it refuses a request without it, 403).
fn origin(base: &str) -> String {
    match base.split_once("://") {
        Some((scheme, rest)) => {
            format!("{scheme}://{}", rest.split('/').next().unwrap_or(rest))
        }
        None => base.to_string(),
    }
}

fn curl(path: &str, secs: u64, post: Option<&serde_json::Value>) -> Result<Vec<u8>> {
    if base().is_empty() {
        return Err(anyhow!(NO_URL_MESSAGE));
    }
    let mut cmd = Command::new("curl");
    // --fail-with-body: a refusal still hands back Invoke's {"error": ...}.
    cmd.args(["-sS", "--fail-with-body", "-m", &secs.to_string()]);
    if post.is_some() {
        cmd.args([
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-H",
            &format!("Origin: {}", origin(&base())),
            "--data-binary",
            "@-",
        ]);
    }
    cmd.arg(format!("{}{path}", base()))
        .stdin(if post.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("run curl")?;
    if let (Some(body), Some(mut stdin)) = (post, child.stdin.take()) {
        stdin.write_all(body.to_string().as_bytes())?;
    }
    outcome(child.wait_with_output()?)
}

/// curl's answer: its body, or the reason it failed (Invoke's own words when
/// it refused).
fn outcome(out: std::process::Output) -> Result<Vec<u8>> {
    if !out.status.success() {
        if let Some(said) = refusal(&out.stdout) {
            return Err(anyhow!("{said}"));
        }
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("{}", err.trim()));
    }
    Ok(out.stdout)
}

/// What Invoke said when it refused a request: its `{"error": ...}` text.
fn refusal(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("error")?.as_str().map(str::to_string)
}

fn get<T: DeserializeOwned>(path: &str, secs: u64) -> Result<T> {
    let bytes = curl(path, secs, None)?;
    serde_json::from_slice(&bytes).with_context(|| format!("read {path}"))
}

pub fn circles() -> Result<Circles> {
    get("/api/circles", REQUEST_SECS)
}

/// The circle's thread; with `after`, only the messages past that id.
pub fn thread(ws: &str, after: Option<i64>) -> Result<Thread> {
    let after = after.map_or(String::new(), |id| format!("&after={id}"));
    get(&format!("/api/thread?ws={}{after}", enc(ws)), REQUEST_SECS)
}

pub fn progress(ws: &str) -> Result<Progress> {
    get(&format!("/api/tasks?ws={}", enc(ws)), REQUEST_SECS)
}

/// The whole to-do list of a circle's lead.
pub fn tasks(ws: &str) -> Result<Tasks> {
    get(&format!("/api/tasks?ws={}", enc(ws)), REQUEST_SECS)
}

/// Blocks until Invoke's change counter moves past `v`, or its own wait of
/// `wait` seconds runs out. Returns the counter either way. curl's limit sits
/// past the server's wait, or every quiet poll would end in a timeout.
pub fn poll(v: u64, wait: u64) -> Result<u64> {
    Ok(get::<Poll>(&format!("/api/poll?v={v}"), wait + 10)?.v)
}

pub fn send(ws: &str, text: &str) -> Result<()> {
    curl(
        "/api/send",
        REQUEST_SECS,
        Some(&serde_json::json!({ "ws": ws, "text": text })),
    )
    .map(|_| ())
}

/// Send a parked task back to the circle's lead as the user's message and close
/// the parked copy. Invoke refuses with "not a parked task" for anything else.
pub fn resume(ws: &str, id: &str) -> Result<()> {
    curl(
        "/api/resume",
        REQUEST_SECS,
        Some(&serde_json::json!({ "ws": ws, "id": id })),
    )
    .map(|_| ())
}

/// Close a parked task without doing it.
pub fn dismiss(ws: &str, id: &str) -> Result<()> {
    curl(
        "/api/dismiss",
        REQUEST_SECS,
        Some(&serde_json::json!({ "ws": ws, "id": id })),
    )
    .map(|_| ())
}

/// Answer a lead's message with one of its choices: sent as the user's message,
/// tied to the message it answers so Invoke can mark the pick.
pub fn answer(ws: &str, text: &str, answers: i64) -> Result<()> {
    curl(
        "/api/send",
        REQUEST_SECS,
        Some(&serde_json::json!({ "ws": ws, "text": text, "answers": answers })),
    )
    .map(|_| ())
}

/// Documents (and pictures) a note can carry, and the biggest one Invoke takes.
const UPLOAD_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "heic", "pdf", "csv", "tsv", "txt", "md", "json", "doc",
    "docx", "xls", "xlsx", "ppt", "pptx", "key", "numbers", "pages", "zip",
];
const UPLOAD_MAX: u64 = 50 * 1024 * 1024;
/// A 50 MB upload over the tailnet needs more than a plain request's limit.
const UPLOAD_SECS: u64 = 300;

/// Whether Invoke would take this file, before the bytes are sent.
pub fn check_upload(path: &std::path::Path) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !UPLOAD_EXTS.contains(&ext.as_str()) {
        return Err(anyhow!("can't send .{ext} files"));
    }
    if std::fs::metadata(path)?.len() > UPLOAD_MAX {
        return Err(anyhow!("that file is over 50 MB"));
    }
    Ok(())
}

/// Send a file as the user's note (`text` rides along as its caption).
pub fn send_file(ws: &str, text: &str, path: &std::path::Path) -> Result<()> {
    if base().is_empty() {
        return Err(anyhow!(NO_URL_MESSAGE));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| anyhow!("bad file name"))?;
    let url = format!(
        "{}/api/image?ws={}&text={}&name={}",
        base(),
        enc(ws),
        enc(text),
        enc(&name)
    );
    let mut data = std::ffi::OsString::from("@");
    data.push(path);
    let out = Command::new("curl")
        .args(["-sS", "--fail-with-body", "-m", &UPLOAD_SECS.to_string()])
        .args(["-X", "POST", "-H", "content-type: application/octet-stream"])
        .args(["-H", &format!("Origin: {}", origin(&base()))])
        .arg("--data-binary")
        .arg(data)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("run curl")?;
    outcome(out).map(|_| ())
}

/// Ask the circle's lead to stop its turn; a 409 says why it could not.
pub fn stop(ws: &str) -> Result<()> {
    post("/api/stop", serde_json::json!({ "ws": ws }))
}

/// the user's messages and the leads' that match `q`, newest first.
pub fn search(q: &str) -> Result<Vec<Hit>> {
    Ok(get::<Hits>(&format!("/api/search?q={}", enc(q)), REQUEST_SECS)?.results)
}

/// The circle's panes, for @-mentions.
pub fn panes(ws: &str) -> Result<Vec<PaneRow>> {
    #[derive(Deserialize)]
    struct Panes {
        #[serde(default)]
        panes: Vec<PaneRow>,
    }
    Ok(get::<Panes>(&format!("/api/panes?ws={}", enc(ws)), REQUEST_SECS)?.panes)
}

pub fn forward(id: i64, ws: &str) -> Result<()> {
    post("/api/forward", serde_json::json!({ "id": id, "ws": ws }))
}

/// Remind the user of a message at `at` (unix ms), or, with `lead`, have the
/// circle's lead pick it up then. `label` is how the time was worded.
pub fn remind(id: i64, at: u64, lead: bool, label: &str) -> Result<()> {
    post(
        "/api/remind",
        serde_json::json!({ "id": id, "at": at, "lead": lead, "label": label }),
    )
}

pub fn unread(ws: &str) -> Result<()> {
    post("/api/unread", serde_json::json!({ "ws": ws }))
}

fn post(path: &str, body: serde_json::Value) -> Result<()> {
    curl(path, REQUEST_SECS, Some(&body)).map(|_| ())
}

pub fn mark_read(ws: &str, id: i64) -> Result<()> {
    curl(
        "/api/read",
        REQUEST_SECS,
        Some(&serde_json::json!({ "ws": ws, "id": id })),
    )
    .map(|_| ())
}

/// An attached image, downloaded once into the cache; its local path.
pub fn file(name: &str) -> Result<PathBuf> {
    file_within(name, 30)
}

/// Same, with `secs` for the download: a render sent as a note can be big.
/// Written straight to disk, so a long file is not held in memory.
pub fn file_within(name: &str, secs: u64) -> Result<PathBuf> {
    // The name goes into a path: keep only its last component.
    let leaf = name.rsplit('/').next().unwrap_or(name);
    if leaf.is_empty() || leaf.starts_with('.') {
        return Err(anyhow!("bad file name"));
    }
    let dir = PathBuf::from(shellexpand::tilde("~/.cache/seance/chat").as_ref());
    let path = dir.join(leaf);
    if path.exists() {
        return Ok(path);
    }
    if base().is_empty() {
        return Err(anyhow!(NO_URL_MESSAGE));
    }
    std::fs::create_dir_all(&dir)?;
    // Not `with_extension`: a.mp4 and a.png must not share a partial file.
    let tmp = dir.join(format!("{leaf}.part"));
    let out = Command::new("curl")
        .args(["-sS", "--fail-with-body", "-m", &secs.to_string(), "-o"])
        .arg(&tmp)
        .arg(format!("{}/api/file/{}", base(), enc(leaf)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("run curl")?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Pixel size of a picture, read from its header: PNG, JPEG, GIF, WebP and BMP.
/// None for anything else (HEIC and the like) or a file that cannot be read.
pub fn image_size(path: &std::path::Path) -> Option<(u32, u32)> {
    use std::io::Read as _;
    let mut head = Vec::new();
    // A JPEG's size can sit behind a long EXIF block; a megabyte reaches it.
    std::fs::File::open(path)
        .ok()?
        .take(1 << 20)
        .read_to_end(&mut head)
        .ok()?;
    size_from_header(&head)
}

fn size_from_header(b: &[u8]) -> Option<(u32, u32)> {
    let be32 = |at: usize| {
        b.get(at..at + 4)
            .map(|x| u32::from_be_bytes(x.try_into().unwrap()))
    };
    let le16 = |at: usize| {
        b.get(at..at + 2)
            .map(|x| u32::from(u16::from_le_bytes(x.try_into().unwrap())))
    };
    let le24 = |at: usize| {
        b.get(at..at + 3)
            .map(|x| u32::from(x[0]) | u32::from(x[1]) << 8 | u32::from(x[2]) << 16)
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"BM") {
        let h = b
            .get(22..26)
            .map(|x| i32::from_le_bytes(x.try_into().unwrap()))?;
        return Some((
            b.get(18..22)
                .map(|x| u32::from_le_bytes(x.try_into().unwrap()))?,
            h.unsigned_abs(),
        ));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut at = 2;
        while at + 4 <= b.len() {
            if b[at] != 0xff {
                return None;
            }
            let marker = b[at + 1];
            if marker == 0xff {
                at += 1;
                continue;
            }
            let len = usize::from(u16::from_be_bytes([b[at + 2], b[at + 3]]));
            // The start-of-frame markers carry the size (not DHT, JPG or DAC).
            if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                let h = u32::from(u16::from_be_bytes([*b.get(at + 5)?, *b.get(at + 6)?]));
                let w = u32::from(u16::from_be_bytes([*b.get(at + 7)?, *b.get(at + 8)?]));
                return Some((w, h));
            }
            at += 2 + len;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_sizes_come_from_the_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 3, 0x20, 0, 0, 6, 0x40]);
        assert_eq!(size_from_header(&png), Some((800, 1600)));
        assert_eq!(size_from_header(b"GIF89a\x10\x00\x20\x00"), Some((16, 32)));
        // A JPEG: SOI, an APP0 segment to skip, then SOF0 (height 1200, width 900).
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x0b, 0x08, 0x04,
            0xb0, 0x03, 0x84, 0x01, 0x01, 0x11, 0x00,
        ];
        assert_eq!(size_from_header(&jpeg), Some((900, 1200)));
        // WebP, extended header: 24-bit width-1 and height-1.
        let mut webp = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        webp.extend([0x1f, 0x03, 0x00, 0xff, 0x01, 0x00]);
        assert_eq!(size_from_header(&webp), Some((800, 512)));
        assert_eq!(size_from_header(b"not a picture"), None);
        assert_eq!(size_from_header(b"\x89PNG"), None);
    }

    #[test]
    fn a_spec_title_parses_and_is_optional() {
        let m: Msg = serde_json::from_str(
            r#"{"id":8,"from":"lead","text":"Title\nbody","spec":"Title","choices":["Go","Change"]}"#,
        )
        .unwrap();
        assert_eq!(m.spec.as_deref(), Some("Title"));
        let plain: Msg = serde_json::from_str(r#"{"id":9,"from":"lead","text":"hi"}"#).unwrap();
        assert!(plain.spec.is_none());
    }

    #[test]
    fn choices_and_the_pick_parse() {
        let m: Msg = serde_json::from_str(
            r#"{"id":7,"from":"lead","text":"which?","choices":["a","b"],"picked":"b"}"#,
        )
        .unwrap();
        assert_eq!(m.choices, ["a", "b"]);
        assert_eq!(m.picked.as_deref(), Some("b"));
        let plain: Msg = serde_json::from_str(r#"{"id":8,"from":"lead","text":"hi"}"#).unwrap();
        assert!(plain.choices.is_empty() && plain.picked.is_none());
    }

    #[test]
    fn a_note_with_several_pictures_lists_them_all() {
        let one = Msg {
            image: Some("a.png".into()),
            ..Default::default()
        };
        assert_eq!(one.image_names(), ["a.png"]);
        let many: Msg =
            serde_json::from_str(r#"{"image":"a.png","images":["a.png","b.png"]}"#).unwrap();
        assert_eq!(many.image_names(), ["a.png", "b.png"]);
        assert!(Msg::default().image_names().is_empty());
    }

    #[test]
    fn circles_parse_with_a_missing_last_message() {
        let c: Circles = serde_json::from_str(
            r#"{"circles":[{"ws":"main","name":"PM","unread":2,"working":true,"crew":[0,4,9],
            "last":{"from":"lead","text":"hi","ts":5,"image":null}},
            {"ws":"b","name":"B","last":null}],"v":20,"afk":{"on":true}}"#,
        )
        .unwrap();
        assert_eq!(c.v, 20);
        assert_eq!(c.circles[0].unread, 2);
        assert_eq!(c.circles[0].last.as_ref().unwrap().text, "hi");
        assert!(c.circles[1].last.is_none() && !c.circles[1].working);
        assert_eq!(c.circles[0].crew, [0, 4, 9]);
        assert!(c.circles[1].crew.is_empty());
    }

    #[test]
    fn thread_ignores_fields_it_does_not_show() {
        let t: Thread = serde_json::from_str(
            r#"{"messages":[{"id":3,"ws":"main","from":"me","ts":9,"status":"answered","mode":"","text":"yes"}],
            "read":3,"working":false,"usage":{}}"#,
        )
        .unwrap();
        assert_eq!(t.read, 3);
        assert_eq!(t.usage, Some(Usage::default()));
        assert_eq!(t.messages[0].from, "me");
        assert_eq!(t.messages[0].image, None);
    }

    #[test]
    fn tasks_parse_names_helpers_and_agents() {
        let t: Tasks = serde_json::from_str(
            r#"{"lead":"L","busy":true,"doing":"✻ Working","now":[{"id":"task-1","status":"open",
            "text":"fix it","phone":true,"start":5,"end":null,"name":"Fix the thing"}],
            "waiting":[],"parked":[{"id":"task-2","status":"parked","text":"x"}],
            "helpers":[{"name":"Builder","doing":null,"task":"build it"}],
            "agents":[{"name":"review","doing":"reading","for":"5 min"}],
            "done":[{"id":"task-3","status":"done","text":"y","phone":false,"start":1,"end":2,"name":null}]}"#,
        )
        .unwrap();
        assert_eq!(t.now[0].name.as_deref(), Some("Fix the thing"));
        assert_eq!(t.agents[0].running, "5 min");
        assert_eq!(t.helpers[0].task.as_deref(), Some("build it"));
        assert_eq!(t.parked[0].start, None);
        assert_eq!(t.done[0].end, Some(2));
    }

    #[test]
    fn a_refusal_reads_back_as_its_error_text() {
        assert_eq!(
            refusal(br#"{"error": "not a parked task"}"#).as_deref(),
            Some("not a parked task")
        );
        assert_eq!(refusal(b"<html>nope</html>"), None);
        assert_eq!(refusal(br#"{"message": "ok"}"#), None);
    }

    #[test]
    fn attachments_stops_and_search_hits_parse() {
        let m: Msg = serde_json::from_str(
            r#"{"id":4,"from":"me","text":"📎 plan.pdf","attachment":{"file":"ab.pdf","title":"plan v2.pdf"}}"#,
        )
        .unwrap();
        let a = m.attachment.unwrap();
        assert_eq!(
            (a.file.as_str(), a.title.as_str()),
            ("ab.pdf", "plan v2.pdf")
        );
        assert!(a.url().ends_with("/api/file/ab.pdf?name=plan%20v2.pdf"));
        assert!(serde_json::from_str::<Msg>(r#"{"id":5}"#)
            .unwrap()
            .attachment
            .is_none());
        let t: Thread = serde_json::from_str(r#"{"messages":[],"lead_busy":true}"#).unwrap();
        assert!(t.lead_busy);
        assert_eq!(t.context, None);
        let t: Thread = serde_json::from_str(r#"{"messages":[],"context":28}"#).unwrap();
        assert_eq!(t.context, Some(28));
        let t: Thread = serde_json::from_str(r#"{"messages":[],"context":null}"#).unwrap();
        assert_eq!(t.context, None);
        assert!(t.crew.is_empty());
        let t: Thread = serde_json::from_str(r#"{"messages":[],"crew":[0,3]}"#).unwrap();
        assert_eq!(t.crew, [0, 3]);
        let h: Hits = serde_json::from_str(
            r#"{"results":[{"ws":"main","circle":"PM","id":9,"from":"lead","ts":3,"text":"hi"}]}"#,
        )
        .unwrap();
        assert_eq!(h.results[0].circle, "PM");
        assert_eq!(h.results[0].id, 9);
        let p: PaneRow = serde_json::from_str(r#"{"at":"Builder","helper":true}"#).unwrap();
        assert!(p.helper && !p.lead);
    }

    #[test]
    fn uploads_are_checked_before_they_are_sent() {
        let dir = std::env::temp_dir().join(format!("chat-up-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("Plan.PDF");
        std::fs::write(&ok, b"x").unwrap();
        assert!(check_upload(&ok).is_ok());
        let bad = dir.join("run.sh");
        std::fs::write(&bad, b"x").unwrap();
        assert_eq!(
            check_upload(&bad).unwrap_err().to_string(),
            "can't send .sh files"
        );
        assert!(check_upload(&dir.join("gone.pdf")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn base_resolution_order() {
        let file = Some("\n  https://file.example:1  \nignored\n");
        assert_eq!(
            resolve_base(Some(" https://env.example "), file),
            "https://env.example"
        );
        assert_eq!(resolve_base(Some(""), file), "https://file.example:1");
        assert_eq!(resolve_base(None, file), "https://file.example:1");
        assert_eq!(resolve_base(Some("  "), Some("\n\n")), "");
        assert_eq!(resolve_base(None, None), "");
    }

    #[test]
    fn no_url_message_names_both_settings() {
        assert!(NO_URL_MESSAGE.contains("SEANCE_INVOKE_URL"));
        assert!(NO_URL_MESSAGE.contains("~/.config/seance/invoke-url"));
    }

    #[test]
    fn origin_drops_the_path() {
        assert_eq!(
            origin("https://invoke.example:8443"),
            "https://invoke.example:8443"
        );
        assert_eq!(
            origin("http://localhost:9000/invoke/"),
            "http://localhost:9000"
        );
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(enc("alpha-circle"), "alpha-circle");
        assert_eq!(enc("a b&c"), "a%20b%26c");
    }

    #[test]
    fn file_names_cannot_leave_the_cache() {
        assert!(file("").is_err());
        assert!(file("../").is_err());
        assert!(file("x/.hidden").is_err());
    }
}
