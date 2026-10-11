//! A made-up home for a made-up person: their history, their shell setup.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

use super::terminal::{COLS, Launch, ROWS, Terminal};

pub struct Sandbox {
    pub dir: TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("home/.config")).unwrap();
        Self { dir }
    }

    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn histfile(&self) -> PathBuf {
        self.home().join(".bash_history")
    }

    pub fn write_history(&self, lines: &[&str]) {
        let mut text = lines.join("\n");
        text.push('\n');
        std::fs::write(self.histfile(), text).unwrap();
    }

    /// The shell setup with `before` ahead of inkling's line and `after`
    /// behind it.
    pub fn rc_around(&self, before: &str, after: &str) -> PathBuf {
        let rc = self.home().join("rc.sh");
        let body = format!(
            "PS1='$ '\nunset PROMPT_COMMAND\nHISTFILE={}\nHISTCONTROL=ignoreboth\n{before}\neval \"$(inkling init bash)\"\n{after}\n",
            self.histfile().display()
        );
        std::fs::write(&rc, body).unwrap();
        rc
    }

    /// The environment a person's shell would start in, in the sandbox.
    pub fn env(&self) -> Vec<(String, String)> {
        let bin = Path::new(env!("CARGO_BIN_EXE_inkling")).parent().unwrap();
        vec![
            ("HOME".into(), self.home().display().to_string()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "PATH".into(),
                format!("{}:/usr/local/bin:/usr/bin:/bin", bin.display()),
            ),
            (
                "XDG_DATA_HOME".into(),
                self.home().join(".local/share").display().to_string(),
            ),
        ]
    }

    pub fn bash_argv(rc: &Path) -> Vec<String> {
        [
            "bash",
            "--noprofile",
            "--rcfile",
            &rc.display().to_string(),
            "-i",
        ]
        .map(String::from)
        .to_vec()
    }

    /// An interactive bash with inkling, at its first prompt.
    pub fn shell(&self, extra_rc: &str) -> Terminal {
        self.shell_with(extra_rc, &[])
    }

    pub fn shell_with(&self, extra_rc: &str, env: &[(&str, &str)]) -> Terminal {
        self.shell_around(extra_rc, "", env)
    }

    /// As [`Sandbox::shell_with`], with setup before and after inkling's line.
    pub fn shell_around(&self, before: &str, after: &str, env: &[(&str, &str)]) -> Terminal {
        let rc = self.rc_around(before, after);
        let argv = Self::bash_argv(&rc);
        let mut vars = self.env();
        vars.extend(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
        let mut terminal = Terminal::start(&Launch {
            argv: &argv,
            env: vars,
            dir: &self.home(),
            size: (ROWS, COLS),
        });
        let prompt = terminal.wait_for(std::time::Duration::from_secs(10), |screen| {
            screen.row(screen.cursor().1) == "$"
        });
        assert!(prompt, "no prompt:\n{}", terminal.screen.text());
        terminal.settle(std::time::Duration::from_millis(50));
        terminal
    }

    /// Atuin's settings for a person with no server: nothing leaves the
    /// machine.
    pub fn atuin_config(&self) {
        let dir = self.home().join(".config/atuin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            "update_check = false\nauto_sync = false\nsync_address = \"http://127.0.0.1:9\"\nstyle = \"compact\"\n",
        )
        .unwrap();
    }

    /// An Atuin database as Atuin makes one, with these commands, oldest
    /// first, each run in the directory given.
    pub fn atuin_db(&self, commands: &[(&str, &str)]) -> PathBuf {
        let path = self.home().join(".local/share/atuin/history.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "wal").unwrap();
        conn.execute_batch(
            "CREATE TABLE history (
                id text primary key, timestamp integer not null, duration integer not null,
                exit integer not null, command text not null, cwd text not null,
                session text not null, hostname text not null, deleted_at integer,
                author text, intent text, shell text, author_kind text);
             CREATE INDEX idx_history_timestamp ON history(timestamp);",
        )
        .unwrap();
        for (n, (command, cwd)) in commands.iter().enumerate() {
            conn.execute(
                "INSERT INTO history (id, timestamp, duration, exit, command, cwd, session, hostname)
                 VALUES (?1, ?2, 1, 0, ?3, ?4, 's', 'h')",
                rusqlite::params![format!("id{n}"), i64::try_from(n).unwrap().saturating_add(1), command, cwd],
            )
            .unwrap();
        }
        path
    }
}
