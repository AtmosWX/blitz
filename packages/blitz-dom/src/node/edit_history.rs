//! Undo and redo for text inputs. Each step keeps only the text an edit replaced and what
//! replaced it, and runs of typing (or of deleting) merge into one step, as native text fields
//! do: a run ends when the caret moves, the selection changes, or a different kind of edit comes.

use parley::Selection;

/// What an edit did, for merging runs of edits into one undo step.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditKind {
    /// Typed text: characters, newlines, IME commits.
    Typing,
    /// Deleting backwards from the caret (Backspace, including by word or to the line's start).
    DeleteBackward,
    /// Deleting forwards from the caret.
    DeleteForward,
    /// Anything else (paste, cut): a step of its own.
    Other,
}

/// A text input's text and selection, from before an edit.
pub(crate) struct Snapshot {
    pub(crate) text: String,
    pub(crate) selection: Selection,
}

/// One undo step: the bytes from `start` were `removed` before it and are `inserted` after it.
struct Edit {
    start: usize,
    removed: String,
    inserted: String,
    selection_before: Selection,
    selection_after: Selection,
}

impl Edit {
    /// The step from `before` to `after`: the span between their common start and end.
    fn between(
        before: &str,
        after: &str,
        selection_before: Selection,
        selection_after: Selection,
    ) -> Self {
        let mut prefix = before
            .bytes()
            .zip(after.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(prefix) || !after.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let longest = before.len().min(after.len()) - prefix;
        let mut suffix = before
            .bytes()
            .rev()
            .zip(after.bytes().rev())
            .take(longest)
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(before.len() - suffix)
            || !after.is_char_boundary(after.len() - suffix)
        {
            suffix -= 1;
        }
        Self {
            start: prefix,
            removed: before[prefix..before.len() - suffix].to_owned(),
            inserted: after[prefix..after.len() - suffix].to_owned(),
            selection_before,
            selection_after,
        }
    }

    /// `text` (as this step left it) with the step undone, or `None` if it isn't that text.
    fn revert(&self, text: &str) -> Option<String> {
        replace(text, self.start, &self.inserted, &self.removed)
    }

    /// `text` (as it was before this step) with the step done again, or `None` if it isn't.
    fn apply(&self, text: &str) -> Option<String> {
        replace(text, self.start, &self.removed, &self.inserted)
    }
}

/// `text` with `old` at `start` replaced by `new`, if `old` is there.
fn replace(text: &str, start: usize, old: &str, new: &str) -> Option<String> {
    let end = start + old.len();
    if text.get(start..end) != Some(old) {
        return None;
    }
    Some([&text[..start], new, &text[end..]].concat())
}

fn same_selection(a: &Selection, b: &Selection) -> bool {
    a.anchor().index() == b.anchor().index() && a.focus().index() == b.focus().index()
}

#[derive(Default)]
pub(crate) struct EditHistory {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// The kind of edit the last undo step still takes in, while its run lasts.
    open: Option<EditKind>,
}

impl EditHistory {
    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Ends the current run of typing or deleting: the next edit starts a step of its own.
    pub(crate) fn close(&mut self) {
        self.open = None;
    }

    /// Records an edit of `kind` from `before` to `text` and `selection`.
    pub(crate) fn record(
        &mut self,
        kind: EditKind,
        before: Snapshot,
        text: &str,
        selection: Selection,
    ) {
        if before.text == text {
            return;
        }
        self.redo.clear();
        // Merging needs the run's step to be the last thing that happened to the text.
        let continues = |last: &Edit| {
            same_selection(&last.selection_after, &before.selection)
                && before
                    .text
                    .get(last.start..last.start + last.inserted.len())
                    == Some(last.inserted.as_str())
        };
        let merged = match self.undo.last() {
            Some(last) if kind != EditKind::Other && self.open == Some(kind) && continues(last) => {
                last.revert(&before.text).map(|original| {
                    Edit::between(&original, text, last.selection_before, selection)
                })
            }
            _ => None,
        };
        match merged {
            Some(edit) => *self.undo.last_mut().expect("merged into the last step") = edit,
            None => self.undo.push(Edit::between(
                &before.text,
                text,
                before.selection,
                selection,
            )),
        }
        self.open = (kind != EditKind::Other).then_some(kind);
    }

    /// Undoes the last step of `text`: the text and selection to restore, if there's a step.
    pub(crate) fn undo(&mut self, text: &str) -> Option<(String, Selection)> {
        self.open = None;
        let edit = self.undo.pop()?;
        let Some(restored) = edit.revert(text) else {
            // The text changed without the history knowing; its steps no longer fit.
            self.clear();
            return None;
        };
        let selection = edit.selection_before;
        self.redo.push(edit);
        Some((restored, selection))
    }

    /// Redoes the last undone step of `text`: the text and selection to restore.
    pub(crate) fn redo(&mut self, text: &str) -> Option<(String, Selection)> {
        self.open = None;
        let edit = self.redo.pop()?;
        let Some(restored) = edit.apply(text) else {
            self.clear();
            return None;
        };
        let selection = edit.selection_after;
        self.undo.push(edit);
        Some((restored, selection))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records `before` → `after`. Selections are left at their default: without a layout,
    /// parley's cursors all sit at the start.
    fn edit(history: &mut EditHistory, kind: EditKind, before: &str, after: &str) {
        let before = Snapshot {
            text: before.into(),
            selection: Selection::default(),
        };
        history.record(kind, before, after, Selection::default());
    }

    fn undo(history: &mut EditHistory, text: &str) -> Option<String> {
        history.undo(text).map(|(text, _)| text)
    }

    #[test]
    fn typing_runs_undo_together() {
        let mut history = EditHistory::default();
        edit(&mut history, EditKind::Typing, "", "a");
        edit(&mut history, EditKind::Typing, "a", "ab");
        edit(&mut history, EditKind::Typing, "ab", "abc");
        assert_eq!(undo(&mut history, "abc").as_deref(), Some(""));
        assert!(!history.can_undo());
        assert_eq!(
            history.redo("").map(|(text, _)| text).as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn kinds_and_closing_split_steps() {
        let mut history = EditHistory::default();
        edit(&mut history, EditKind::Typing, "", "ab");
        edit(&mut history, EditKind::DeleteBackward, "ab", "a");
        history.close();
        edit(&mut history, EditKind::DeleteBackward, "a", "");
        assert_eq!(undo(&mut history, "").as_deref(), Some("a"));
        assert_eq!(undo(&mut history, "a").as_deref(), Some("ab"));
        assert_eq!(undo(&mut history, "ab").as_deref(), Some(""));
        assert!(undo(&mut history, "").is_none());
    }

    #[test]
    fn other_edits_are_steps_of_their_own() {
        let mut history = EditHistory::default();
        edit(&mut history, EditKind::Other, "", "x");
        edit(&mut history, EditKind::Other, "x", "xy");
        assert_eq!(undo(&mut history, "xy").as_deref(), Some("x"));
    }

    #[test]
    fn multibyte_edits_split_on_char_boundaries() {
        // "é" and "è" share their first byte.
        let edit = Edit::between("é", "è", Selection::default(), Selection::default());
        assert_eq!((edit.start, &*edit.removed, &*edit.inserted), (0, "é", "è"));
    }

    #[test]
    fn a_new_edit_drops_redo() {
        let mut history = EditHistory::default();
        edit(&mut history, EditKind::Other, "", "x");
        undo(&mut history, "x");
        edit(&mut history, EditKind::Other, "", "y");
        assert!(!history.can_redo());
    }

    #[test]
    fn foreign_text_clears() {
        let mut history = EditHistory::default();
        edit(&mut history, EditKind::Typing, "", "a");
        assert!(undo(&mut history, "zzz").is_none());
        assert!(!history.can_undo());
    }
}
