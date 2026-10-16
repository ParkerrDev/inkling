//! The history replayed: how much typing the suggestions would have saved.
//!
//! The commands are taken in the order they were run. Before each one is
//! learned, a person is imagined typing it at a prompt in the directory it
//! ran in, after the command run before it in the same session: one
//! character at a time, taking the suggestion whenever it is right, the
//! whole of it, or one word of it at a time (Alt-F) while its words are
//! right. What is counted is the keys that took, against the characters of
//! the commands, and how often the suggestion was the very command after
//! one, two and three characters. Several ways of putting the commands in
//! order are judged on the same pass, so they see the same history.
//!
//! The filesystem is not judged: whether a path was there when a command
//! ran is not recorded anywhere. The project a directory is in is taken
//! from the filesystem as it is now.
//!
//! Only counts come out of this; no command does.

use std::collections::HashMap;
use std::path::Path;

use crate::history::Past;
use crate::index::{Index, Order, Run, Weights};
use crate::paths::Look;
use crate::text;

/// How many characters are counted for "right after".
pub const AFTER: usize = 3;

/// How one way of ordering did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Score {
    /// Commands typed.
    pub commands: u64,
    /// Their characters.
    pub chars: u64,
    /// The keys it took to type them.
    pub keys: u64,
    /// Commands longer than one, two and three characters.
    pub long_enough: [u64; AFTER],
    /// Of those, how many the suggestion after that many characters was.
    pub right: [u64; AFTER],
}

impl Score {
    /// The share of the characters that did not have to be typed.
    #[must_use]
    pub fn saved(&self) -> f64 {
        let chars = u32::try_from(self.chars).map_or(f64::MAX, f64::from);
        let keys = u32::try_from(self.keys).map_or(f64::MAX, f64::from);
        if chars <= 0.0 {
            0.0
        } else {
            1.0 - keys / chars
        }
    }

    /// The share of commands the suggestion was after `n` characters.
    #[must_use]
    pub fn right_after(&self, n: usize) -> f64 {
        let (Some(&right), Some(&of)) = (
            self.right.get(n.saturating_sub(1)),
            self.long_enough.get(n.saturating_sub(1)),
        ) else {
            return 0.0;
        };
        let right = u32::try_from(right).map_or(0.0, f64::from);
        let of = u32::try_from(of).map_or(0.0, f64::from);
        if of <= 0.0 { 0.0 } else { right / of }
    }

    fn add(&mut self, one: &Typed) {
        self.commands = self.commands.saturating_add(1);
        self.chars = self.chars.saturating_add(one.chars);
        self.keys = self.keys.saturating_add(one.keys);
        for n in 0..AFTER {
            if let (Some(long), Some(right)) = (one.long_enough.get(n), one.right.get(n)) {
                if let Some(slot) = self.long_enough.get_mut(n) {
                    *slot = slot.saturating_add(u64::from(*long));
                }
                if let Some(slot) = self.right.get_mut(n) {
                    *slot = slot.saturating_add(u64::from(*right));
                }
            }
        }
    }
}

/// How the typing of one command went.
#[derive(Debug, Default)]
struct Typed {
    chars: u64,
    keys: u64,
    long_enough: [bool; AFTER],
    right: [bool; AFTER],
}

/// Where the `n`th character of `text` begins, or its end.
fn char_start(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(at, _)| at)
}

fn count_chars(text: &str) -> u64 {
    u64::try_from(text.chars().count()).unwrap_or(u64::MAX)
}

/// What taking the suggestion `offered` for the first `at` bytes of
/// `command` gets, if it gets anything: where the line then ends. The whole
/// suggestion if it is right, else its next word while that is right. Only
/// a gain of two characters or more is worth the key.
fn take(command: &str, offered: &str, at: usize) -> Option<usize> {
    let gain = |end: usize| count_chars(command.get(at..end).unwrap_or_default()) >= 2;
    if offered.len() > at && command.starts_with(offered) && gain(offered.len()) {
        return Some(offered.len());
    }
    let rest = offered.get(at..)?;
    let end = at.saturating_add(text::word_end(rest));
    let part = offered.get(..end)?;
    (end > at && command.starts_with(part) && gain(end)).then_some(end)
}

/// The keys it takes to type `command` with `ask` suggesting.
fn type_one<'a>(command: &str, ask: &dyn Fn(&str) -> Option<&'a str>) -> Typed {
    let mut typed = Typed {
        chars: count_chars(command),
        ..Typed::default()
    };
    for n in 0..AFTER {
        let end = char_start(command, n.saturating_add(1));
        if end < command.len() {
            if let Some(slot) = typed.long_enough.get_mut(n) {
                *slot = true;
            }
            if let Some(slot) = typed.right.get_mut(n) {
                *slot = ask(command.get(..end).unwrap_or_default()) == Some(command);
            }
        }
    }
    let mut at = 0;
    // Each turn takes a suggestion or types a character, so the line grows.
    for _ in 0..command.len().saturating_add(1) {
        if at >= command.len() {
            break;
        }
        typed.keys = typed.keys.saturating_add(1);
        let offered = (at > 0)
            .then(|| ask(command.get(..at).unwrap_or_default()))
            .flatten();
        if let Some(end) = offered.and_then(|offered| take(command, offered, at)) {
            at = end;
            continue;
        }
        at = command
            .get(at..)
            .and_then(|rest| rest.chars().next())
            .map_or(command.len(), |c| at.saturating_add(c.len_utf8()));
    }
    typed
}

/// What a replay found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Replayed {
    /// How each order did, in the order asked.
    pub orders: Vec<Score>,
    /// How a suggester did that knows the command whenever it was run
    /// before, and suggests nothing for one that is new: the most any order
    /// of the history can give for the commands it has seen.
    pub knowing: Score,
}

/// Replay `past`, learning every command and judging those from index
/// `from` up to `to`, with each of `orders`, ranked with `weights`.
#[must_use]
pub fn replay(
    past: &[Past],
    weights: Weights,
    range: (usize, usize),
    orders: &[Order],
    look: &mut Look,
) -> Replayed {
    let (from, to) = range;
    let mut scores = vec![Score::default(); orders.len()];
    let mut knowing = Score::default();
    let mut index = Index::with_weights(weights);
    let mut before: HashMap<&str, &str> = HashMap::new();
    for (at, one) in past.iter().enumerate().take(to) {
        let root = if one.cwd.is_empty() {
            None
        } else {
            look.project_root(Path::new(&one.cwd))
        };
        index.set_context(&one.cwd, root.as_deref());
        index.set_prev(before.get(one.session.as_str()).copied());
        let run = Run {
            command: &one.command,
            cwd: &one.cwd,
            ok: one.ok,
            session: &one.session,
        };
        let judged = at >= from && one.command.chars().count() >= 2;
        let known = index.knows(&one.command);
        if judged && index.push(&run).is_ok() {
            let command = one.command.as_str();
            let ask = |line: &str| (known && command.starts_with(line)).then_some(command);
            knowing.add(&type_one(command, &ask));
            // Pushed to find out whether it would be taken in at all; it is
            // not searchable until the commit, after it has been typed.
            for (order, score) in orders.iter().zip(scores.iter_mut()) {
                let ask = |line: &str| {
                    let best = index.ranked(line, *order);
                    best.first().map(|id| index.command(id))
                };
                score.add(&type_one(&one.command, &ask));
            }
        } else if !judged {
            let _ = index.push(&run);
        }
        index.commit();
        before.insert(&one.session, &one.command);
    }
    Replayed {
        orders: scores,
        knowing,
    }
}

/// The weights that can be tuned, by number.
fn weight_mut(weights: &mut Weights, n: usize) -> Option<&mut f64> {
    match n {
        0 => Some(&mut weights.fade),
        1 => Some(&mut weights.fade_here),
        2 => Some(&mut weights.project),
        3 => Some(&mut weights.directory),
        4 => Some(&mut weights.follows),
        5 => Some(&mut weights.failed),
        _ => None,
    }
}

/// How many weights can be tuned.
const TUNABLE: usize = 6;
/// The most rounds of tuning.
const ROUNDS: usize = 6;

/// The weights that type `past` from `from` to `to` with the fewest keys,
/// found by trying each weight halved and doubled in turn, from `start`,
/// until nothing helps. Returns them and their score.
#[must_use]
pub fn tune(
    past: &[Past],
    start: Weights,
    range: (usize, usize),
    look: &mut Look,
) -> (Weights, Score) {
    let judge = |weights: Weights, look: &mut Look| {
        replay(past, weights, range, &[Order::Likely], look)
            .orders
            .first()
            .copied()
            .unwrap_or_default()
    };
    let mut best = start;
    let mut best_score = judge(best, look);
    for _ in 0..ROUNDS {
        let mut better = false;
        for n in 0..TUNABLE {
            for factor in [0.5, 2.0] {
                let mut trial = best;
                let Some(weight) = weight_mut(&mut trial, n) else {
                    continue;
                };
                *weight *= factor;
                if trial.failed > 1.0 {
                    continue;
                }
                let score = judge(trial, look);
                if score.keys < best_score.keys {
                    (best, best_score, better) = (trial, score, true);
                }
            }
        }
        if !better {
            break;
        }
    }
    (best, best_score)
}

#[cfg(test)]
mod tests;
