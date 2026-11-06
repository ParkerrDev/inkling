//! How the commands that fit what was typed are put in order.
//!
//! Each command gets the chance that it is the one meant, as a mixture of
//! four estimates, each a share that sums to one over the commands it knows:
//!
//! * its share of all the runs in the history, the old ones fading;
//! * its share of the runs in the project the shell is in (the tree of the
//!   nearest directory with a `.git`), fading the same way;
//! * its share of the runs in the very directory the shell is in;
//! * how often it was the next command after the one just run, in the same
//!   session.
//!
//! A command that fits what was typed only because letters typed in lower
//! case are capitals in it (see [`crate::text`]) has its chance multiplied
//! by [`Weights::case`]: someone who did not reach for Shift is likelier to
//! have meant a command that has the letters as typed.
//!
//! A failed run counts for less than one that went well. A scope the shell
//! has never run anything in adds nothing, so the mixture falls back to what
//! is known. Because each share is taken within its own scope, ten commands
//! run in one directory weigh there as much as ten thousand do in the whole
//! history, which is what makes the directory matter.

/// What decides the order. The values are those that typed the most of the
/// owner's own history for them when it was replayed (`inkling replay`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weights {
    /// After how many commands, run anywhere, a run in the whole history has
    /// faded to `1/e` of its weight.
    pub fade: f64,
    /// The same, in a directory or a project.
    pub fade_here: f64,
    /// The weight of the whole history's estimate.
    pub everywhere: f64,
    /// The weight of the project's.
    pub project: f64,
    /// The weight of the directory's.
    pub directory: f64,
    /// The weight of what usually comes next.
    pub follows: f64,
    /// What a failed run counts for, against 1 for one that went well.
    pub failed: f64,
    /// What a command counts for that fits only with letters typed in
    /// lower case taken for capitals, against 1 for one that fits exactly.
    pub case: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            fade: 400.0,
            fade_here: 200.0,
            everywhere: 1.0,
            project: 1.0,
            directory: 2.0,
            follows: 1.0,
            failed: 0.3,
            case: 0.25,
        }
    }
}

/// How many of the best are kept.
pub const BEST: usize = 8;

/// The best few commands, best first, each once.
#[derive(Clone, Copy, Debug)]
pub struct Best {
    items: [(u32, f64); BEST],
    len: usize,
}

impl Default for Best {
    fn default() -> Self {
        Self {
            items: [(0, 0.0); BEST],
            len: 0,
        }
    }
}

impl Best {
    /// Consider command `id` with `score`.
    pub fn offer(&mut self, id: u32, score: f64) {
        let kept = self.items.get(..self.len).unwrap_or_default();
        if kept.iter().any(|&(seen, _)| seen == id) {
            return;
        }
        // Where it goes: after every score at least as good.
        let at = kept.partition_point(|&(_, s)| s >= score);
        if at >= BEST {
            return;
        }
        let end = self.len.min(BEST.saturating_sub(1));
        if let Some(tail) = self.items.get_mut(at..=end) {
            tail.rotate_right(1);
        }
        if let Some(slot) = self.items.get_mut(at) {
            *slot = (id, score);
        }
        self.len = self.len.saturating_add(1).min(BEST);
    }

    /// Whether a command with `score` would be kept if offered.
    #[must_use]
    pub fn would_take(&self, score: f64) -> bool {
        self.len < BEST
            || self
                .items
                .get(BEST.saturating_sub(1))
                .is_some_and(|&(_, last)| last < score)
    }

    /// The ids, best first.
    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.items
            .get(..self.len)
            .unwrap_or_default()
            .iter()
            .map(|&(id, _)| id)
    }

    /// The best, if there is one.
    #[must_use]
    pub fn first(&self) -> Option<u32> {
        self.ids().next()
    }

    /// How many are kept.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether none are.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_best_are_kept_in_order_and_each_once() {
        let mut best = Best::default();
        for (id, score) in [(1, 0.5), (2, 0.9), (3, 0.1), (2, 0.95), (4, 0.7)] {
            best.offer(id, score);
        }
        assert_eq!(best.ids().collect::<Vec<_>>(), vec![2, 4, 1, 3]);
        assert_eq!(best.first(), Some(2));
    }

    #[test]
    fn only_so_many_are_kept() {
        let mut best = Best::default();
        for id in 0..20_u32 {
            best.offer(id, f64::from(id));
        }
        assert_eq!(best.len(), BEST);
        assert_eq!(best.first(), Some(19));
        assert_eq!(best.ids().last(), Some(12));
        assert!(!best.would_take(-1.0) && !best.would_take(12.0));
        assert!(best.would_take(12.5));
        best.offer(99, -1.0);
        assert!(!best.ids().any(|id| id == 99), "worse than all kept");
    }
}
