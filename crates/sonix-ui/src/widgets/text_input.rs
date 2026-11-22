//! Single-line text input: caret, selection (shift+arrows, mouse drag, double-click =
//! word, triple-click = all), word navigation (ctrl+left/right, ctrl+backspace/delete),
//! clipboard (copy/cut/paste) and IME composition through GPUI's `EntityInputHandler`.
//! Derived from GPUI's own `input` example (Apache-2.0), extended for SONIX.
//! Gaps (docs/GPUI-GAPS.md): IME composition is wired but
//! was not exercised with a real IME, no bidi/RTL caret handling.
use std::ops::Range;

use crate::theme::Theme;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter,
    FocusHandle, Focusable, GlobalElementId, Hsla, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PaintQuad, Pixels, Point, Render, ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, actions, div,
    fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::*;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        DeleteWordLeft,
        DeleteWordRight,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Submit,
    ]
);

pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding as K;
    let c = Some("TextInput");
    cx.bind_keys([
        K::new("backspace", Backspace, c),
        K::new("delete", Delete, c),
        K::new("left", Left, c),
        K::new("right", Right, c),
        K::new("shift-left", SelectLeft, c),
        K::new("shift-right", SelectRight, c),
        K::new("ctrl-a", SelectAll, c),
        K::new("ctrl-v", Paste, c),
        K::new("ctrl-c", Copy, c),
        K::new("ctrl-x", Cut, c),
        K::new("home", Home, c),
        K::new("end", End, c),
        K::new("ctrl-left", WordLeft, c),
        K::new("ctrl-right", WordRight, c),
        K::new("ctrl-shift-left", SelectWordLeft, c),
        K::new("ctrl-shift-right", SelectWordRight, c),
        K::new("ctrl-backspace", DeleteWordLeft, c),
        K::new("ctrl-delete", DeleteWordRight, c),
        K::new("enter", Submit, c),
        K::new("ctrl-z", Undo, c),
        K::new("ctrl-y", Redo, c),
        K::new("ctrl-shift-z", Redo, c),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Changed,
    Submit,
}
impl EventEmitter<InputEvent> for TextInput {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CharClass {
    Space,
    Word,
    Other,
}
fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Other
    }
}

/// Start of the word before `offset` (a byte offset on a char boundary): skips
/// whitespace, then one run of same-class characters.
pub fn prev_word_boundary(s: &str, offset: usize) -> usize {
    let mut chars: Vec<(usize, char)> = s[..offset].char_indices().collect();
    while matches!(chars.last(), Some((_, c)) if class(*c) == CharClass::Space) {
        chars.pop();
    }
    let Some(&(_, last)) = chars.last() else { return 0 };
    let cl = class(last);
    while matches!(chars.last(), Some((_, c)) if class(*c) == cl) {
        chars.pop();
    }
    chars.last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0)
}

/// End of the word after `offset`: skips one run of same-class characters, then
/// whitespace (like Windows text fields: lands on the start of the next word).
pub fn next_word_boundary(s: &str, offset: usize) -> usize {
    let mut it = s[offset..].char_indices().peekable();
    if let Some(&(_, c)) = it.peek() {
        let cl = class(c);
        if cl != CharClass::Space {
            while matches!(it.peek(), Some((_, c)) if class(*c) == cl) {
                it.next();
            }
        }
    }
    while matches!(it.peek(), Some((_, c)) if class(*c) == CharClass::Space) {
        it.next();
    }
    it.peek().map(|(i, _)| offset + i).unwrap_or(s.len())
}

/// The word (or whitespace run / symbol run) containing `offset`, for double-click.
pub fn word_range_at(s: &str, offset: usize) -> Range<usize> {
    if s.is_empty() {
        return 0..0;
    }
    let offset = offset.min(s.len());
    let at = s[offset..].chars().next().or_else(|| s[..offset].chars().next_back()).unwrap();
    let cl = class(at);
    let mut start = offset;
    for (i, c) in s[..offset].char_indices().rev() {
        if class(c) == cl {
            start = i;
        } else {
            break;
        }
    }
    let mut end = offset;
    for (i, c) in s[offset..].char_indices() {
        if class(c) == cl {
            end = offset + i + c.len_utf8();
        } else {
            break;
        }
    }
    start..end
}

pub struct TextInput {
    pub focus_handle: FocusHandle,
    pub content: SharedString,
    pub placeholder: SharedString,
    pub font_family: &'static str,
    pub font_size: f32,
    pub line_height: f32,
    pub font_weight: f32,
    pub col_text: Hsla,
    pub col_muted: Hsla,
    pub col_cursor: Hsla,
    pub col_sel: Hsla,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    /// password mode: the input itself shows one bullet per grapheme (selection and caret follow the bullets)
    pub mask: bool,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// Chromium selects the whole value when an input is focused by keyboard (Tab, Shift+Tab) or script, not by a click.
    focus_seen: bool,
    mouse_focus: bool,
    /// Ctrl+Z / Ctrl+Y (Chromium `<input>` undo): snapshots before each edit, typing runs coalesced
    history: History,
    /// Right-click menu (Undo/Redo/Cut/Copy/Paste/Select all, the WebView2 input menu). Off for inputs inside modals:
    /// the shell menu paints below dialogs and the smart editor.
    pub context_menu: bool,
}

impl TextInput {
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let o = prev_word_boundary(&self.content, self.cursor_offset());
        self.move_to(o, cx);
    }
    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        let o = next_word_boundary(&self.content, self.cursor_offset());
        self.move_to(o, cx);
    }
    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        let o = prev_word_boundary(&self.content, self.cursor_offset());
        self.select_to(o, cx);
    }
    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        let o = next_word_boundary(&self.content, self.cursor_offset());
        self.select_to(o, cx);
    }
    fn delete_word_left(&mut self, _: &DeleteWordLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let o = prev_word_boundary(&self.content, self.cursor_offset());
            self.select_to(o, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn delete_word_right(&mut self, _: &DeleteWordRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let o = next_word_boundary(&self.content, self.cursor_offset());
            self.select_to(o, cx);
        }
        self.replace_text_in_range(None, "", window, cx)
    }
    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(InputEvent::Submit);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mouse_focus = true;
        window.focus(&self.focus_handle);
        // a focusable ancestor (the shell root) would otherwise take the focus back on the same mouse-down
        cx.stop_propagation();
        self.is_selecting = true;

        let at = self.index_for_mouse_position(event.position);
        if event.click_count >= 3 {
            self.selected_range = 0..self.content.len();
            self.selection_reversed = false;
            self.is_selecting = false;
            cx.notify();
        } else if event.click_count == 2 {
            self.selected_range = word_range_at(&self.content, at);
            self.selection_reversed = false;
            self.is_selecting = false;
            cx.notify();
        } else if event.modifiers.shift {
            self.select_to(at, cx);
        } else {
            self.move_to(at, cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace("\n", " "), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.mask {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.mask {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    const BULLET: &str = "•";

    /// content byte offset -> offset in the displayed (masked) text
    fn disp(&self, off: usize) -> usize {
        if self.mask { self.content[..off.min(self.content.len())].graphemes(true).count() * Self::BULLET.len() } else { off }
    }

    /// offset in the displayed text -> content byte offset
    fn undisp(&self, d: usize) -> usize {
        if !self.mask {
            return d;
        }
        let n = d / Self::BULLET.len();
        self.content.grapheme_indices(true).nth(n).map(|(i, _)| i).unwrap_or(self.content.len())
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }

        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        self.undisp(line.closest_index_for_x(position.x - bounds.left()))
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    pub fn new(cx: &mut Context<Self>, placeholder: &str, th: &Theme, font_family: &'static str, font_size: f32, line_height: f32, font_weight: f32) -> Self {
        Self {
            // `<input>` is a tab stop (document order = paint order)
            focus_handle: cx.focus_handle().tab_stop(true),
            content: "".into(),
            placeholder: placeholder.to_string().into(),
            font_family,
            font_size,
            line_height,
            font_weight,
            col_text: th.text,
            col_muted: th.muted,
            col_cursor: th.accent_fg,
            col_sel: th.accent.opacity(0.35),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            mask: false,
            last_bounds: None,
            is_selecting: false,
            focus_seen: false,
            mouse_focus: false,
            history: History::default(),
            context_menu: true,
        }
    }

    /// Replace the whole content and select it (`input.select()` after a prompt opens).
    pub fn set_content(&mut self, text: &str, cx: &mut Context<Self>) {
        self.history = History::default();
        self.content = text.to_string().into();
        self.selected_range = 0..text.len();
        self.selection_reversed = false;
        self.marked_range = None;
        cx.notify();
    }

    pub fn set_theme(&mut self, th: &Theme) {
        self.col_text = th.text;
        self.col_muted = th.muted;
        self.col_cursor = th.accent_fg;
        self.col_sel = th.accent.opacity(0.35);
    }

}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        // committing an IME composition: its snapshot was taken when it started (`replace_and_mark_text_in_range`)
        if self.marked_range.is_none() {
            self.history.record(&self.content, &self.selected_range, &range, new_text);
        } else {
            self.history.typing_at = None;
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        // a composition starts: snapshot the text before it, so undo never restores half-composed text
        if self.marked_range.is_none() {
            self.history.record(&self.content, &self.selected_range, &range, "");
            self.history.typing_at = None;
        }
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| new_range.start + range.start..new_range.end + range.end)
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());

        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let last_layout = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range_utf16);
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(self.disp(range.start)),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(self.disp(range.end)),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;

        let utf8_index = self.undisp(last_layout.index_for_x(point.x - line_point.x)?);
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let weight = input.font_weight;

        let masked = input.mask && !content.is_empty();
        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), input.col_muted)
        } else if masked {
            (SharedString::from(TextInput::BULLET.repeat(content.graphemes(true).count())), input.col_text)
        } else {
            (content, input.col_text)
        };

        let run = TextRun {
            len: display_text.len(),
            font: {
                let mut f = style.font();
                f.weight = gpui::FontWeight(weight);
                f
            },
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = if let (false, Some(marked_range)) = (masked, input.marked_range.as_ref()) {
            vec![
                TextRun {
                    len: marked_range.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked_range.end - marked_range.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked_range.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        } else {
            vec![run]
        };

        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_pos = line.x_for_index(if masked { input.disp(cursor) } else { cursor });
        let (selection, cursor) = if selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    input.col_cursor,
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(if masked { input.disp(selected_range.start) } else { selected_range.start }),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(if masked { input.disp(selected_range.end) } else { selected_range.end }),
                            bounds.bottom(),
                        ),
                    ),
                    input.col_sel,
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), window, cx)
            .unwrap();

        if focus_handle.is_focused(window)
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }

        let focused = focus_handle.is_focused(window);
        self.input.update(cx, |input, cx| {
            if focused && !input.focus_seen && !input.mouse_focus && tab_nav_recent() && !input.content.is_empty() {
                input.selected_range = 0..input.content.len();
                input.selection_reversed = false;
                cx.notify();
            }
            if !focused {
                input.mouse_focus = false;
            }
            input.focus_seen = focused;
            input.last_layout = Some(line);
            input.last_bounds = Some(bounds);
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit("input");
        div()
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::delete_word_right))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full()
            .font_family(self.font_family)
            .line_height(px(self.line_height))
            .text_size(px(self.font_size))
            .child(div().w_full().child(TextElement { input: cx.entity() }))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_navigation_matches_windows_fields() {
        let s = "foo bar-baz  qux";
        // ctrl+right lands on the start of the next word
        assert_eq!(next_word_boundary(s, 0), 4);
        assert_eq!(next_word_boundary(s, 4), 7); // "bar", then the "-" run starts
        assert_eq!(next_word_boundary(s, 7), 8);
        assert_eq!(next_word_boundary(s, 8), 13);
        assert_eq!(next_word_boundary(s, 13), s.len());
        assert_eq!(next_word_boundary(s, s.len()), s.len());
        // ctrl+left lands on the start of the previous word
        assert_eq!(prev_word_boundary(s, s.len()), 13);
        assert_eq!(prev_word_boundary(s, 13), 8);
        assert_eq!(prev_word_boundary(s, 8), 7);
        assert_eq!(prev_word_boundary(s, 7), 4);
        assert_eq!(prev_word_boundary(s, 4), 0);
        assert_eq!(prev_word_boundary(s, 0), 0);
    }

    #[test]
    fn word_navigation_is_utf8_safe() {
        let s = "ärger über öl";
        let n = next_word_boundary(s, 0);
        assert!(s.is_char_boundary(n));
        assert_eq!(&s[n..], "über öl");
        assert_eq!(&s[prev_word_boundary(s, s.len())..], "öl");
    }

    #[test]
    fn double_click_selects_the_word() {
        let s = "alpha beta";
        assert_eq!(&s[word_range_at(s, 2)], "alpha");
        assert_eq!(&s[word_range_at(s, 7)], "beta");
        assert_eq!(&s[word_range_at(s, 5)], " ");
        assert_eq!(&s[word_range_at(s, s.len())], "beta");
        assert_eq!(word_range_at("", 0), 0..0);
    }
}

thread_local! {
    static TAB_NAV: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

/// The shell calls this right before `focus_next`/`focus_prev`: an input that receives focus within the next moments
/// selects its whole value, as Chromium does for Tab navigation (script `focus()` leaves the caret alone).
pub fn note_tab_nav() {
    TAB_NAV.with(|t| t.set(Some(std::time::Instant::now())));
}

fn tab_nav_recent() -> bool {
    TAB_NAV.with(|t| t.get().is_some_and(|i| i.elapsed() < std::time::Duration::from_millis(250)))
}

actions!(text_input, [Undo, Redo]);

/// Undo/redo stacks of one input. A snapshot (text + selection) is taken before every edit; a run of single
/// characters typed at the caret is one step, like Chromium's typing coalescing.
#[derive(Default)]
struct History {
    undo: Vec<(SharedString, Range<usize>)>,
    redo: Vec<(SharedString, Range<usize>)>,
    /// caret after the last typed character of the current run
    typing_at: Option<usize>,
}

impl History {
    const MAX: usize = 100;

    fn record(&mut self, content: &SharedString, sel: &Range<usize>, range: &Range<usize>, new_text: &str) {
        let typed = range.is_empty() && new_text.graphemes(true).count() == 1 && !new_text.chars().all(char::is_whitespace);
        let continues = typed && self.typing_at == Some(range.start);
        if !continues {
            self.undo.push((content.clone(), sel.clone()));
            if self.undo.len() > Self::MAX {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.typing_at = typed.then_some(range.start + new_text.len());
    }

    fn step(from: &mut Vec<(SharedString, Range<usize>)>, to: &mut Vec<(SharedString, Range<usize>)>, content: &SharedString, sel: &Range<usize>) -> Option<(SharedString, Range<usize>)> {
        let prev = from.pop()?;
        to.push((content.clone(), sel.clone()));
        Some(prev)
    }
}

impl TextInput {
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let h = &mut self.history;
        h.typing_at = None;
        if let Some((c, s)) = History::step(&mut h.undo, &mut h.redo, &self.content, &self.selected_range) {
            self.restore_snapshot(c, s, cx);
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let h = &mut self.history;
        h.typing_at = None;
        if let Some((c, s)) = History::step(&mut h.redo, &mut h.undo, &self.content, &self.selected_range) {
            self.restore_snapshot(c, s, cx);
        }
    }

    fn restore_snapshot(&mut self, content: SharedString, sel: Range<usize>, cx: &mut Context<Self>) {
        let end = content.len();
        self.content = content;
        self.selected_range = sel.start.min(end)..sel.end.min(end);
        self.marked_range = None;
        cx.emit(InputEvent::Changed);
        cx.notify();
    }
}

#[cfg(test)]
mod history_tests {
    use super::History;
    use gpui::SharedString;

    fn s(t: &str) -> SharedString {
        t.to_string().into()
    }

    #[test]
    fn typing_run_is_one_undo_step_and_a_paste_is_another() {
        let mut h = History::default();
        // type "ab" at the caret, then paste " cd"
        h.record(&s(""), &(0..0), &(0..0), "a");
        h.record(&s("a"), &(1..1), &(1..1), "b");
        h.record(&s("ab"), &(2..2), &(2..2), " cd");
        assert_eq!(h.undo.len(), 2);
        assert_eq!(h.undo[0].0, s(""));
        assert_eq!(h.undo[1].0, s("ab"));
        // a redo stack is dropped by a new edit
        h.redo.push((s("x"), 0..0));
        h.record(&s("ab cd"), &(5..5), &(5..5), "e");
        assert!(h.redo.is_empty());
    }

    #[test]
    fn step_moves_between_stacks() {
        let mut h = History::default();
        h.record(&s(""), &(0..0), &(0..0), "a");
        let (mut u, mut r) = (std::mem::take(&mut h.undo), Vec::new());
        let prev = History::step(&mut u, &mut r, &s("a"), &(1..1)).unwrap();
        assert_eq!(prev.0, s(""));
        assert_eq!(r[0].0, s("a"));
        assert!(History::step(&mut u, &mut r, &s(""), &(0..0)).is_none());
    }
}

/// The shell's context menu, used by inputs for their right-click menu (set by `Shell::new`).
pub struct InputMenu(pub gpui::WeakEntity<super::context_menu::ContextMenu>);
impl gpui::Global for InputMenu {}

type InputOp = fn(&mut TextInput, &mut Window, &mut Context<TextInput>);

impl TextInput {
    /// Right-click: focus the field, keep a selection the click is inside (else move the caret there, as Chromium
    /// does), then open the edit menu with the WebView2 items and their shortcuts.
    fn on_right_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.mouse_focus = true;
        window.focus(&self.focus_handle);
        cx.stop_propagation();
        if !self.context_menu {
            return;
        }
        let at = self.index_for_mouse_position(ev.position);
        if !(self.selected_range.start <= at && at <= self.selected_range.end) {
            self.move_to(at, cx);
        }
        let Some(menu) = cx.try_global::<InputMenu>().and_then(|m| m.0.upgrade()) else { return };
        let me = cx.entity().downgrade();
        let item = |label: &'static str, hint: &'static str, enabled: bool, op: InputOp| {
            let me = me.clone();
            super::context_menu::MenuItem::action(label, move |window, cx| {
                if let Some(e) = me.upgrade() {
                    e.update(cx, |i, cx| op(i, window, cx));
                }
            })
            .hint(hint)
            .disabled(!enabled)
        };
        let sel = !self.selected_range.is_empty() && !self.mask;
        let items = vec![
            item("Undo", "Ctrl+Z", !self.history.undo.is_empty(), |i, w, cx| i.undo(&Undo, w, cx)),
            item("Redo", "Ctrl+Y", !self.history.redo.is_empty(), |i, w, cx| i.redo(&Redo, w, cx)),
            super::context_menu::MenuItem::separator(),
            item("Cut", "Ctrl+X", sel, |i, w, cx| i.cut(&Cut, w, cx)),
            item("Copy", "Ctrl+C", sel, |i, w, cx| i.copy(&Copy, w, cx)),
            item("Paste", "Ctrl+V", true, |i, w, cx| i.paste(&Paste, w, cx)),
            super::context_menu::MenuItem::separator(),
            item("Select all", "Ctrl+A", !self.content.is_empty(), |i, w, cx| i.select_all(&SelectAll, w, cx)),
        ];
        let pos = ev.position;
        menu.update(cx, |m, cx| m.show(items, pos, window, cx));
    }
}
