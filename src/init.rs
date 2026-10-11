//! The shell's half: what `inkling init bash` prints.
//!
//! The script lives in `init.bash` so that it can be read, and checked with a
//! shell linter, as the shell script it is.

/// The key sequence the script binds to the probe, and that the proxy sends:
/// Ctrl-X, Ctrl-underscore, `I`, BEL. It starts like the sequences Atuin
/// binds for its own use, and has no ESC in it, so it cannot be mistaken for
/// the start of a key the terminal sent.
pub const PROBE_KEYS: &[u8] = b"\x18\x1fI\x07";

/// The binding as the script writes it.
pub const PROBE_BINDING: &str = r#""\C-x\C-_I\a""#;

/// The integration for bash, as it is kept.
#[must_use]
pub fn bash() -> &'static str {
    include_str!("init.bash")
}

/// The integration for bash, for the program at `exe`: wherever the script
/// starts the proxy it names that program by its full path, so that it does
/// not depend on `PATH` being set up by the time the shell reads its startup
/// file.
#[must_use]
pub fn bash_for(exe: &str) -> String {
    bash().replace("@INKLING@", &shell_quote(exe))
}

/// `text` as one word of shell, in single quotes.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_binds_the_keys_the_proxy_sends() {
        assert!(bash().contains(PROBE_BINDING));
        assert_eq!(PROBE_KEYS, [0x18, 0x1f, b'I', 0x07]);
    }

    #[test]
    fn the_script_writes_the_marks_the_filter_reads() {
        let script = bash();
        assert!(script.contains(r"'\e]7777;%s;P;%s\a'"));
        assert!(script.contains(r"'\e]7777;%s;L;%d;%d;%q\a'"));
        assert!(script.contains(r"$'\e]7777;'$__inkling_nonce$';R\a'"));
    }

    #[test]
    fn the_program_is_named_by_its_full_path_and_quoted() {
        let script = bash_for("/home/a b/it's/inkling");
        assert!(!script.contains("@INKLING@"));
        assert!(script.contains(r"[[ -x '/home/a b/it'\''s/inkling' ]]"));
        assert!(script.contains(r"exec '/home/a b/it'\''s/inkling' run --arg0"));
    }

    #[test]
    fn the_script_has_no_dashes_that_are_not_ascii() {
        assert!(bash().is_ascii());
    }
}
