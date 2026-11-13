//! The board: every Claude Code session as a Tern card.
//!
//! Running sessions sit at the top (the VS Code dropdown's job), the rest
//! follow newest first. Continue reopens a finished session with
//! `claude --resume` in a new tab, focuses the tab of one already running in
//! this window, or forks one running elsewhere. Hide only touches
//! `%APPDATA%\tern-claude\hidden.json`; nothing under `~/.claude` changes.

use std::{
	collections::HashSet,
	fs,
	time::{Duration, Instant},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use tern_sdk::{
	Input, Options, Session, SurfaceOptions,
	ui::{self, Status, View, html},
};

use crate::index::{self, Entry, Index, Stats, local_short, one_line};

/// The surface's look.
const CSS: &str = ".board{display:flex;flex-direction:column;gap:8px;padding:8px 12px 16px;max-width:900px;font-family:Geist,system-ui,sans-serif;font-size:12.9px}\
.top{display:flex;align-items:baseline;gap:12px;flex-wrap:wrap;padding-bottom:2px}\
.top .t{font-weight:700;font-size:16px}\
.top .n{color:var(--sf-c-muted)}\
.top .f{font-family:ui-monospace,monospace;padding:1px 8px;border-radius:6px;border:1px solid color-mix(in srgb,var(--sf-c-muted) 45%,transparent)}\
.top .f.on{border-color:var(--sf-c-accent)}\
.sec{color:var(--sf-c-muted);font-weight:600;letter-spacing:.04em;text-transform:uppercase;font-size:11px;padding-top:6px}\
.meta{display:flex;gap:8px;align-items:center;flex-wrap:wrap;color:var(--sf-c-muted)}\
.meta .w{color:var(--sf-c-fg)}\
.meta .sel{color:var(--sf-c-accent);font-weight:700}\
.btns{display:flex;gap:8px;padding-top:6px;flex-wrap:wrap}\
.btns button{padding:3px 12px;border-radius:6px;cursor:pointer;border:1px solid color-mix(in srgb,var(--sf-c-muted) 45%,transparent)}\
.btns button.go{background:color-mix(in srgb,var(--sf-c-accent) 22%,transparent);border-color:var(--sf-c-accent);font-weight:600}\
.note{color:var(--sf-c-muted);padding-top:4px}\
.err{color:var(--sf-bad,#f85149);padding-top:4px}\
.empty{color:var(--sf-c-muted);padding:12px 0}";

/// How often the board re-reads `~/.claude` on its own.
const AUTO_REFRESH: Duration = Duration::from_secs(30);

/// Appends one line to `%APPDATA%\tern-claude\board.log` (inputs, actions,
/// errors; rolled over past 256 KB).
fn dbg(line: impl AsRef<str>) {
	use std::io::Write;
	let p = index::data_dir().join("board.log");
	if fs::metadata(&p).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
		let _ = fs::rename(&p, p.with_extension("log.1"));
	}
	if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&p) {
		let _ = writeln!(f, "{}  {}", index::now_iso(), line.as_ref());
	}
}

/// What a click or key sends back.
#[derive(Clone, Debug)]
enum Msg {
	/// Open, focus or fork the session.
	Continue(String),
	/// A new session that starts from this one's context.
	Fork(String),
	/// A fresh session handed a short brief (title, last prompt, last reply,
	/// transcript path) instead of the whole context.
	Brief(String),
	/// Close the Tern tab a running session lives in; the transcript stays.
	Close(String),
	/// Take the card off the board.
	Hide(String),
	/// Put a hidden card back.
	Restore(String),
	/// Make this the selected card.
	Select(String),
	Refresh,
	ToggleHidden,
	Quit,
}

/// The board's state.
struct App {
	idx:        Index,
	stats:      Stats,
	hidden:     HashSet<String>,
	show_hidden: bool,
	filter:     String,
	/// Keys type into the filter.
	filtering:  bool,
	/// The selected card's id.
	cursor:     Option<String>,
	note:       String,
	error:      bool,
	refreshed:  Instant,
}

impl App {
	fn new() -> Result<Self> {
		let (idx, stats) = index::refresh("board")?;
		Ok(Self {
			idx,
			stats,
			hidden: load_hidden(),
			show_hidden: false,
			filter: String::new(),
			filtering: false,
			cursor: None,
			note: String::new(),
			error: false,
			refreshed: Instant::now(),
		})
	}

	fn refresh(&mut self) {
		match index::refresh("board") {
			Ok((idx, stats)) => {
				self.idx = idx;
				self.stats = stats;
			},
			Err(e) => self.fail(format!("index: {e:#}")),
		}
		self.refreshed = Instant::now();
	}

	fn say(&mut self, what: String) {
		dbg(format!("ok: {what}"));
		self.note = what;
		self.error = false;
	}

	fn fail(&mut self, what: String) {
		dbg(format!("error: {what}"));
		self.note = what;
		self.error = true;
	}

	/// Running sessions first (busy before idle), then the rest newest first;
	/// hidden ones only when shown; all matched against the filter.
	fn visible(&self) -> Vec<&Entry> {
		let q = self.filter.trim().to_lowercase();
		let mut out: Vec<&Entry> = self
			.idx
			.sessions
			.iter()
			.filter(|e| self.show_hidden || !self.hidden.contains(&e.id))
			.filter(|e| q.is_empty() || matches(e, &q))
			.collect();
		out.sort_by_key(|e| match &e.live {
			Some(l) if l.status == "busy" => 0,
			Some(_) => 1,
			None => 2,
		});
		out
	}

	fn selected(&self) -> Option<String> {
		let v = self.visible();
		if let Some(c) = &self.cursor {
			if v.iter().any(|e| &e.id == c) {
				return Some(c.clone());
			}
		}
		v.first().map(|e| e.id.clone())
	}

	fn step(&mut self, by: isize) {
		let v: Vec<String> = self.visible().iter().map(|e| e.id.clone()).collect();
		if v.is_empty() {
			self.cursor = None;
			return;
		}
		let at = self.selected().and_then(|c| v.iter().position(|x| *x == c)).unwrap_or(0) as isize;
		let next = (at + by).clamp(0, v.len() as isize - 1) as usize;
		self.cursor = Some(v[next].clone());
	}

	fn entry(&self, id: &str) -> Option<Entry> {
		self.idx.sessions.iter().find(|e| e.id == id).cloned()
	}

	fn go(&mut self, id: &str) {
		let Some(e) = self.entry(id) else { return };
		self.cursor = Some(id.to_string());
		let outcome = match &e.live {
			Some(l) if l.kind == "background" => {
				new_tab(&e.root, &["claude", "attach", &e.id]).map(|t| format!("attached to {} in {t}", &e.id[..8]))
			},
			Some(l) => match find_pane(&e) {
				Ok(Some(pane)) => focus(pane.id).map(|_| format!("focused tab {} ({})", pane.tab, one_line(&pane.title, 50))),
				Ok(None) => new_tab(&e.root, &["claude", "--resume", &e.id, "--fork-session"]).map(|t| {
					format!("{} is running outside this window (pid {}); opened a fork in {t}", &e.id[..8], l.pid)
				}),
				Err(err) => Err(err),
			},
			None => new_tab(&e.root, &["claude", "--resume", &e.id]).map(|t| format!("continuing {} in {t}", &e.id[..8])),
		};
		match outcome {
			Ok(what) => self.say(what),
			Err(err) => self.fail(format!("{err:#}")),
		}
	}

	fn fork(&mut self, id: &str) {
		let Some(e) = self.entry(id) else { return };
		self.cursor = Some(id.to_string());
		match new_tab(&e.root, &["claude", "--resume", &e.id, "--fork-session"]) {
			Ok(t) => self.say(format!("forked {} in {t}", &e.id[..8])),
			Err(err) => self.fail(format!("{err:#}")),
		}
	}

	/// Brief: write a handoff note and start a fresh session that reads it.
	fn brief(&mut self, id: &str) {
		let Some(e) = self.entry(id) else { return };
		self.cursor = Some(id.to_string());
		let outcome = write_handoff(&e).and_then(|path| {
			let ask = format!("Read {} and pick the work up from there.", path.display());
			new_tab(&e.root, &["claude", &ask]).map(|t| format!("briefed a new session in {t} from {}", &e.id[..8]))
		});
		match outcome {
			Ok(what) => self.say(what),
			Err(err) => self.fail(format!("{err:#}")),
		}
	}

	/// Close: end the Tern tab a running session lives in. Its transcript
	/// stays on disk, so the card turns into a plain Continue card.
	fn close(&mut self, id: &str) {
		let Some(e) = self.entry(id) else { return };
		self.cursor = Some(id.to_string());
		if e.live.is_none() {
			self.fail(format!("{} is not running", &e.id[..8]));
			return;
		}
		match find_pane(&e) {
			Ok(Some(pane)) => match tern(&["close", &pane.id.to_string()]) {
				Ok(out) if out.status.success() => {
					self.say(format!("closed tab {} ({})", pane.tab, &e.id[..8]));
					std::thread::sleep(Duration::from_millis(400));
					self.refresh();
				},
				Ok(out) => self.fail(format!("tern close: {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("failed"))),
				Err(err) => self.fail(format!("tern close: {err}")),
			},
			Ok(None) => self.fail(format!(
				"{} runs outside this window (pid {}); close it there",
				&e.id[..8],
				e.live.as_ref().map(|l| l.pid).unwrap_or_default()
			)),
			Err(err) => self.fail(format!("{err:#}")),
		}
	}

	fn hide(&mut self, id: &str, hide: bool) {
		self.step(0);
		let was = self.selected();
		if hide {
			self.hidden.insert(id.to_string());
		} else {
			self.hidden.remove(id);
		}
		if let Err(e) = save_hidden(&self.hidden) {
			self.fail(format!("hidden.json: {e:#}"));
			return;
		}
		if hide && was.as_deref() == Some(id) {
			// Keep the selection on a card that is still visible.
			let v: Vec<String> = self.visible().iter().map(|e| e.id.clone()).collect();
			self.cursor = v.first().cloned();
		}
		self.say(if hide { format!("hidden {}", &id[..8]) } else { format!("restored {}", &id[..8]) });
	}

	fn selected_hidden(&self) -> bool {
		self.selected().map(|c| self.hidden.contains(&c)).unwrap_or(false)
	}
}

fn matches(e: &Entry, q: &str) -> bool {
	q.split_whitespace().all(|w| {
		e.title.to_lowercase().contains(w)
			|| e.last_prompt.to_lowercase().contains(w)
			|| e.cwd.to_lowercase().contains(w)
			|| e.branch.to_lowercase().contains(w)
			|| e.id.starts_with(w)
			|| e.entrypoint.contains(w)
			|| e.live.as_ref().map(|l| l.name.contains(w) || l.status == w).unwrap_or(false)
	})
}

// ---------------------------------------------------------------- hidden.json

fn hidden_path() -> std::path::PathBuf {
	index::data_dir().join("hidden.json")
}

fn load_hidden() -> HashSet<String> {
	fs::read_to_string(hidden_path())
		.ok()
		.and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
		.unwrap_or_default()
		.into_iter()
		.collect()
}

fn save_hidden(set: &HashSet<String>) -> Result<()> {
	let dir = index::data_dir();
	fs::create_dir_all(&dir)?;
	let mut v: Vec<&String> = set.iter().collect();
	v.sort();
	let tmp = dir.join(format!("hidden.{}.tmp", std::process::id()));
	fs::write(&tmp, serde_json::to_vec_pretty(&v)?)?;
	fs::rename(&tmp, hidden_path()).context("replacing hidden.json")?;
	Ok(())
}

// ---------------------------------------------------------------- handoff

/// Writes `%APPDATA%\tern-claude\handoff-<id8>.md`: the session's title,
/// folder, last prompt, last reply (the recap) and transcript path. A fresh
/// session reads this (a few hundred tokens) instead of the whole context.
fn write_handoff(e: &Entry) -> Result<std::path::PathBuf> {
	let dir = index::data_dir();
	fs::create_dir_all(&dir)?;
	let path = dir.join(format!("handoff-{}.md", &e.id[..8]));
	let reply = index::last_reply(std::path::Path::new(&e.path)).unwrap_or_default();
	let reply: String = reply.chars().take(6000).collect();
	let mut md = String::new();
	md.push_str(&format!("# Handoff from Claude Code session {} — {}\n\n", &e.id[..8], e.title));
	md.push_str(&format!("- Session id: `{}`\n- Folder: `{}`\n", e.id, e.cwd));
	if !e.branch.is_empty() && e.branch != "HEAD" {
		md.push_str(&format!("- Branch: `{}`\n", e.branch));
	}
	md.push_str(&format!("- Last activity: {} · {} typed prompts · opened in {}\n", e.last, e.prompts, match e.entrypoint.as_str() {
		"claude-vscode" => "VS Code",
		"cli" => "the terminal",
		other => other,
	}));
	md.push_str(&format!("- Full transcript (read the tail only if this note is not enough): `{}`\n\n", e.path));
	md.push_str("## Last prompt from the user\n\n");
	md.push_str(if e.last_prompt.is_empty() { "(none recorded)" } else { &e.last_prompt });
	md.push_str("\n\n## Last reply (the recap)\n\n");
	md.push_str(if reply.is_empty() { "(no text reply found in the transcript tail)" } else { &reply });
	md.push_str("\n\n## What to do\n\nPick the work up from the recap above. Check the project memory (MEMORY.md) for the current state before reading the transcript. Start by saying in one line what you are about to do.\n");
	fs::write(&path, md).with_context(|| format!("writing {}", path.display()))?;
	Ok(path)
}

// ---------------------------------------------------------------- tern CLI

/// Opens `cmd` in a new tab of this window, in `cwd`. Returns a short
/// description of where it went.
fn new_tab(cwd: &str, cmd: &[&str]) -> Result<String> {
	let mut args = vec!["new", "tab", "--cwd", cwd, "--keep-open", "--json", "--"];
	args.extend_from_slice(cmd);
	let out = tern(&args).context("running tern")?;
	if !out.status.success() {
		let err = String::from_utf8_lossy(&out.stderr);
		anyhow::bail!("tern: {}", err.lines().next().unwrap_or("could not open the tab").trim());
	}
	let said = String::from_utf8_lossy(&out.stdout);
	let block = serde_json::from_str::<serde_json::Value>(&said)
		.ok()
		.and_then(|v| v.get("block").and_then(|b| b.as_u64()));
	Ok(block
		.and_then(|b| list_panes().ok().and_then(|panes| panes.into_iter().find(|p| p.id == b)))
		.map(|p| format!("tab {}", p.tab))
		.unwrap_or_else(|| "a new tab".into()))
}

/// Runs the `tern` CLI with no stdin, so a child never shares the pane's input.
fn tern(args: &[&str]) -> std::io::Result<std::process::Output> {
	std::process::Command::new("tern")
		.args(args)
		.stdin(std::process::Stdio::null())
		.output()
}

fn focus(block: u64) -> Result<()> {
	let out = tern(&["focus", &block.to_string()]).context("running tern")?;
	if !out.status.success() {
		let err = String::from_utf8_lossy(&out.stderr);
		anyhow::bail!("tern focus: {}", err.lines().next().unwrap_or("failed").trim());
	}
	Ok(())
}

/// One live pane of this window, as `tern ls --json` lists it.
struct Pane {
	id:      u64,
	tab:     u64,
	title:   String,
	program: String,
	/// The command line, for matching a session id.
	cmd:     String,
}

fn list_panes() -> Result<Vec<Pane>> {
	let out = tern(&["ls", "--json"]).context("running tern ls")?;
	if !out.status.success() {
		anyhow::bail!("tern ls failed");
	}
	let ls: LsOut = serde_json::from_slice(&out.stdout).context("reading tern ls")?;
	let mut panes = Vec::new();
	for s in &ls.sessions {
		for t in &s.tabs {
			for b in &t.blocks {
				if !b.live {
					continue;
				}
				panes.push(Pane {
					id: b.id,
					tab: t.number,
					title: b.title.clone(),
					program: b.program.clone(),
					cmd: format!("{} {}", b.command.clone().unwrap_or_default(), b.args.join(" ")),
				});
			}
		}
	}
	Ok(panes)
}

#[derive(Deserialize)]
struct LsOut {
	sessions: Vec<LsSession>,
}
#[derive(Deserialize)]
struct LsSession {
	tabs: Vec<LsTab>,
}
#[derive(Deserialize)]
struct LsTab {
	number: u64,
	blocks: Vec<LsBlock>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct LsBlock {
	id:      u64,
	title:   String,
	program: String,
	args:    Vec<String>,
	command: Option<String>,
	live:    bool,
}

/// The pane running this session, if it is in this window: a block whose
/// command names the session id, else one whose title is the session's title
/// (Claude sets the terminal title to the same text it writes as `ai-title`).
fn find_pane(e: &Entry) -> Result<Option<Pane>> {
	let panes = list_panes()?;
	if let Some(i) = panes.iter().position(|p| p.cmd.contains(&e.id)) {
		return Ok(panes.into_iter().nth(i));
	}
	Ok(panes
		.into_iter()
		.find(|p| !e.title.is_empty() && p.title == e.title && !p.program.contains("tern-claude")))
}

// ---------------------------------------------------------------- view

fn tilde(p: &str) -> String {
	match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
		Ok(h) if !h.is_empty() && p.len() >= h.len() && p[..h.len()].eq_ignore_ascii_case(&h) => format!("~{}", &p[h.len()..]),
		_ => p.to_string(),
	}
}

fn card(e: &Entry, selected: bool, hidden: bool) -> ui::Card<Msg> {
	let id = e.id.clone();
	let status = match &e.live {
		Some(l) if l.status == "busy" => Status::Running,
		Some(_) => Status::Pending,
		None => Status::Done,
	};
	let from = match e.entrypoint.as_str() {
		"claude-vscode" => "VS Code",
		"cli" => "CLI",
		"" => "?",
		other => other,
	};
	let mut meta = html::div().key("meta").class("meta").child(ui::badge(from).key("from"));
	if let Some(l) = &e.live {
		let mem = if l.mem_mb > 0 { format!(" · {} MB", l.mem_mb) } else { String::new() };
		meta = meta.child(ui::badge(format!("{} · {}{mem}", l.name, l.status)).key("live"));
		if l.kind == "background" {
			meta = meta.child(ui::badge("background").key("bg"));
		}
	}
	if hidden {
		meta = meta.child(ui::badge("hidden").key("hid"));
	}
	let mut where_ = tilde(&e.cwd);
	if !e.branch.is_empty() && e.branch != "HEAD" {
		where_ = format!("{where_}  ·  {}", e.branch);
	}
	meta = meta
		.child(html::span(local_short(&e.last)).key("when").class("w"))
		.child(html::span(where_).key("where"))
		.child(html::span(format!("{} prompt{}", e.prompts, if e.prompts == 1 { "" } else { "s" })).key("n"));

	let go_label = match &e.live {
		Some(l) if l.kind == "background" => "Attach",
		Some(_) => "Focus",
		None => "Continue",
	};
	let mut btns = html::div()
		.key("btns")
		.class("btns")
		.child(html::button(go_label).key("go").class("go").on_click(Msg::Continue(id.clone())))
		.child(html::button("Fork").key("fork").on_click(Msg::Fork(id.clone())))
		.child(html::button("Brief").key("brief").on_click(Msg::Brief(id.clone())));
	if matches!(&e.live, Some(l) if l.kind != "background") {
		btns = btns.child(html::button("Close").key("close").on_click(Msg::Close(id.clone())));
	}
	btns = if hidden {
		btns.child(html::button("Restore").key("restore").on_click(Msg::Restore(id.clone())))
	} else {
		btns.child(html::button("Hide").key("hide").on_click(Msg::Hide(id.clone())))
	};

	let mut c = ui::card()
		.key(format!("c-{}", e.id))
		.head(e.title.clone())
		.status(status)
		.selected(selected)
		.on_click(Msg::Select(id.clone()))
		.child(meta);
	if !e.last_prompt.is_empty() {
		c = c.child(ui::text(e.last_prompt.clone()).key("last").lines(2u32));
	}
	c.child(btns)
}

fn view(app: &App) -> View<Msg> {
	let visible = app.visible();
	let selected = app.selected();
	let running = visible.iter().filter(|e| e.live.is_some()).count();

	let mut top = html::div()
		.key("top")
		.class("top")
		.child(html::span("Claude sessions").key("t").class("t"))
		.child(
			html::span(format!(
				"{} shown · {} running · {} hidden · refreshed {}",
				visible.len(),
				running,
				app.hidden.len(),
				local_short(&app.idx.generated).get(6..).unwrap_or("").to_string()
			))
			.key("n")
			.class("n"),
		);
	if app.filtering || !app.filter.is_empty() {
		top = top.child(
			html::span(format!("/{}{}", app.filter, if app.filtering { "▏" } else { "" }))
				.key("f")
				.class(if app.filtering { "f on" } else { "f" }),
		);
	}
	top = top.child(
		html::div()
			.key("tb")
			.class("btns")
			.child(html::button("Refresh").key("r").on_click(Msg::Refresh))
			.child(
				html::button(if app.show_hidden { "Hide hidden" } else { "Show hidden" })
					.key("h")
					.on_click(Msg::ToggleHidden),
			)
			.child(html::button("Quit").key("q").on_click(Msg::Quit)),
	);

	let mut root = html::div().key("board").class("board").child(top);
	if !app.note.is_empty() {
		root = root.child(
			html::div()
				.key("note")
				.class(if app.error { "err" } else { "note" })
				.child(html::span(app.note.clone()).key("t")),
		);
	}

	if visible.is_empty() {
		let why = if app.filter.is_empty() { "no sessions yet" } else { "nothing matches the filter" };
		root = root.child(html::div().key("empty").class("empty").child(html::span(why).key("t")));
	}
	let mut in_running = true;
	if running > 0 {
		root = root.child(html::div().key("sec-run").class("sec").child(html::span("Running").key("t")));
	}
	for e in &visible {
		if in_running && e.live.is_none() {
			in_running = false;
			if running > 0 {
				root = root.child(html::div().key("sec-rest").class("sec").child(html::span("Earlier").key("t")));
			}
		}
		let sel = selected.as_deref() == Some(e.id.as_str());
		root = root.child(card(e, sel, app.hidden.contains(&e.id)));
	}

	let keys = if app.filtering {
		"type to filter · Enter keep · Esc clear"
	} else {
		"↑↓ j k select · Enter continue · f fork · b brief · c close tab · d hide · / filter · H hidden · r refresh · q quit"
	};
	let bar = ui::status()
		.key("bar")
		.child(ui::seg(keys).key("keys"))
		.child(
			ui::seg(format!("{} ms", app.stats.ms))
				.key("ms")
				.side(ui::Side::Right),
		);
	View::new().main([root]).dock(bar)
}

// ---------------------------------------------------------------- run

/// `tern-claude selftest [ROLE]`: a counter that re-renders twice a second
/// for 20 s, to check that frames keep flowing (acks) under a surface role.
pub fn selftest(role: &str) -> Result<()> {
	let Some(mut session) = Session::<Msg>::connect(Options::new().app("tern-claude"))? else {
		anyhow::bail!("needs Tern");
	};
	let sf = session.open(SurfaceOptions::screen().title("selftest").role(role))?;
	let start = Instant::now();
	let mut n = 0u32;
	while start.elapsed() < Duration::from_secs(20) {
		n += 1;
		let root = html::div()
			.key("root")
			.child(html::span(format!("frame {n} · {:.1}s · role {role}", start.elapsed().as_secs_f32())).key("t"));
		if let Err(e) = session.render(sf, View::new().main([root])) {
			dbg(format!("selftest render failed at {n}: {e}"));
			return Err(e.into());
		}
		match session.next(Some(Duration::from_millis(500))) {
			Ok(Some(Input::Key(k))) if k.is("q") || k.is("escape") || k.is("ctrl+c") => break,
			Ok(_) => {},
			Err(tern_sdk::Error::InputClosed) => break,
			Err(e) => return Err(e.into()),
		}
	}
	session.close()?;
	Ok(())
}

/// The interactive board inside Tern.
pub fn run() -> Result<()> {
	let Some(mut session) = Session::<Msg>::connect(Options::new().app("tern-claude"))? else {
		anyhow::bail!("the board needs Tern; use `tern-claude list` elsewhere");
	};
	let sf = session.open(SurfaceOptions::screen().title("Claude Sessions").role("sessions"))?;
	session.stylesheet(sf, "claude", Some(CSS))?;
	let mut app = App::new()?;
	dbg(format!("start: {} sessions", app.idx.sessions.len()));
	loop {
		if let Err(e) = session.render(sf, view(&app)) {
			dbg(format!("render failed: {e}"));
			return Err(e.into());
		}
		let input = match session.next(Some(Duration::from_millis(1000))) {
			Ok(Some(i)) => i,
			Ok(None) => {
				if app.refreshed.elapsed() >= AUTO_REFRESH {
					app.refresh();
					dbg("auto-refresh");
				}
				continue;
			},
			Err(tern_sdk::Error::InputClosed) => return session.close().map_err(Into::into),
			Err(e) => return Err(e.into()),
		};
		match &input {
			Input::Key(k) => dbg(format!("key: {k} text={:?}", k.text)),
			Input::Msg(m, _) => dbg(format!("msg: {m:?}")),
			_ => dbg("input: event"),
		}
		match input {
			Input::Key(key) => {
				if app.filtering {
					if key.is("escape") {
						app.filter.clear();
						app.filtering = false;
					} else if key.is("enter") {
						app.filtering = false;
					} else if key.is("backspace") {
						app.filter.pop();
					} else if let Some(t) = key.typed() {
						app.filter.push_str(t);
					}
					continue;
				}
				if key.is("ctrl+c") || key.is("ctrl+q") || key.is("q") {
					break;
				}
				if key.is("escape") {
					if app.filter.is_empty() {
						break;
					}
					app.filter.clear();
				} else if key.is("j") || key.is("down") {
					app.step(1);
				} else if key.is("k") || key.is("up") {
					app.step(-1);
				} else if key.is("g") || key.is("home") {
					app.step(isize::MIN / 2);
				} else if key.is("shift+g") || key.is("end") {
					app.step(isize::MAX / 2);
				} else if key.is("page_down") {
					app.step(8);
				} else if key.is("page_up") {
					app.step(-8);
				} else if key.is("enter") {
					if let Some(id) = app.selected() {
						app.go(&id);
					}
				} else if key.is("f") {
					if let Some(id) = app.selected() {
						app.fork(&id);
					}
				} else if key.is("b") {
					if let Some(id) = app.selected() {
						app.brief(&id);
					}
				} else if key.is("c") {
					if let Some(id) = app.selected() {
						app.close(&id);
					}
				} else if key.is("d") || key.is("x") {
					if let Some(id) = app.selected() {
						let restore = app.selected_hidden();
						app.hide(&id, !restore);
					}
				} else if key.is("/") {
					app.filtering = true;
				} else if key.is("shift+h") {
					app.show_hidden = !app.show_hidden;
				} else if key.is("r") {
					app.refresh();
					app.say(format!(
						"re-read {} sessions ({} re-read) in {} ms",
						app.stats.found, app.stats.scanned, app.stats.ms
					));
				}
			},
			Input::Msg(m, _) => match m {
				Msg::Continue(id) => app.go(&id),
				Msg::Fork(id) => app.fork(&id),
				Msg::Brief(id) => app.brief(&id),
				Msg::Close(id) => app.close(&id),
				Msg::Hide(id) => app.hide(&id, true),
				Msg::Restore(id) => app.hide(&id, false),
				Msg::Select(id) => app.cursor = Some(id),
				Msg::Refresh => app.refresh(),
				Msg::ToggleHidden => app.show_hidden = !app.show_hidden,
				Msg::Quit => break,
			},
			_ => {},
		}
	}
	session.close()?;
	Ok(())
}
