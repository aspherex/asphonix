//! The shell's cached pieces (`crate::part`, CPU work): the top bar and sidebar render as cached parts of the
//! shell (the title bar stays uncached, see `chrome_views`), the stage pages, the rail and the dock as cached views,
//! so the root frame driven by the live layer (`crate::live`) or the dock's position tick rebuilds only the root's
//! thin frame.
use super::shell::Shell;
use crate::metrics::layout;
use crate::part::{self, Part};
use gpui::{AnyElement, AppContext, Context, Entity, IntoElement, StyleRefinement, Styled, Window, div, px};

pub(crate) struct ShellParts {
    top: Entity<Part<Shell>>,
    side: Entity<Part<Shell>>,
}

fn style(d: gpui::Div) -> StyleRefinement {
    let mut d = d;
    std::mem::take(d.style())
}

fn top_part(sh: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    sh.top_bar(window, cx).into_any_element()
}
fn side_part(sh: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) -> AnyElement {
    let vw = f32::from(window.viewport_size().width);
    let route = sh.router.current().clone();
    sh.sidebar(vw, &route, window, cx).into_any_element()
}

impl Shell {
    /// Top bar and sidebar as cached views (each re-renders when the shell notifies). The title bar stays uncached: GPUI
    /// does not replay `window_control_area` hitboxes for a reused view (`reuse_prepaint` skips
    /// `Frame::window_control_hitboxes`), and WM_NCHITTEST reads only those: drag, double-click maximize, Snap and the
    /// min/max/close buttons would stop working on every reused frame.
    pub(crate) fn chrome_views(&mut self, vw: f32, window: &mut Window, cx: &mut Context<Self>) -> [AnyElement; 3] {
        if self.parts.is_none() {
            let me = cx.entity();
            self.parts = Some(ShellParts {
                top: cx.new(|cx| Part::new(&me, "topbar", top_part, cx)),
                side: cx.new(|cx| Part::new(&me, "sidebar", side_part, cx)),
            });
        }
        let p = self.parts.as_ref().unwrap();
        let (top, side) = (p.top.clone(), p.side.clone());
        [
            self.title_bar(window, cx).into_any_element(),
            part::cached(top, style(div().h(px(crate::metrics::topbar::H)).w_full().flex_none())).into_any_element(),
            part::cached(side, style(div().w(px(Self::sidebar_width(vw))).h_full().flex_none())).into_any_element(),
        ]
    }

    /// The rail (docked or in the drawer), cached: `QueueRail`'s root is `w(RAIL_W).h_full().flex_none()`.
    pub(crate) fn rail_view(&self) -> AnyElement {
        part::cached(self.queue.clone(), style(div().w(px(layout::RAIL_W)).h_full().flex_none())).into_any_element()
    }

    /// The dock, cached: `DockView`'s root is `h(DOCK_H).w_full().flex_none()`.
    pub(crate) fn dock_view(&self) -> AnyElement {
        part::cached(self.dock.clone(), style(div().h(px(layout::DOCK_H)).w_full().flex_none())).into_any_element()
    }

    /// A stage page, cached, filling the stage.
    pub(crate) fn stage_view(v: impl Into<gpui::AnyView>) -> AnyElement {
        part::cached(v, part::full()).into_any_element()
    }

    /// The layer that paints the animated elements (`crate::live`): above the pages, below menus and dialogs.
    pub(crate) fn live_view(&self) -> AnyElement {
        self.live.clone().into_any_element()
    }
}
