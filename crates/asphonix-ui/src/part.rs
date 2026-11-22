//! View caching (CPU work). GPUI re-renders a dirty view and every view above it; a child view placed with
//! [`cached`] is reused (layout + paint recorded last frame) unless it notified itself since. Every heavy subtree
//! (the stage pages, the rail, the dock, the window chrome) is a cached view, so a frame driven by something small
//! (a spectrum, the now-playing bars, the position tick) rebuilds only that small view and its thin ancestors.
//!
//! Rule for a cached view: it repaints only on its own `cx.notify()` (or a descendant view's). Anything else it
//! reads in `render` (the player model, Shell state) must be observed and turned into a notify of the view.
//!
//! [`Part`] renders a piece of another entity's UI (the owner's `fn(&mut T, ..) -> AnyElement`) as its own view, so
//! that piece can be cached apart from the owner; it re-renders whenever the owner notifies.
use gpui::{AnyElement, AnyView, Context, Entity, IntoElement, Render, StyleRefinement, Subscription, WeakEntity, Window};

/// `view` as a cached child of the given outer style (it must reproduce the view's own root sizing: the placeholder
/// node takes this style in the parent's layout, then the view lays out inside the resulting bounds).
pub fn cached(view: impl Into<AnyView>, style: StyleRefinement) -> AnyView {
    let v: AnyView = view.into();
    v.cached(style)
}

/// The common outer style: fill the parent (`size_full`).
pub fn full() -> StyleRefinement {
    use gpui::Styled;
    let mut d = gpui::div().size_full();
    std::mem::take(d.style())
}

pub type PartFn<T> = fn(&mut T, &mut Window, &mut Context<T>) -> AnyElement;

pub struct Part<T: 'static> {
    owner: WeakEntity<T>,
    render: PartFn<T>,
    name: &'static str,
    _sub: Subscription,
}

impl<T: 'static> Part<T> {
    /// A part of `owner`'s UI, re-rendered whenever `owner` notifies.
    pub fn new(owner: &Entity<T>, name: &'static str, render: PartFn<T>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(owner, |_, _, cx| cx.notify());
        Self { owner: owner.downgrade(), render, name, _sub: sub }
    }
}

impl<T: 'static> Render for Part<T> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::rcount::hit(self.name);
        crate::live::begin_view(window);
        let f = self.render;
        self.owner.update(cx, |o, cx| f(o, window, cx)).unwrap_or_else(|_| gpui::Empty.into_any_element())
    }
}
