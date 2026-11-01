//! Drag and drop (`lib/dnd.ts`): a drag carries tracks, an album or an artist; targets (sidebar playlists, the queue rail)
//! resolve it to track ids at drop time, exactly like `resolveDragTrackIds`. Built on GPUI's `on_drag` / `on_drop` /
//! `drag_over`; the drag image is the tiny accent chip with the label ("3 tracks"), not a snapshot of the row.
use crate::theme::Theme;
use crate::text::txt;
use crate::tracking::MONO_DATA;
use gpui::{Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};
use sonix_api::Backend;

#[derive(Clone, Debug)]
pub enum DragPayload {
    Tracks(Vec<String>),
    Album { id: String, name: String },
    Artist { id: String, name: String },
}

/// The dragged value (what `application/x-sonix-tracks` carries on the web).
#[derive(Clone, Debug)]
pub struct SonixDrag(pub DragPayload);

/// `describeDrag`
pub fn describe(p: &DragPayload, count: usize) -> String {
    let n = if count == 1 { "track" } else { "tracks" };
    match p {
        DragPayload::Tracks(_) => format!("{count} {n}"),
        DragPayload::Album { name, .. } | DragPayload::Artist { name, .. } => format!("{name} ({count} {n})"),
    }
}

/// `resolveDragTrackIds`: blocking (call from the background executor).
pub fn resolve_ids(p: &DragPayload, b: &dyn Backend) -> Vec<String> {
    match p {
        DragPayload::Tracks(ids) => ids.clone(),
        DragPayload::Album { id, .. } => b.browse_get_album(id).map(|a| a.tracks.into_iter().map(|t| t.id).collect()).unwrap_or_default(),
        DragPayload::Artist { id, .. } => b.browse_get_artist_tracks(id).map(|t| t.into_iter().map(|t| t.id).collect()).unwrap_or_default(),
    }
}

/// `setGhost`: `padding: 4px 8px; background: accent; color: accent-text; font: 700 12px/16px mono; uppercase`.
pub struct Ghost {
    pub label: SharedString,
    pub th: Theme,
}

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(8.)).py(px(4.)).bg(self.th.accent).child(txt(&self.th, MONO_DATA, self.label.to_string()).weight(700.0).color(self.th.accent_text))
    }
}

/// Label of a track drag: the web's drag image text ("3 tracks" / "album: Name").
pub fn ghost_label(p: &DragPayload) -> String {
    match p {
        DragPayload::Tracks(ids) => describe(p, ids.len()),
        DragPayload::Album { name, .. } => format!("album: {name}"),
        DragPayload::Artist { name, .. } => format!("artist: {name}"),
    }
}
