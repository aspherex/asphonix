//! Stable "parity ids" for laid-out elements. `pid("track.row.0.title", element)` is the identity
//! function in every normal build (the optimizer removes it). With the `parity` feature it wraps
//! the element in a transparent [`Probe`] that records the element's border-box bounds (logical
//! px, window coordinates, i.e. the same box as `getBoundingClientRect`) during prepaint, so the
//! parity runner can diff them against the web app's boxes.
use gpui::IntoElement;
#[cfg(not(feature = "parity"))]
use gpui::SharedString;

#[cfg(not(feature = "parity"))]
#[inline(always)]
pub fn pid<E: IntoElement>(_id: impl Into<SharedString>, e: E) -> E {
    e
}

#[cfg(not(feature = "parity"))]
#[inline(always)]
pub fn begin_frame() {}
#[cfg(not(feature = "parity"))]
#[inline(always)]
pub fn end_frame() {}

#[cfg(feature = "parity")]
pub use enabled::*;

#[cfg(feature = "parity")]
mod enabled {
    use gpui::{App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, SharedString, Window};
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    thread_local! {
        static BOXES: RefCell<BTreeMap<String, [f32; 4]>> = const { RefCell::new(BTreeMap::new()) };
        static FRAMES: Cell<u64> = const { Cell::new(0) };
    }

    pub fn begin_frame() {
        BOXES.with(|b| b.borrow_mut().clear());
    }
    pub fn end_frame() {
        FRAMES.with(|f| f.set(f.get() + 1));
    }
    pub fn frames() -> u64 {
        FRAMES.with(|f| f.get())
    }
    pub fn boxes() -> BTreeMap<String, [f32; 4]> {
        BOXES.with(|b| b.borrow().clone())
    }

    pub fn pid<E: IntoElement>(id: impl Into<SharedString>, e: E) -> Probe<E::Element> {
        Probe { id: id.into(), inner: e.into_element() }
    }

    pub struct Probe<E: Element> {
        id: SharedString,
        inner: E,
    }

    impl<E: Element> IntoElement for Probe<E> {
        type Element = Self;
        fn into_element(self) -> Self {
            self
        }
    }

    impl<E: Element> Element for Probe<E> {
        type RequestLayoutState = E::RequestLayoutState;
        type PrepaintState = E::PrepaintState;

        fn id(&self) -> Option<ElementId> {
            self.inner.id()
        }
        fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
            self.inner.source_location()
        }
        fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState) {
            self.inner.request_layout(id, inspector_id, window, cx)
        }
        fn prepaint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            state: &mut Self::RequestLayoutState,
            window: &mut Window,
            cx: &mut App,
        ) -> Self::PrepaintState {
            let r = [f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height)];
            BOXES.with(|b| b.borrow_mut().insert(self.id.to_string(), r));
            self.inner.prepaint(id, inspector_id, bounds, state, window, cx)
        }
        fn paint(
            &mut self,
            id: Option<&GlobalElementId>,
            inspector_id: Option<&InspectorElementId>,
            bounds: Bounds<Pixels>,
            state: &mut Self::RequestLayoutState,
            prepaint: &mut Self::PrepaintState,
            window: &mut Window,
            cx: &mut App,
        ) {
            self.inner.paint(id, inspector_id, bounds, state, prepaint, window, cx)
        }
    }
}

/// Last child of the root: counts painted frames (the runner's "settle" waits on it).
#[cfg(feature = "parity")]
pub fn frame_marker() -> impl IntoElement {
    use gpui::{canvas, prelude::*};
    canvas(|_, _, _| (), |_, _, _, _| end_frame()).absolute().size_full()
}
