//! Document commands bridge formatted inputs and the canonical source buffer.

use super::inline_edit::changed_span;
use super::*;

impl TextFileView {
    pub(super) fn format_live_selection(
        &mut self,
        marker: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(edit) = &self.live_edit else { return };
        let selection = edit.input.read(cx).selected_range();
        let Some(formatted) = edit.projection.toggle_mark(selection.clone(), marker) else {
            return;
        };
        let block = edit.block;
        let cursor = edit.range.start;
        let mut source = self.input.read(cx).value().to_string();
        self.history.record(&source);
        source.replace_range(edit.range.clone(), &formatted);
        self.finish_live_edit(cx);
        self.replace_document_text(&source, window, cx);
        self.history.record(&source);
        self.apply_outline(Outline::parse(&source), cx);
        self.resume_live_at(block, cursor, window, cx);
        if let Some(edit) = &self.live_edit {
            edit.input.update(cx, |input, cx| input.set_selected_range(selection, cx));
        }
    }

    pub(super) fn travel_history(
        &mut self,
        redo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading || self.document.as_ref().is_none_or(|doc| doc.read_only) {
            return;
        }
        self.history.record(&self.input.read(cx).value());
        let Some((text, selection)) = self.history.travel(redo) else { return };
        let editing = self.live_edit.is_some();
        self.finish_live_edit(cx);
        self.replace_document_text(&text, window, cx);
        self.input.update(cx, |input, cx| input.set_selected_range(selection.clone(), cx));
        let mut outline = Outline::parse(&text);
        let block = editing.then(|| outline.edit_block_at(selection.start));
        self.apply_outline(outline, cx);
        if let Some(block) = block {
            self.resume_live_at(block, selection.start, window, cx);
        }
    }

    pub(super) fn split_live_paragraph(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(edit) = &self.live_edit else { return false };
        if !edit.projection.rich || edit.part.is_some() {
            return false;
        }
        let selection = edit.input.read(cx).selected_range();
        let left = edit.projection.replace(&edit.projection.text[..selection.start]);
        let right =
            paragraph_source(&edit.projection.replace(&edit.projection.text[selection.end..]));
        let cursor = edit.range.start + left.len() + 2;
        let mut source = self.input.read(cx).value().to_string();
        self.history.record(&source);
        source.replace_range(edit.range.clone(), &format!("{left}\n\n{right}"));
        self.finish_live_edit(cx);
        self.replace_document_text(&source, window, cx);
        self.history.record(&source);
        let mut outline = Outline::parse(&source);
        let block = outline.edit_block_at(cursor);
        self.apply_outline(outline, cx);
        self.begin_live_edit(block, window, cx);
        if let Some(edit) = &self.live_edit {
            edit.input.update(cx, |input, cx| input.set_selected_range(0..0, cx));
        }
        self.scroll.scroll_to_reveal_item(block);
        true
    }

    pub(super) fn replace_document_text(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = self.input.read(cx).value();
        let (removed, inserted) = changed_span(&source, text);
        self.input.update(cx, |input, cx| {
            input.set_selected_range(removed, cx);
            input.replace(text[inserted].to_owned(), window, cx);
        });
    }
}

pub(super) fn paragraph_source(source: &str) -> String {
    if let Ok(markdown::mdast::Node::Root(root)) =
        markdown::to_mdast(source, &markdown::ParseOptions::gfm())
    {
        if let Some(markdown::mdast::Node::Heading(heading)) = root.children.first() {
            if let (Some(first), Some(last)) = (
                heading.children.first().and_then(markdown::mdast::Node::position),
                heading.children.last().and_then(markdown::mdast::Node::position),
            ) {
                return source[first.start.offset..last.end.offset].to_owned();
            }
        }
    }
    source.to_owned()
}
