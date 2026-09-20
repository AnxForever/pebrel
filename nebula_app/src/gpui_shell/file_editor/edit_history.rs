//! One document history across source and formatted inputs. Each input is a
//! presentation: its lifetime must not decide which edits can be undone.

use super::inline_edit::changed_span;
use std::{
    ops::Range,
    time::{Duration, Instant},
};

const MAX_HISTORY_BYTES: usize = 8 * 1024 * 1024;

struct Change {
    start: usize,
    before: String,
    after: String,
}

impl Change {
    fn bytes(&self) -> usize {
        self.before.len() + self.after.len()
    }
}

#[derive(Default)]
pub(super) struct EditHistory {
    current: String,
    undo: Vec<Vec<Change>>,
    redo: Vec<Vec<Change>>,
    bytes: usize,
    last_edit: Option<Instant>,
}

impl EditHistory {
    pub fn reset(&mut self, text: &str) {
        *self = Self { current: text.to_owned(), ..Self::default() };
    }

    pub fn barrier(&mut self) {
        self.last_edit = None;
    }

    pub fn record(&mut self, text: &str) {
        if text == self.current {
            return;
        }
        let (before, after) = changed_span(&self.current, text);
        let change = Change {
            start: before.start,
            before: self.current[before].to_owned(),
            after: text[after].to_owned(),
        };
        self.current = text.to_owned();
        self.bytes -= self.redo.iter().flatten().map(Change::bytes).sum::<usize>();
        self.redo.clear();
        let now = Instant::now();
        let join = self
            .last_edit
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(600))
            && self.undo.last().and_then(|group| group.last()).is_some_and(|last| {
                change.start <= last.start + last.after.len()
                    && change.start + change.before.len() >= last.start
            });
        self.bytes += change.bytes();
        if join {
            self.undo.last_mut().unwrap().push(change);
        } else {
            self.undo.push(vec![change]);
        }
        while self.bytes > MAX_HISTORY_BYTES && !self.undo.is_empty() {
            self.bytes -= self.undo.remove(0).iter().map(Change::bytes).sum::<usize>();
        }
        self.last_edit = Some(now);
    }

    pub fn travel(&mut self, redo: bool) -> Option<(String, Range<usize>)> {
        self.barrier();
        let changes = if redo { self.redo.pop()? } else { self.undo.pop()? };
        let mut cursor = 0;
        if redo {
            for change in &changes {
                self.current
                    .replace_range(change.start..change.start + change.before.len(), &change.after);
                cursor = change.start + change.after.len();
            }
            self.undo.push(changes);
        } else {
            for change in changes.iter().rev() {
                self.current
                    .replace_range(change.start..change.start + change.after.len(), &change.before);
                cursor = change.start + change.before.len();
            }
            self.redo.push(changes);
        }
        Some((self.current.clone(), cursor..cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_spans_input_lifetimes_and_discards_redo_after_a_new_edit() {
        let mut history = EditHistory::default();
        history.reset("# 中文\n\nbody");
        history.record("# 中文😀\n\nbody");
        history.barrier();
        history.record("# 中文😀\n\nchanged");
        assert_eq!(history.travel(false).unwrap().0, "# 中文😀\n\nbody");
        assert_eq!(history.travel(false).unwrap().0, "# 中文\n\nbody");
        assert_eq!(history.travel(true).unwrap().0, "# 中文😀\n\nbody");
        history.record("# 中文🌿\n\nbody");
        assert!(history.travel(true).is_none());
        assert_eq!(history.travel(false).unwrap().0, "# 中文😀\n\nbody");
    }
}
