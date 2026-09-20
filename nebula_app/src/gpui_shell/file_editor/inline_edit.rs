//! Editable text projected from Markdown source positions, with formatting kept
//! in the source. Unchanged leaves and link destinations remain byte-for-byte.

use markdown::mdast::Node;
use std::ops::Range;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Marks {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub link: bool,
    pub code: bool,
}

#[derive(Clone)]
struct Leaf {
    source: Range<usize>,
    visible: Range<usize>,
    text: String,
    marks: Marks,
    raw: bool,
}

pub(super) struct Projection {
    source: String,
    pub text: String,
    leaves: Vec<Leaf>,
    containers: Vec<Range<usize>>,
    pub rich: bool,
    literal: bool,
}

/// A single replacement in Unicode text; never split a CJK character or emoji.
pub(super) fn changed_span(before: &str, after: &str) -> (Range<usize>, Range<usize>) {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum::<usize>();
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum::<usize>();
    (prefix..before.len() - suffix, prefix..after.len() - suffix)
}

impl Projection {
    /// During a composition or a trailing-space edit, Markdown can temporarily
    /// be incomplete. Retain the existing leaf/mark identity until it parses to
    /// the same visible text; do not replace the native input to "repair" it.
    pub fn accept(&mut self, text: &str, source: &str) -> bool {
        if self.literal {
            *self = Self::raw(source);
            return true;
        }
        let parsed = Self::new(source);
        if parsed.text == text {
            *self = parsed;
            return true;
        }
        let (visible_before, visible_after) = changed_span(&self.text, text);
        let Some(index) = self.leaves.iter().position(|leaf| {
            leaf.visible.start <= visible_before.start && leaf.visible.end >= visible_before.end
        }) else {
            return false;
        };
        let (source_before, source_after) = changed_span(&self.source, source);
        let shift =
            |range: &mut Range<usize>, change: &Range<usize>, inserted: usize, affected: bool| {
                let delta = inserted as isize - change.len() as isize;
                if affected {
                    range.end = range.end.saturating_add_signed(delta);
                } else if range.start >= change.end {
                    range.start = range.start.saturating_add_signed(delta);
                    range.end = range.end.saturating_add_signed(delta);
                }
            };
        for (i, leaf) in self.leaves.iter_mut().enumerate() {
            shift(&mut leaf.source, &source_before, source_after.len(), i == index);
            shift(&mut leaf.visible, &visible_before, visible_after.len(), i == index);
            if i == index {
                leaf.text = text[leaf.visible.clone()].to_owned();
            }
        }
        for range in &mut self.containers {
            let affected = range.start <= source_before.start && range.end >= source_before.end;
            shift(range, &source_before, source_after.len(), affected);
        }
        self.source = source.to_owned();
        self.text = text.to_owned();
        true
    }

    pub fn new(source: &str) -> Self {
        let mut result = Self {
            source: source.to_owned(),
            text: String::new(),
            leaves: vec![],
            containers: vec![],
            rich: false,
            literal: false,
        };
        let mut options = markdown::ParseOptions::gfm();
        options.constructs.math_text = true;
        options.constructs.math_flow = true;
        if let Ok(Node::Root(root)) = markdown::to_mdast(source, &options) {
            if root.children.len() == 1 {
                match &root.children[0] {
                    Node::Heading(heading) => {
                        result.rich = true;
                        for child in &heading.children {
                            result.collect(child, Marks::default());
                        }
                    },
                    Node::Paragraph(paragraph) => {
                        result.rich = true;
                        for child in &paragraph.children {
                            result.collect(child, Marks::default());
                        }
                    },
                    _ => {},
                }
            }
        }
        if result.leaves.is_empty() {
            if result.rich {
                result.push(source.len()..source.len(), String::new(), Marks::default(), false);
                return result;
            }
            result.rich = source.is_empty();
            result.push(0..source.len(), source.to_owned(), Marks::default(), true);
        }
        result
    }

    pub fn raw(source: &str) -> Self {
        let mut result = Self {
            source: source.to_owned(),
            text: String::new(),
            leaves: vec![],
            containers: vec![],
            rich: false,
            literal: true,
        };
        result.push(0..source.len(), source.to_owned(), Marks::default(), true);
        result
    }

    fn collect(&mut self, node: &Node, mut marks: Marks) {
        let Some(position) = node.position() else { return };
        let span = position.start.offset..position.end.offset;
        match node {
            Node::Text(text) => self.push(span, text.value.clone(), marks, false),
            Node::InlineCode(code) => {
                marks.code = true;
                self.push(span, code.value.clone(), marks, false);
            },
            Node::Strong(_)
            | Node::Emphasis(_)
            | Node::Delete(_)
            | Node::Link(_)
            | Node::LinkReference(_) => {
                match node {
                    Node::Strong(_) => marks.bold = true,
                    Node::Emphasis(_) => marks.italic = true,
                    Node::Delete(_) => marks.strike = true,
                    _ => marks.link = true,
                }
                self.containers.push(span);
                for child in node.children().into_iter().flatten() {
                    self.collect(child, marks);
                }
            },
            Node::Break(_) => self.push(span, "\n".to_owned(), marks, false),
            Node::Html(html) if matches!(html.value.as_str(), "<br>" | "<br/>" | "<br />") => {
                self.push(span, "\n".to_owned(), marks, true);
            },
            _ => {
                // Embedded objects keep an explicit Markdown representation in
                // the focused block; their untouched source is never serialized.
                let text = self.source[span.clone()].to_owned();
                self.push(span, text, marks, true);
            },
        }
    }

    fn push(&mut self, source: Range<usize>, text: String, marks: Marks, raw: bool) {
        let start = self.text.len();
        self.text.push_str(&text);
        self.leaves.push(Leaf { source, visible: start..self.text.len(), text, marks, raw });
    }

    pub fn marks(&self) -> impl Iterator<Item = (Range<usize>, Marks)> + '_ {
        self.leaves.iter().map(|leaf| (leaf.visible.clone(), leaf.marks))
    }

    pub fn toggle_mark(&self, selection: Range<usize>, marker: &str) -> Option<String> {
        if !self.rich || selection.is_empty() {
            return None;
        }
        let first = self.leaves.iter().find(|leaf| leaf.visible.contains(&selection.start))?;
        let last = self
            .leaves
            .iter()
            .find(|leaf| leaf.visible.start < selection.end && leaf.visible.end >= selection.end)?;
        if first.raw || last.raw || first.marks.code || last.marks.code {
            return None;
        }
        let start = first.source.start
            + raw_offset(
                &self.source[first.source.clone()],
                &first.text,
                selection.start - first.visible.start,
            )?;
        let end = last.source.start
            + raw_offset(
                &self.source[last.source.clone()],
                &last.text,
                selection.end - last.visible.start,
            )?;
        let mut source = self.source.clone();
        if start >= marker.len()
            && source.get(start - marker.len()..start) == Some(marker)
            && source.get(end..end + marker.len()) == Some(marker)
        {
            source.replace_range(end..end + marker.len(), "");
            source.replace_range(start - marker.len()..start, "");
        } else {
            source.insert_str(end, marker);
            source.insert_str(start, marker);
        }
        (Self::new(&source).text == self.text).then_some(source)
    }

    pub fn visible_offset(&self, offset: usize) -> usize {
        let leaf = self
            .leaves
            .iter()
            .find(|leaf| leaf.source.end >= offset)
            .or_else(|| self.leaves.last())
            .unwrap();
        let local = offset.saturating_sub(leaf.source.start).min(leaf.text.len());
        leaf.visible.start + leaf.text.floor_char_boundary(local)
    }

    /// Translate a visible edit into source edits. Deleting across formatting
    /// boundaries removes empty wrappers, while surviving text keeps its marks.
    pub fn replace(&self, next: &str) -> String {
        if !self.rich {
            return next.to_owned();
        }
        if next.is_empty() {
            return String::new();
        }
        let (removed, inserted) = changed_span(&self.text, next);
        if removed.is_empty() && inserted.is_empty() {
            return self.source.clone();
        }
        let insertion = self
            .leaves
            .iter()
            .enumerate()
            .filter(|(_, leaf)| {
                leaf.visible.start <= removed.start && leaf.visible.end >= removed.start
            })
            .max_by_key(|(_, leaf)| leaf.marks != Marks::default())
            .map(|(index, _)| index)
            .unwrap_or(self.leaves.len() - 1);
        let mut edits = Vec::new();
        let mut emptied = Vec::new();
        for (index, leaf) in self.leaves.iter().enumerate() {
            let start = removed.start.max(leaf.visible.start);
            let end = removed.end.min(leaf.visible.end);
            if start >= end && index != insertion {
                continue;
            }
            let local_start = start.saturating_sub(leaf.visible.start).min(leaf.text.len());
            let local_end = end.saturating_sub(leaf.visible.start).max(local_start);
            let mut text = leaf.text.clone();
            text.replace_range(
                local_start..local_end,
                if index == insertion { &next[inserted.clone()] } else { "" },
            );
            if text.is_empty() {
                emptied.push(leaf.source.clone());
            }
            let replacement = if index == insertion { &next[inserted.clone()] } else { "" };
            let raw = &self.source[leaf.source.clone()];
            if !leaf.raw && !leaf.marks.code {
                if let (Some(start), Some(end)) = (
                    raw_offset(raw, &leaf.text, local_start),
                    raw_offset(raw, &leaf.text, local_end),
                ) {
                    edits.push((
                        leaf.source.start + start..leaf.source.start + end,
                        escape_text(replacement),
                    ));
                    continue;
                }
            }
            let encoded = if leaf.raw {
                text
            } else if leaf.marks.code {
                inline_code(&text)
            } else {
                escape_text(&text)
            };
            // When raw and decoded text agree, keep escapes and whitespace in
            // the untouched prefix/suffix, including soft line breaks.
            if !leaf.marks.code && self.source[leaf.source.clone()] == leaf.text {
                let (old, new) = changed_span(&leaf.text, &encoded);
                edits.push((
                    leaf.source.start + old.start..leaf.source.start + old.end,
                    encoded[new].to_owned(),
                ));
            } else {
                edits.push((leaf.source.clone(), encoded));
            }
        }
        for container in &self.containers {
            let children: Vec<_> = self
                .leaves
                .iter()
                .filter(|leaf| {
                    container.start <= leaf.source.start && container.end >= leaf.source.end
                })
                .collect();
            if !children.is_empty() && children.iter().all(|leaf| emptied.contains(&leaf.source)) {
                if edits
                    .iter()
                    .any(|(span, _)| span.start <= container.start && span.end >= container.end)
                {
                    continue;
                }
                edits.retain(|(span, _)| {
                    !(container.start <= span.start && container.end >= span.end)
                });
                edits.push((container.clone(), String::new()));
            }
        }
        edits.sort_by_key(|(range, _)| range.start);
        let mut source = self.source.clone();
        for (range, text) in edits.into_iter().rev() {
            source.replace_range(range, &text);
        }
        source
    }
}

fn raw_offset(raw: &str, text: &str, offset: usize) -> Option<usize> {
    if raw == text {
        return Some(offset);
    }
    let (mut source, mut visible) = (0, 0);
    while visible < offset {
        let tail = &raw[source..];
        if tail.starts_with('\\') && tail.as_bytes().get(1).is_some_and(u8::is_ascii_punctuation) {
            source += 2;
            visible += 1;
        } else if tail.starts_with("\r\n") {
            source += 2;
            visible += 1;
        } else if tail.starts_with('&') {
            let length = tail.find(';').filter(|end| *end < 40).map(|end| end + 1);
            if let Some(length) = length {
                let decoded = html_escape::decode_html_entities(&tail[..length]);
                if decoded.as_ref() != &tail[..length]
                    && text[visible..].starts_with(decoded.as_ref())
                {
                    source += length;
                    visible += decoded.len();
                    continue;
                }
            }
            source += 1;
            visible += 1;
        } else {
            let ch = tail.chars().next()?;
            if !text[visible..].starts_with(ch) {
                return None;
            }
            source += ch.len_utf8();
            visible += ch.len_utf8();
        }
    }
    (visible == offset).then_some(source)
}

fn escape_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '*' | '_' | '[' | ']' | '<' | '>' | '`' | '~' | '&' | '#' | '!') {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

fn inline_code(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let length = text.split(|ch| ch != '`').map(str::len).max().unwrap_or(0) + 1;
    let fence = "`".repeat(length);
    let padding = if text.starts_with(['`', ' ']) || text.ends_with(['`', ' ']) { " " } else { "" };
    format!("{fence}{padding}{text}{padding}{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_text_edits_preserve_delimiters_and_destinations() {
        for (source, visible, next, expected) in [
            ("## 中文 **标题** ##", "中文 标题", "中文 新标题", "## 中文 **新标题** ##"),
            ("Title\n===", "Title", "New Title", "New Title\n==="),
            (
                "a **bold** [label](url \"title\") z",
                "a bold label z",
                "a bolder label z",
                "a **bolder** [label](url \"title\") z",
            ),
            ("a &amp; b", "a & b", "a & c", "a &amp; c"),
            ("`code`", "code", "co`de", "``co`de``"),
        ] {
            let projection = Projection::new(source);
            assert_eq!(projection.text, visible);
            let actual = projection.replace(next);
            assert_eq!(actual, expected);
            assert_eq!(Projection::new(&actual).text, next);
        }
    }

    #[test]
    fn deletion_across_nested_marks_leaves_valid_surviving_text() {
        let projection = Projection::new("a **bold *inner*** and [link](dest) z");
        assert_eq!(projection.text, "a bold inner and link z");
        let source = projection.replace("a z");
        assert_eq!(source, "a z");
        assert_eq!(Projection::new(&source).text, "a z");
    }

    #[test]
    fn formatting_commands_keep_selection_text_and_unrelated_links() {
        let original = "A 中文 title and [link](destination)";
        let formatted = Projection::new(original).toggle_mark(2..8, "**").unwrap();
        assert_eq!(formatted, "A **中文** title and [link](destination)");
        assert_eq!(Projection::new(&formatted).toggle_mark(2..8, "**").unwrap(), original);
    }

    #[test]
    fn typing_spaces_does_not_duplicate_invisible_markdown_whitespace() {
        for initial in ["", "# ", "**start**"] {
            let mut projection = Projection::new(initial);
            let mut text = projection.text.clone();
            let mut source = initial.to_owned();
            for ch in " new paragraph".chars() {
                text.push(ch);
                source = projection.replace(&text);
                assert!(projection.accept(&text, &source));
                assert_eq!(projection.text, text);
            }
            assert!(!source.ends_with(' '), "{source:?}");
        }
    }

    #[test]
    fn unchanged_and_unsupported_blocks_round_trip_exactly() {
        for source in [
            "# A  #",
            "| a | b |\n| - | - |\n| 1 | 2 |",
            "$$\nx^2\n$$",
            "- a\n- b",
            "a \\*literal\\* &copy; b",
        ] {
            let projection = Projection::new(source);
            assert_eq!(projection.replace(&projection.text), source);
        }
    }

    #[test]
    fn unicode_edits_do_not_split_codepoints() {
        for (before, after) in [("中😀文", "中🌿文"), ("abc", ""), ("", "新段落"), ("same", "same")]
        {
            let (old, new) = changed_span(before, after);
            let mut source = before.to_owned();
            source.replace_range(old, &after[new]);
            assert_eq!(source, after);
        }
    }
}
