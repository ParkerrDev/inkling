//! The commands run in one place (a directory, or a project's tree), with
//! what each is worth there, searchable by prefix like the whole history.

use std::collections::HashMap;

use super::worth::{self, NONE};
use crate::text;

/// A command in a scope.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stat {
    pub runs: u32,
    /// The latest run.
    pub last: u32,
    /// Its anchor (see [`super::worth`]).
    pub anchor: f64,
}

/// The commands of one scope.
#[derive(Debug)]
pub struct Scope {
    /// The ids, in the order of their text (see [`text::key_cmp`]).
    ids: Vec<u32>,
    stats: HashMap<u32, Stat>,
    /// Ids taken in since the last [`Scope::settle`], not yet in `ids`.
    fresh: Vec<u32>,
    /// The anchor of every run here together.
    mass: f64,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            stats: HashMap::new(),
            fresh: Vec::new(),
            mass: NONE,
        }
    }
}

impl Scope {
    /// A run of command `id` here, at `seq`.
    pub fn add(&mut self, id: u32, seq: u32, ln_weight: f64, fade: f64) {
        let term = worth::term(seq, ln_weight, fade);
        let fresh = &mut self.fresh;
        let stat = self.stats.entry(id).or_insert_with(|| {
            fresh.push(id);
            Stat {
                runs: 0,
                last: seq,
                anchor: NONE,
            }
        });
        stat.runs = stat.runs.saturating_add(1);
        stat.last = stat.last.max(seq);
        stat.anchor = worth::log_add(stat.anchor, term);
        self.mass = worth::log_add(self.mass, term);
    }

    /// Put the ids taken in since the last time into their places.
    pub fn settle<'a>(&mut self, text: impl Fn(u32) -> &'a str) {
        if self.fresh.is_empty() {
            return;
        }
        let mut fresh = std::mem::take(&mut self.fresh);
        fresh.sort_by(|a, b| text::key_cmp(text(*a), text(*b)));
        let mut merged = Vec::with_capacity(self.ids.len().saturating_add(fresh.len()));
        let (mut old, mut new) = (self.ids.iter().peekable(), fresh.iter().peekable());
        // Each turn takes one id from one of the two lists.
        for _ in 0..merged.capacity() {
            let take_old = match (old.peek(), new.peek()) {
                (Some(a), Some(b)) => text::key_cmp(text(**a), text(**b)).is_le(),
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            let next = if take_old { old.next() } else { new.next() };
            if let Some(id) = next {
                merged.push(*id);
            }
        }
        self.ids = merged;
    }

    /// The ids whose text starts with `prefix`, case aside, in order.
    pub fn starting<'a>(&self, prefix: &str, text: impl Fn(u32) -> &'a str) -> &[u32] {
        let low = self
            .ids
            .partition_point(|&id| text::fold_cmp(text(id), prefix).is_lt());
        let tail = self.ids.get(low..).unwrap_or_default();
        let run = tail.partition_point(|&id| text::starts_folded(text(id), prefix));
        tail.get(..run).unwrap_or_default()
    }

    /// What is known of command `id` here.
    pub fn stat(&self, id: u32) -> Option<&Stat> {
        self.stats.get(&id)
    }

    /// The share of this scope's runs that command `id` has, the old ones
    /// fading: 0 for one never run here.
    pub fn share(&self, id: u32) -> f64 {
        self.stats
            .get(&id)
            .map_or(0.0, |stat| worth::share(stat.anchor, self.mass))
    }

    /// Take in everything of `other`, which was kept with the same fade.
    pub fn absorb(&mut self, other: &Self) {
        for (&id, theirs) in &other.stats {
            let fresh = &mut self.fresh;
            let mine = self.stats.entry(id).or_insert_with(|| {
                fresh.push(id);
                Stat {
                    runs: 0,
                    last: theirs.last,
                    anchor: NONE,
                }
            });
            mine.runs = mine.runs.saturating_add(theirs.runs);
            mine.last = mine.last.max(theirs.last);
            mine.anchor = worth::log_add(mine.anchor, theirs.anchor);
        }
        self.mass = worth::log_add(self.mass, other.mass);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXTS: &[&str] = &["git add", "git status", "ls", "make", "Git log"];

    fn text(id: u32) -> &'static str {
        TEXTS[usize::try_from(id).unwrap()]
    }

    #[test]
    fn commands_are_found_by_prefix_once_settled() {
        let mut scope = Scope::default();
        for (id, seq) in [(3, 1), (1, 2), (0, 3), (1, 4)] {
            scope.add(id, seq, 0.0, 10.0);
        }
        assert!(
            scope.starting("git", text).is_empty(),
            "not before settling"
        );
        scope.settle(text);
        assert_eq!(scope.starting("git", text), &[0, 1]);
        assert_eq!(scope.starting("m", text), &[3]);
        assert_eq!(scope.starting("x", text), &[] as &[u32]);
        assert_eq!(scope.stat(1).map(|s| (s.runs, s.last)), Some((2, 4)));
        assert_eq!(scope.starting("", text).len(), 3);
    }

    #[test]
    fn a_prefix_finds_its_commands_in_any_case() {
        let mut scope = Scope::default();
        for (id, seq) in [(1, 1), (4, 2), (0, 3), (3, 4)] {
            scope.add(id, seq, 0.0, 10.0);
        }
        scope.settle(text);
        assert_eq!(scope.starting("git", text), &[0, 4, 1]);
        assert_eq!(scope.starting("GIT L", text), &[4]);
        assert_eq!(scope.starting("M", text), &[3]);
    }

    #[test]
    fn shares_add_up_and_the_more_run_has_more() {
        let mut scope = Scope::default();
        scope.add(0, 1, 0.0, 10.0);
        scope.add(1, 2, 0.0, 10.0);
        scope.add(1, 3, 0.0, 10.0);
        scope.settle(text);
        assert!(scope.share(1) > scope.share(0));
        assert!((scope.share(0) + scope.share(1) - 1.0).abs() < 1e-9);
        assert!(scope.share(2).abs() < f64::EPSILON);
    }

    #[test]
    fn two_scopes_absorbed_are_one() {
        let (mut a, mut b, mut both) = (Scope::default(), Scope::default(), Scope::default());
        a.add(0, 1, 0.0, 10.0);
        b.add(1, 2, 0.0, 10.0);
        b.add(0, 3, 0.0, 10.0);
        for (id, seq) in [(0, 1), (1, 2), (0, 3)] {
            both.add(id, seq, 0.0, 10.0);
        }
        let mut merged = Scope::default();
        merged.absorb(&a);
        merged.absorb(&b);
        merged.settle(text);
        both.settle(text);
        assert!((merged.share(0) - both.share(0)).abs() < 1e-9);
        assert_eq!(merged.starting("", text), both.starting("", text));
        assert_eq!(merged.stat(0).map(|s| s.runs), Some(2));
    }
}
