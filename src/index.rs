//! The session index: every Claude Code session as one entry, built from
//! `~/.claude` and cached at `%APPDATA%\tern-claude\index.json`.
//!
//! Sources (checked 2026-11-05 against Claude Code 2.1.318):
//! - `projects/<slug>/<id>.jsonl`: the transcript. `ai-title` records carry
//!   Claude's own title (rewritten per turn, so the newest sits near the end),
//!   `last-prompt` records come one per typed prompt, and ordinary records
//!   carry `timestamp`, `cwd`, `gitBranch` and `entrypoint` (`cli` or
//!   `claude-vscode`).
//! - `history.jsonl`: typed prompts with their sessionId. CLI sessions only;
//!   the VS Code extension does not write it.
//! - `sessions/<pid>.json`: running sessions with status, display name and the
//!   process start time. The `.key` files beside them hold tokens and are
//!   never opened.
//!
//! A session with no typed prompt at all is left out unless it is running:
//! those are IDE-opened shells nobody talked to.

use std::{
	collections::HashMap,
	fs,
	path::{Path, PathBuf},
	time::Instant,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Bump when the entry shape changes; an older cache is rebuilt from scratch.
pub const VERSION: u32 = 2;
/// How much of a transcript's end is parsed for the newest title and state.
const TAIL: usize = 64 * 1024;
/// How much of a transcript's start is parsed for the first timestamp.
const HEAD: usize = 32 * 1024;
/// The log rolls over to `index.log.1` past this size.
const LOG_CAP: u64 = 256 * 1024;

/// A running Claude process attached to a session.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Live {
	pub pid:    u32,
	/// `busy` or `idle`.
	pub status: String,
	/// Claude's display name for the session, e.g. `repos-d8`.
	pub name:   String,
	/// `interactive` or `background`.
	pub kind:   String,
	/// When the process started (ISO, UTC).
	pub since:  String,
	/// The process's working set in MB (0 when unknown).
	pub mem_mb: u32,
}

/// One session.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Entry {
	pub id:          String,
	pub title:       String,
	/// Where the title came from: `custom`, `ai`, `prompt`, `name` (a running
	/// session with no transcript yet) or `none`.
	pub title_from:  String,
	/// The folder the session started in.
	pub project:     String,
	/// The folder it was last in.
	pub cwd:         String,
	/// The folder `claude --resume` must run in: the one whose slug names the
	/// transcript's directory (`C:\Users\x\repos` ↔ `C--Users-x-repos`).
	pub root:        String,
	pub branch:      String,
	/// `cli` or `claude-vscode`.
	pub entrypoint:  String,
	/// First and latest activity (ISO, UTC).
	pub first:       String,
	pub last:        String,
	/// Typed prompts.
	pub prompts:     u32,
	pub last_prompt: String,
	/// Transcript size and mtime: the cache key.
	pub bytes:       u64,
	pub mtime_ms:    i64,
	pub path:        String,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub live:        Option<Live>,
}

/// The whole index as written to disk.
#[derive(Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Index {
	pub version:     u32,
	pub generated:   String,
	pub claude_home: String,
	pub sessions:    Vec<Entry>,
}

/// What one build did.
#[derive(Debug, Default)]
pub struct Stats {
	/// Transcripts found (plus running sessions without one).
	pub found:   usize,
	/// Entries kept after the no-prompt rule.
	pub shown:   usize,
	pub live:    usize,
	/// Transcripts re-read because they changed.
	pub scanned: usize,
	pub ms:      u128,
}

// ---------------------------------------------------------------- paths

fn home_dir() -> PathBuf {
	std::env::var_os("USERPROFILE")
		.or_else(|| std::env::var_os("HOME"))
		.map(PathBuf::from)
		.unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.claude`, or `CLAUDE_CONFIG_DIR` when set.
pub fn claude_home() -> PathBuf {
	match std::env::var("CLAUDE_CONFIG_DIR") {
		Ok(d) if !d.trim().is_empty() => PathBuf::from(d),
		_ => home_dir().join(".claude"),
	}
}

/// `%APPDATA%\tern-claude` (or `~/.config/tern-claude`).
pub fn data_dir() -> PathBuf {
	std::env::var_os("APPDATA")
		.map(PathBuf::from)
		.unwrap_or_else(|| home_dir().join(".config"))
		.join("tern-claude")
}

pub fn index_path() -> PathBuf {
	data_dir().join("index.json")
}

pub fn log_path() -> PathBuf {
	data_dir().join("index.log")
}

// ---------------------------------------------------------------- build

/// The cached index, if there is one of the current shape.
pub fn load() -> Option<Index> {
	let text = fs::read_to_string(index_path()).ok()?;
	let idx: Index = serde_json::from_str(&text).ok()?;
	(idx.version == VERSION).then_some(idx)
}

/// Builds the index, re-reading only transcripts that changed since `prev`.
pub fn build(prev: Option<&Index>) -> Result<(Index, Stats)> {
	let t0 = Instant::now();
	let home = claude_home();
	let prev: HashMap<&str, &Entry> = prev
		.map(|p| p.sessions.iter().map(|e| (e.id.as_str(), e)).collect())
		.unwrap_or_default();
	let history = read_history(&home.join("history.jsonl"));
	let mut stats = Stats::default();
	let mut sessions: Vec<Entry> = Vec::new();

	let projects = home.join("projects");
	for dir in list_dir(&projects, |m| m.is_dir()) {
		for file in list_dir(&dir, |m| m.is_file()) {
			let Some(id) = uuid_stem(&file) else { continue };
			let Ok(meta) = fs::metadata(&file) else { continue };
			let bytes = meta.len();
			let mtime_ms = mtime_ms(&meta);
			let mut entry = match prev.get(id.as_str()) {
				Some(p) if p.bytes == bytes && p.mtime_ms == mtime_ms => {
					let mut e = (*p).clone();
					e.live = None;
					e
				},
				_ => {
					stats.scanned += 1;
					scan(&file, &id, bytes, mtime_ms)
				},
			};
			if let Some(h) = history.get(&id) {
				merge_history(&mut entry, h);
			}
			sessions.push(entry);
		}
	}

	for lf in read_live(&home.join("sessions")) {
		let since = lf.started_at.as_ref().and_then(num).map(iso_ms).unwrap_or_default();
		let seen = [&lf.status_updated_at, &lf.updated_at]
			.into_iter()
			.filter_map(|v| v.as_ref().and_then(num))
			.max()
			.map(iso_ms)
			.unwrap_or_default();
		let pid = lf.pid.as_ref().and_then(num).unwrap_or_default() as u32;
		let live = Live {
			pid,
			status: lf.status.clone(),
			name: lf.name.clone(),
			kind: lf.kind.clone(),
			since: since.clone(),
			mem_mb: memory_mb(pid),
		};
		match sessions.iter_mut().find(|e| e.id == lf.session_id) {
			Some(e) => {
				e.live = Some(live);
				if seen > e.last {
					e.last = seen;
				}
			},
			None => sessions.push(Entry {
				id: lf.session_id.clone(),
				title: if lf.name.is_empty() { "(new session)".into() } else { lf.name.clone() },
				title_from: "name".into(),
				project: tidy_path(&lf.cwd),
				cwd: tidy_path(&lf.cwd),
				root: tidy_path(&lf.cwd),
				entrypoint: lf.entrypoint.clone(),
				first: since.clone(),
				last: if seen.is_empty() { since } else { seen },
				live: Some(live),
				..Default::default()
			}),
		}
	}

	stats.found = sessions.len();
	sessions.retain(|e| e.prompts > 0 || e.live.is_some());
	stats.shown = sessions.len();
	stats.live = sessions.iter().filter(|e| e.live.is_some()).count();
	sessions.sort_by(|a, b| b.last.cmp(&a.last));
	stats.ms = t0.elapsed().as_millis();
	let idx = Index {
		version: VERSION,
		generated: now_iso(),
		claude_home: home.display().to_string(),
		sessions,
	};
	Ok((idx, stats))
}

/// Writes the index atomically: a per-process temp file, then a rename.
pub fn save(idx: &Index) -> Result<PathBuf> {
	let dir = data_dir();
	fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
	let path = index_path();
	let tmp = dir.join(format!("index.{}.tmp", std::process::id()));
	fs::write(&tmp, serde_json::to_vec_pretty(idx)?).with_context(|| format!("writing {}", tmp.display()))?;
	let mut last = None;
	for _ in 0..4 {
		match fs::rename(&tmp, &path) {
			Ok(()) => return Ok(path),
			Err(e) => {
				last = Some(e);
				std::thread::sleep(std::time::Duration::from_millis(40));
			},
		}
	}
	let _ = fs::remove_file(&tmp);
	Err(last.unwrap()).with_context(|| format!("replacing {}", path.display()))
}

/// Appends one line to the log, rolling it over past `LOG_CAP`.
pub fn log(line: &str) {
	use std::io::Write;
	let p = log_path();
	if fs::metadata(&p).map(|m| m.len() > LOG_CAP).unwrap_or(false) {
		let _ = fs::rename(&p, p.with_extension("log.1"));
	}
	if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
		let _ = writeln!(f, "{line}");
	}
}

/// Load, rebuild, save, log. `why` names the trigger (a hook event, `manual`, `board`).
pub fn refresh(why: &str) -> Result<(Index, Stats)> {
	let prev = load();
	let (idx, stats) = build(prev.as_ref())?;
	save(&idx)?;
	log(&format!(
		"{}  why={why}  found={} shown={} live={} scanned={} took={}ms",
		idx.generated, stats.found, stats.shown, stats.live, stats.scanned, stats.ms
	));
	Ok((idx, stats))
}

// ---------------------------------------------------------------- transcripts

/// The fields the index reads from any transcript record.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Rec {
	#[serde(rename = "type")]
	kind:         Option<String>,
	#[serde(rename = "aiTitle")]
	ai_title:     Option<String>,
	#[serde(rename = "customTitle")]
	custom_title: Option<String>,
	#[serde(rename = "lastPrompt")]
	last_prompt:  Option<String>,
	timestamp:    Option<Value>,
	cwd:          Option<String>,
	#[serde(rename = "gitBranch")]
	git_branch:   Option<String>,
	entrypoint:   Option<String>,
}

impl Rec {
	fn timestamp(&self) -> Option<String> {
		self.timestamp.as_ref()?.as_str().map(str::to_string)
	}
}

/// Reads one transcript: prompt count from the whole file, first timestamp from
/// the head, newest title and state from the tail.
fn scan(path: &Path, id: &str, bytes: u64, mtime_ms: i64) -> Entry {
	let mut e = Entry {
		id: id.to_string(),
		bytes,
		mtime_ms,
		path: path.display().to_string(),
		title_from: "none".into(),
		..Default::default()
	};
	let Ok(data) = fs::read(path) else {
		e.title = "(unreadable transcript)".into();
		return e;
	};
	let text = String::from_utf8_lossy(&data);
	e.prompts = text.matches("\"type\":\"last-prompt\"").count() as u32;

	let head_end = boundary_down(&text, HEAD.min(text.len()));
	for line in text[..head_end].lines() {
		if !line.contains("\"timestamp\":\"") {
			continue;
		}
		let Ok(r) = serde_json::from_str::<Rec>(line) else { continue };
		if let Some(ts) = r.timestamp() {
			e.first = ts;
			if let Some(c) = r.cwd {
				e.project = tidy_path(&c);
			}
			if let Some(ep) = r.entrypoint {
				e.entrypoint = ep;
			}
			break;
		}
	}

	let start = boundary_up(&text, text.len().saturating_sub(TAIL));
	let mut lines = text[start..].lines();
	if start > 0 {
		lines.next();
	}
	let (mut ai, mut custom, mut prompt) = (None, None, None);
	for line in lines {
		let Ok(mut r) = serde_json::from_str::<Rec>(line) else { continue };
		if let Some(ts) = r.timestamp() {
			e.last = ts;
		}
		match r.kind.as_deref() {
			Some("ai-title") => ai = r.ai_title.take().or(ai),
			Some("custom-title") => custom = r.custom_title.take().or(custom),
			Some("last-prompt") => prompt = r.last_prompt.take().or(prompt),
			_ => {},
		}
		if let Some(c) = r.cwd {
			e.cwd = tidy_path(&c);
		}
		if let Some(b) = r.git_branch {
			e.branch = b;
		}
		if let Some(ep) = r.entrypoint {
			e.entrypoint = ep;
		}
	}
	if ai.is_none() {
		ai = last_record(&text, "\"type\":\"ai-title\"").and_then(|r| r.ai_title);
	}
	if prompt.is_none() && e.prompts > 0 {
		prompt = last_record(&text, "\"type\":\"last-prompt\"").and_then(|r| r.last_prompt);
	}

	if let Some(t) = custom.filter(|t| !t.trim().is_empty()) {
		e.title = one_line(&t, 80);
		e.title_from = "custom".into();
	} else if let Some(t) = ai.filter(|t| !t.trim().is_empty()) {
		e.title = one_line(&t, 80);
		e.title_from = "ai".into();
	} else if let Some(p) = prompt.as_deref().filter(|p| !p.trim().is_empty()) {
		e.title = one_line(p, 72);
		e.title_from = "prompt".into();
	} else {
		e.title = "(untitled)".into();
	}
	e.last_prompt = prompt.map(|p| one_line(&p, 200)).unwrap_or_default();
	if e.last.is_empty() {
		e.last = iso_ms(mtime_ms);
	}
	if e.first.is_empty() {
		e.first = e.last.clone();
	}
	if e.cwd.is_empty() {
		e.cwd = e.project.clone();
	}
	if e.project.is_empty() {
		e.project = e.cwd.clone();
	}
	e.root = resume_root(path, &e.project, &e.cwd);
	e
}

/// The folder to resume from: Claude files a transcript under the slug of the
/// folder it started in, so walk up from the last cwd (then the first) until a
/// folder's slug matches the transcript's directory name. Falls back to the
/// first cwd.
fn resume_root(path: &Path, project: &str, cwd: &str) -> String {
	let Some(dir) = path.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str()) else {
		return project.to_string();
	};
	let dir = dir.to_ascii_lowercase();
	let mut candidates: Vec<String> = Vec::new();
	let mut p = Some(Path::new(cwd));
	while let Some(c) = p {
		let s = c.to_string_lossy().to_string();
		if !s.is_empty() {
			candidates.push(s);
		}
		p = c.parent();
	}
	candidates.push(project.to_string());
	candidates
		.into_iter()
		.find(|c| slug(c) == dir)
		.unwrap_or_else(|| project.to_string())
}

/// Claude's project-folder slug: every non-alphanumeric character → `-`.
fn slug(path: &str) -> String {
	path.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect()
}

/// The last record in `text` whose line contains `needle`.
fn last_record(text: &str, needle: &str) -> Option<Rec> {
	let pos = text.rfind(needle)?;
	let start = text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
	let end = text[pos..].find('\n').map(|i| pos + i).unwrap_or(text.len());
	serde_json::from_str(&text[start..end]).ok()
}

// ---------------------------------------------------------------- history

/// What `history.jsonl` says about one session.
struct Hist {
	count:    u32,
	first_ms: i64,
	last_ms:  i64,
	last:     String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct HistLine {
	display:    String,
	timestamp:  Option<Value>,
	#[serde(rename = "sessionId")]
	session_id: String,
}

fn read_history(path: &Path) -> HashMap<String, Hist> {
	let mut out: HashMap<String, Hist> = HashMap::new();
	let Ok(text) = fs::read_to_string(path) else { return out };
	for line in text.lines() {
		let Ok(h) = serde_json::from_str::<HistLine>(line) else { continue };
		if h.session_id.is_empty() {
			continue;
		}
		let ms = h.timestamp.as_ref().and_then(num).unwrap_or(0);
		let e = out.entry(h.session_id).or_insert(Hist { count: 0, first_ms: ms, last_ms: ms, last: String::new() });
		e.count += 1;
		if ms < e.first_ms {
			e.first_ms = ms;
		}
		if ms >= e.last_ms {
			e.last_ms = ms;
			e.last = h.display;
		}
	}
	out
}

fn merge_history(e: &mut Entry, h: &Hist) {
	e.prompts = e.prompts.max(h.count);
	if e.last_prompt.is_empty() {
		e.last_prompt = one_line(&h.last, 200);
	}
	if h.first_ms > 0 {
		let first = iso_ms(h.first_ms);
		if e.first.is_empty() || first < e.first {
			e.first = first;
		}
		let last = iso_ms(h.last_ms);
		if last > e.last {
			e.last = last;
		}
	}
}

// ---------------------------------------------------------------- live sessions

/// `sessions/<pid>.json`, the fields the index uses.
#[derive(Deserialize, Default)]
#[serde(default)]
struct LiveFile {
	pid:               Option<Value>,
	#[serde(rename = "sessionId")]
	session_id:        String,
	cwd:               String,
	#[serde(rename = "startedAt")]
	started_at:        Option<Value>,
	kind:              String,
	entrypoint:        String,
	name:              String,
	status:            String,
	#[serde(rename = "procStart")]
	proc_start:        Option<Value>,
	#[serde(rename = "statusUpdatedAt")]
	status_updated_at: Option<Value>,
	#[serde(rename = "updatedAt")]
	updated_at:        Option<Value>,
}

/// Running sessions: the `.json` files whose process is still the one that
/// wrote them (same pid and start time). Stale files from killed processes are
/// skipped; `.key` files are never opened.
fn read_live(dir: &Path) -> Vec<LiveFile> {
	let mut out = Vec::new();
	for file in list_dir(dir, |m| m.is_file()) {
		if file.extension().and_then(|x| x.to_str()) != Some("json") {
			continue;
		}
		let Ok(text) = fs::read_to_string(&file) else { continue };
		let Ok(lf) = serde_json::from_str::<LiveFile>(&text) else { continue };
		if lf.session_id.is_empty() {
			continue;
		}
		let Some(pid) = lf.pid.as_ref().and_then(num) else { continue };
		let start = lf.proc_start.as_ref().and_then(num).map(|v| v as u64);
		if !alive(pid as u32, start) {
			continue;
		}
		out.push(lf);
	}
	out
}

#[cfg(windows)]
fn alive(pid: u32, start: Option<u64>) -> bool {
	match win::process_times(pid) {
		Some((created, exited)) => exited == 0 && start.is_none_or(|s| s == created),
		None => false,
	}
}

#[cfg(not(windows))]
fn alive(pid: u32, _start: Option<u64>) -> bool {
	Path::new("/proc").join(pid.to_string()).exists()
}

#[cfg(windows)]
fn memory_mb(pid: u32) -> u32 {
	win::working_set(pid).map(|b| (b / (1024 * 1024)) as u32).unwrap_or(0)
}

#[cfg(not(windows))]
fn memory_mb(pid: u32) -> u32 {
	fs::read_to_string(format!("/proc/{pid}/statm"))
		.ok()
		.and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
		.map(|pages| (pages * 4096 / (1024 * 1024)) as u32)
		.unwrap_or(0)
}

/// The last assistant reply in a transcript (its final text block), read
/// from the tail. When replies end with a standalone recap, that reply is the
/// natural handoff note.
pub fn last_reply(path: &Path) -> Option<String> {
	use std::io::{Read, Seek, SeekFrom};
	let mut f = fs::File::open(path).ok()?;
	let len = f.metadata().ok()?.len();
	let start = len.saturating_sub((TAIL as u64) * 4);
	f.seek(SeekFrom::Start(start)).ok()?;
	let mut buf = Vec::new();
	f.read_to_end(&mut buf).ok()?;
	let text = String::from_utf8_lossy(&buf);
	for line in text.lines().rev() {
		if !line.contains("\"type\":\"assistant\"") {
			continue;
		}
		let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
		let Some(content) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else { continue };
		let texts: Vec<&str> = content
			.iter()
			.filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
			.filter_map(|b| b.get("text").and_then(|t| t.as_str()))
			.filter(|t| !t.trim().is_empty())
			.collect();
		if let Some(t) = texts.last() {
			return Some(t.trim().to_string());
		}
	}
	None
}

#[cfg(windows)]
mod win {
	use std::ffi::c_void;

	#[repr(C)]
	#[derive(Clone, Copy, Default)]
	struct FileTime {
		lo: u32,
		hi: u32,
	}

	/// `PROCESS_MEMORY_COUNTERS`.
	#[repr(C)]
	#[derive(Default)]
	struct MemCounters {
		cb:                           u32,
		page_fault_count:             u32,
		peak_working_set_size:        usize,
		working_set_size:             usize,
		quota_peak_paged_pool_usage:  usize,
		quota_paged_pool_usage:       usize,
		quota_peak_nonpaged_pool:     usize,
		quota_nonpaged_pool_usage:    usize,
		pagefile_usage:               usize,
		peak_pagefile_usage:          usize,
	}

	#[link(name = "kernel32")]
	unsafe extern "system" {
		fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
		fn GetProcessTimes(
			h: *mut c_void,
			creation: *mut FileTime,
			exit: *mut FileTime,
			kernel: *mut FileTime,
			user: *mut FileTime,
		) -> i32;
		fn K32GetProcessMemoryInfo(h: *mut c_void, counters: *mut MemCounters, cb: u32) -> i32;
		fn CloseHandle(h: *mut c_void) -> i32;
	}

	const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

	/// The process's working set in bytes.
	pub fn working_set(pid: u32) -> Option<u64> {
		// SAFETY: plain kernel32 calls with a correctly sized out-struct; the
		// handle is closed on every path.
		unsafe {
			let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
			if h.is_null() {
				return None;
			}
			let mut m = MemCounters { cb: std::mem::size_of::<MemCounters>() as u32, ..Default::default() };
			let ok = K32GetProcessMemoryInfo(h, &mut m, m.cb);
			CloseHandle(h);
			(ok != 0).then_some(m.working_set_size as u64)
		}
	}

	fn ft(t: FileTime) -> u64 {
		((t.hi as u64) << 32) | t.lo as u64
	}

	/// The process's creation and exit times as FILETIME integers (exit is 0
	/// while it runs), or None when there is no such process.
	pub fn process_times(pid: u32) -> Option<(u64, u64)> {
		// SAFETY: plain kernel32 calls with valid out-pointers; the handle is
		// closed on every path.
		unsafe {
			let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
			if h.is_null() {
				return None;
			}
			let (mut c, mut e, mut k, mut u) = (FileTime::default(), FileTime::default(), FileTime::default(), FileTime::default());
			let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
			CloseHandle(h);
			(ok != 0).then(|| (ft(c), ft(e)))
		}
	}
}

// ---------------------------------------------------------------- helpers

fn list_dir(dir: &Path, keep: impl Fn(&fs::Metadata) -> bool) -> Vec<PathBuf> {
	let mut out: Vec<PathBuf> = fs::read_dir(dir)
		.map(|rd| {
			rd.filter_map(|e| e.ok())
				.filter(|e| e.metadata().map(|m| keep(&m)).unwrap_or(false))
				.map(|e| e.path())
				.collect()
		})
		.unwrap_or_default();
	out.sort();
	out
}

/// `<uuid>.jsonl` → the uuid.
fn uuid_stem(path: &Path) -> Option<String> {
	if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
		return None;
	}
	let stem = path.file_stem()?.to_str()?;
	let ok = stem.len() == 36
		&& stem.chars().enumerate().all(|(i, c)| match i {
			8 | 13 | 18 | 23 => c == '-',
			_ => c.is_ascii_hexdigit(),
		});
	ok.then(|| stem.to_ascii_lowercase())
}

fn mtime_ms(meta: &fs::Metadata) -> i64 {
	meta.modified()
		.ok()
		.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
		.map(|d| d.as_millis() as i64)
		.unwrap_or(0)
}

/// A JSON number, or a string holding one, as i64.
fn num(v: &Value) -> Option<i64> {
	match v {
		Value::Number(n) => n.as_i64().or_else(|| n.as_u64().map(|u| u as i64)).or_else(|| n.as_f64().map(|f| f as i64)),
		Value::String(s) => s.trim().parse().ok(),
		_ => None,
	}
}

/// Epoch milliseconds → `2026-11-05T19:49:15.752Z`.
pub fn iso_ms(ms: i64) -> String {
	DateTime::<Utc>::from_timestamp_millis(ms)
		.map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
		.unwrap_or_default()
}

pub fn now_iso() -> String {
	Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// An ISO timestamp as local `MM-DD HH:MM`, or the input when it does not parse.
pub fn local_short(iso: &str) -> String {
	DateTime::parse_from_rfc3339(iso)
		.map(|t| t.with_timezone(&Local).format("%m-%d %H:%M").to_string())
		.unwrap_or_else(|_| iso.chars().take(11).collect())
}

/// Uppercases a Windows drive letter so VS Code's `c:\` and the CLI's `C:\` match.
fn tidy_path(p: &str) -> String {
	let mut chars = p.chars();
	match (chars.next(), chars.next()) {
		(Some(d), Some(':')) if d.is_ascii_lowercase() => format!("{}{}", d.to_ascii_uppercase(), &p[1..]),
		_ => p.to_string(),
	}
}

/// Whitespace collapsed to single spaces, cut to `max` chars with an ellipsis.
pub fn one_line(s: &str, max: usize) -> String {
	let joined: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
	if joined.chars().count() <= max {
		return joined;
	}
	let cut: String = joined.chars().take(max.saturating_sub(1)).collect();
	format!("{}…", cut.trim_end())
}

fn boundary_down(s: &str, mut i: usize) -> usize {
	while i > 0 && !s.is_char_boundary(i) {
		i -= 1;
	}
	i
}

fn boundary_up(s: &str, mut i: usize) -> usize {
	while i < s.len() && !s.is_char_boundary(i) {
		i += 1;
	}
	i
}

// ---------------------------------------------------------------- CLI

/// `tern-claude index [--why NAME] [--quiet]`: rebuild and save the index.
/// In `--quiet` mode (hooks) errors go to the log and the exit code stays 0,
/// so a broken index never blocks Claude.
pub fn cli_index(args: &[&str]) -> Result<()> {
	let mut why = "manual".to_string();
	let mut quiet = false;
	let mut it = args.iter();
	while let Some(a) = it.next() {
		match *a {
			"--why" => why = it.next().map(|s| s.to_string()).unwrap_or(why),
			"--quiet" | "-q" => quiet = true,
			other => anyhow::bail!("unknown option {other}"),
		}
	}
	match refresh(&why) {
		Ok((idx, s)) => {
			if !quiet {
				println!(
					"indexed {} sessions: {} shown, {} live, {} re-read, {} ms → {}",
					s.found,
					s.shown,
					s.live,
					s.scanned,
					s.ms,
					index_path().display()
				);
				let _ = idx;
			}
			Ok(())
		},
		Err(e) if quiet => {
			log(&format!("{}  why={why}  error={e:#}", now_iso()));
			eprintln!("tern-claude index: {e:#}");
			Ok(())
		},
		Err(e) => Err(e),
	}
}

/// `tern-claude list`: the index as a table, newest first.
pub fn cli_list(args: &[&str]) -> Result<()> {
	if !args.is_empty() {
		anyhow::bail!("list takes no options");
	}
	let (idx, s) = refresh("list")?;
	println!("{:<8}  {:<11}  {:<6}  {:<4}  {:>4}  {:<56}  folder", "session", "last", "live", "from", "msgs", "title");
	for e in &idx.sessions {
		let live = e.live.as_ref().map(|l| l.status.as_str()).unwrap_or("");
		let from = match e.entrypoint.as_str() {
			"claude-vscode" => "vsc",
			"cli" => "cli",
			other => other,
		};
		let folder = if e.branch.is_empty() || e.branch == "HEAD" {
			e.cwd.clone()
		} else {
			format!("{}  ({})", e.cwd, e.branch)
		};
		println!(
			"{:<8}  {:<11}  {:<6}  {:<4}  {:>4}  {:<56}  {}",
			&e.id[..8],
			local_short(&e.last),
			live,
			from,
			e.prompts,
			one_line(&e.title, 56),
			folder
		);
	}
	println!(
		"\n{} shown of {} found, {} live, {} re-read, {} ms. index: {}",
		s.shown,
		s.found,
		s.live,
		s.scanned,
		s.ms,
		index_path().display()
	);
	Ok(())
}
