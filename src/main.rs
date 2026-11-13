//! `tern-claude`: Claude Code sessions as native Tern cards.
//!
//! - `tern-claude`            the board, inside a Tern pane
//! - `tern-claude index`      rebuild `%APPDATA%\tern-claude\index.json` (hooks call this)
//! - `tern-claude list`       the index as a table, for any shell

mod board;
mod index;

fn main() -> anyhow::Result<()> {
	let args: Vec<String> = std::env::args().skip(1).collect();
	let args: Vec<&str> = args.iter().map(String::as_str).collect();
	match args.as_slice() {
		[] => board::run(),
		["index", rest @ ..] => index::cli_index(rest),
		["list", rest @ ..] => index::cli_list(rest),
		["selftest"] => board::selftest("sessions"),
		["selftest", role] => board::selftest(role),
		_ => {
			eprintln!("usage: tern-claude [index [--why NAME] [--quiet] | list | selftest [ROLE]]");
			std::process::exit(2);
		},
	}
}
