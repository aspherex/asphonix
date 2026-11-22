//! Global single-key shortcuts: L (like the current track), J/K (scroll the page),
//! `/` (focus search), mouse side buttons (history back/forward).
use super::shell::Shell;
use crate::app;
use gpui::{Context, Window, actions};
use sonix_api::{StarRequest, StarTargetType};

actions!(sonix_keys, [LikeCurrent, SlashSearch]);

impl Shell {
    /// A shell-level popover/modal or menu owns single keys while open.
    pub(crate) fn overlay_open(&self, cx: &gpui::App) -> bool {
        self.overlays.read(cx).is_open() || self.menu.read(cx).is_open() || self.lists_editor_open(cx)
    }

    /// L: star/unstar the playing track (no-op without one).
    pub(crate) fn like_current(&mut self, cx: &mut Context<Self>) {
        if self.overlay_open(cx) {
            return;
        }
        let Some(t) = self.player.read(cx).state.track.clone() else { return };
        let (id, on) = (t.id.clone(), t.starred.is_some());
        app::call(cx, move |b| {
            let req = StarRequest { id, kind: StarTargetType::Track };
            let _ = if on { b.star_remove(req) } else { b.star_add(req) };
        });
    }

    /// `/`: same as Ctrl+K (navigate to Search, focus the field) unless a popover owns the keyboard.
    pub(crate) fn slash_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay_open(cx) {
            return;
        }
        window.dispatch_action(Box::new(super::shell::FocusSearch), cx);
    }
}

/// Focus is in a text field, a context menu or a dialog.
pub(crate) fn typing_or_in_overlay(window: &Window) -> bool {
    window.context_stack().iter().any(|c| c.contains("TextInput") || c.contains("ContextMenu") || c.contains("Dialog"))
}

impl Shell {
    /// Root key-down hook for keys whose `key` name depends on the keyboard layout: `/` is Shift+7 on a German
    /// layout, so match the produced character instead of a binding.
    pub(crate) fn feel_key_down(&mut self, ev: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = ev.keystroke.modifiers;
        if m.control || m.alt || m.platform {
            return;
        }
        if typing_or_in_overlay(window) {
            return;
        }
        if ev.keystroke.key_char.as_deref() == Some("/") && !ev.is_held {
            self.slash_search(window, cx);
            cx.stop_propagation();
            return;
        }
        // N / Shift+N: toggle Now Playing, not on key repeat, not over an open menu/dialog
        if ev.keystroke.key == "n" {
            if !ev.is_held && !self.overlay_open(cx) {
                self.toggle_now_playing(cx);
                cx.stop_propagation();
            }
            return;
        }
        // J/K scroll the page by 120 px everywhere but Now Playing (there they skip -10/+10 s); held keys repeat
        let dy = match ev.keystroke.key.as_str() {
            "j" => 120.0,
            "k" => -120.0,
            _ => return,
        };
        if *self.router.current() == crate::router::Route::NowPlaying || self.overlay_open(cx) {
            return;
        }
        // applied in the next full frame (`smooth::scroll_largest`)
        crate::widgets::smooth::scroll_largest(dy);
        window.refresh();
        cx.stop_propagation();
    }
}

#[cfg(test)]
mod tests {
    use gpui::{KeyBindingContextPredicate, KeyContext};

    /// The single-key predicate used in `main.rs` (Space, N, L): active on the shell, inert inside a text field,
    /// a context menu or a dialog.
    #[test]
    fn single_key_predicate_skips_inputs_menus_and_dialogs() {
        let p = KeyBindingContextPredicate::parse("Root && !TextInput && !ContextMenu && !Dialog").unwrap();
        let stack = |names: &[&str]| names.iter().map(|n| KeyContext::parse(n).unwrap()).collect::<Vec<_>>();
        assert!(p.depth_of(&stack(&["Root"])).is_some());
        assert!(p.depth_of(&stack(&["Root", "TrackList"])).is_some());
        assert!(!p.depth_of(&stack(&["Root", "TextInput"])).is_some());
        assert!(!p.depth_of(&stack(&["Root", "ContextMenu"])).is_some());
        assert!(!p.depth_of(&stack(&["Root", "Dialog"])).is_some());
    }
}
