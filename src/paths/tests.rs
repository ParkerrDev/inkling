use super::*;

/// A scratch tree: a project with a repository, some files and folders, and
/// a folder of programs.
struct Tree {
    dir: tempfile::TempDir,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for folder in [
            "proj/.git",
            "proj/src/deep",
            "proj/build",
            "home/Documents",
            "home/Downloads",
            "home/Dev/GitHub",
            "home/Dev/gitea",
            "bin",
        ] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        for file in [
            "proj/src/main.rs",
            "proj/notes with space.txt",
            "home/.hidden",
            "home/readme.md",
        ] {
            std::fs::write(root.join(file), b"").unwrap();
        }
        for program in ["cargo", "cargo-deny", "carbonyl", "ls", "VBoxManage"] {
            std::fs::write(root.join("bin").join(program), b"").unwrap();
        }
        Self { dir }
    }

    fn path(&self, rest: &str) -> PathBuf {
        self.dir.path().join(rest)
    }

    /// A look from `cwd` (a path in the tree).
    fn look(&self, cwd: &str) -> Look {
        let mut look = Look::new(Some(self.path("home")), vec![self.path("bin")]);
        look.prompt(&self.path(cwd));
        look
    }
}

fn texts(command: &str) -> Vec<String> {
    words(command).into_iter().map(|w| w.text).collect()
}

#[test]
fn words_are_split_as_the_shell_splits_them() {
    assert_eq!(texts("ls -la src"), ["ls", "-la", "src"]);
    assert_eq!(
        texts("vim 'a b' \"c d\" e\\ f"),
        ["vim", "a b", "c d", "e f"]
    );
    assert_eq!(
        texts("make&&ls|wc >out"),
        ["make", "&&", "ls", "|", "wc", ">", "out"]
    );
    let plain: Vec<bool> = words("echo $HOME *.rs \"$x\" ok")
        .iter()
        .map(|w| w.plain)
        .collect();
    assert_eq!(plain, [true, false, false, false, true]);
}

#[test]
fn a_projects_root_is_the_nearest_repository() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    let root = tree.path("proj").to_str().unwrap().to_owned();
    assert_eq!(
        look.project_root(&tree.path("proj/src/deep")),
        Some(root.clone())
    );
    assert_eq!(look.project_root(&tree.path("proj")), Some(root));
    assert_eq!(look.project_root(&tree.path("home")), None);
}

#[test]
fn a_command_is_usable_when_its_paths_are_there() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    assert!(look.usable("vim ./src/main.rs"));
    assert!(look.usable("cd src/deep"));
    assert!(look.usable("cd build && make"));
    assert!(!look.usable("vim ./src/gone.rs"));
    assert!(!look.usable("cd nowhere"));
    assert!(!look.usable("cd src/main.rs"), "cd wants a directory");
    assert!(!look.usable("make && cd ../missing"));
    assert!(
        !look.usable("sudo cd nowhere"),
        "a prefix keeps the command's place"
    );
}

#[test]
fn what_is_not_shaped_like_a_path_is_not_judged() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    assert!(look.usable("git checkout feature/login"));
    assert!(look.usable("docker run library/ubuntu"));
    assert!(look.usable("cat $HOME/anything ./*.missing"));
    assert!(
        look.usable("make > ./new/file"),
        "where output goes need not exist"
    );
    assert!(look.usable("cd -"));
    assert!(look.usable("ls ~/Documents"));
    assert!(!look.usable("ls ~/Nothing"));
    assert!(
        look.usable("ls ~someone/x"),
        "another user's home is not looked into"
    );
}

#[test]
fn a_word_in_a_commands_place_is_finished_as_a_program() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    let unknown = |_: &str| 0.0;
    // Nobody has used any: what they all start with.
    assert_eq!(
        look.finish("car", &unknown),
        None,
        "they share no more than that"
    );
    assert_eq!(look.finish("carg", &unknown), Some("cargo".to_owned()));
    let used = |name: &str| if name == "carbonyl" { 0.5 } else { 0.0 };
    assert_eq!(look.finish("car", &used), Some("carbonyl".to_owned()));
    assert_eq!(
        look.finish("make && carb", &unknown),
        Some("make && carbonyl".to_owned())
    );
    assert_eq!(
        look.finish("sudo carb", &unknown),
        Some("sudo carbonyl".to_owned())
    );
    assert_eq!(look.finish("zzz", &unknown), None);
}

#[test]
fn a_later_word_is_finished_as_a_path() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    let unknown = |_: &str| 0.0;
    assert_eq!(
        look.finish("vim src/ma", &unknown),
        Some("vim src/main.rs".to_owned())
    );
    assert_eq!(look.finish("ls sr", &unknown), Some("ls src/".to_owned()));
    assert_eq!(
        look.finish("cat notes", &unknown),
        Some("cat notes\\ with\\ space.txt".to_owned())
    );
    assert_eq!(
        look.finish("cd src/main", &unknown),
        None,
        "cd takes directories only"
    );
    assert_eq!(
        look.finish("ls ", &unknown),
        None,
        "nothing typed of the word"
    );
    let mut home = tree.look("home");
    assert_eq!(
        home.finish("ls .h", &unknown),
        Some("ls .hidden".to_owned())
    );
    assert_eq!(
        home.finish("ls r", &unknown),
        Some("ls readme.md".to_owned())
    );
    // Documents and Downloads share only "Do": the newest of the two.
    assert!(home.finish("cd Do", &unknown).is_some());
    assert_eq!(
        home.finish("ls ~/Doc", &unknown),
        Some("ls ~/Documents/".to_owned())
    );
}

#[test]
fn a_word_in_lower_case_is_finished_in_the_case_of_the_name() {
    let tree = Tree::new();
    let mut home = tree.look("home");
    let unknown = |_: &str| 0.0;
    assert_eq!(
        home.finish("ls docu", &unknown),
        Some("ls Documents/".to_owned())
    );
    // An exact fit comes first: readme.md, not a capital R anything.
    assert_eq!(
        home.finish("ls r", &unknown),
        Some("ls readme.md".to_owned())
    );
    assert_eq!(
        home.finish("ls Docu", &unknown),
        Some("ls Documents/".to_owned())
    );
    assert_eq!(home.finish("ls DOCU", &unknown), None, "a capital is meant");
    assert_eq!(
        home.finish("vboxm", &unknown),
        Some("VBoxManage".to_owned())
    );
}

#[test]
fn directories_typed_in_lower_case_are_put_in_their_own() {
    let tree = Tree::new();
    let mut home = tree.look("home");
    let unknown = |_: &str| 0.0;
    assert_eq!(
        home.finish("cd ~/dev/gith", &unknown),
        Some("cd ~/Dev/GitHub/".to_owned())
    );
    assert_eq!(
        home.finish("cd dev/gite", &unknown),
        Some("cd Dev/gitea/".to_owned())
    );
    // GitHub and gitea are both "git" but for case: an exact fit first.
    assert_eq!(
        home.finish("cd dev/git", &unknown),
        Some("cd Dev/gitea/".to_owned())
    );
    let mut look = tree.look("proj");
    assert_eq!(
        look.finish("vim SRC/ma", &unknown),
        None,
        "capitals typed that the name does not have"
    );
}

#[test]
fn lines_too_involved_to_finish_are_left() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    let unknown = |_: &str| 0.0;
    for line in ["vim 'src/ma", "vim \"src", "echo $HO", "vim src\\ ", ""] {
        assert_eq!(look.finish(line, &unknown), None, "{line:?}");
    }
}

#[test]
fn names_are_escaped_as_they_are_typed() {
    assert_eq!(
        escaped("plain-name_1.rs").as_deref(),
        Some("plain-name_1.rs")
    );
    assert_eq!(escaped("a b&c").as_deref(), Some("a\\ b\\&c"));
    assert_eq!(escaped("caf\u{e9}").as_deref(), Some("caf\u{e9}"));
    assert_eq!(escaped("bad\nname"), None);
}

#[test]
fn what_is_seen_is_seen_again_at_the_next_prompt() {
    let tree = Tree::new();
    let mut look = tree.look("proj");
    assert!(!look.usable("vim ./later.rs"));
    std::fs::write(tree.path("proj/later.rs"), b"").unwrap();
    assert!(
        !look.usable("vim ./later.rs"),
        "remembered until the prompt"
    );
    look.prompt(&tree.path("proj"));
    assert!(look.usable("vim ./later.rs"));
}
