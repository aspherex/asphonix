//! Drag and drop: a drag carries tracks, an album or an artist; targets (sidebar playlists, the queue rail)
//! resolve it to track ids at drop time ([`resolve_ids`]). Built on GPUI's `on_drag` / `on_drop` /
//! `drag_over`; the drag image is the tiny accent chip with the label ("3 tracks"), not a snapshot of the row.
use crate::theme::Theme;
use crate::text::txt;
use crate::tracking::MONO_DATA;
use gpui::{App, Context, Entity, IntoElement, Pixels, Point, Render, SharedString, Window, div, prelude::*, px};
use asphonix_api::Backend;
use asphonix_i18n::tr;

#[derive(Clone, Debug)]
pub enum DragPayload {
    Tracks(Vec<String>),
    /// An album card / a Search album row
    Album { id: String, name: String },
    /// An artist card / a Search artist row
    Artist { id: String, name: String },
}

/// The dragged value.
#[derive(Clone, Debug)]
pub struct AsphonixDrag(pub DragPayload);

/// Text of a drag with `count` resolved tracks ("3 tracks", "Name (12 tracks)").
pub fn describe(p: &DragPayload, count: usize) -> String {
    match p {
        DragPayload::Tracks(_) => tr!(CountTracks, count = count),
        DragPayload::Album { name, .. } | DragPayload::Artist { name, .. } => tr!(DragNamedTracks, name = name.as_str(), count = count),
    }
}

/// The track ids a drag stands for. Blocking (call from the background executor).
pub fn resolve_ids(p: &DragPayload, b: &dyn Backend) -> Vec<String> {
    match p {
        DragPayload::Tracks(ids) => ids.clone(),
        DragPayload::Album { id, .. } => b.browse_get_album(id).map(|a| a.tracks.into_iter().map(|t| t.id).collect()).unwrap_or_default(),
        DragPayload::Artist { id, .. } => b.browse_get_artist_tracks(id).map(|t| t.into_iter().map(|t| t.id).collect()).unwrap_or_default(),
    }
}

/// The drag chip: 4/8 px padding, accent fill, accent-text bold mono label.
pub struct Ghost {
    pub label: SharedString,
    pub th: Theme,
}

impl Render for Ghost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px(px(8.)).py(px(4.)).bg(self.th.accent).child(txt(&self.th, MONO_DATA, self.label.to_string()).weight(700.0).color(self.th.accent_text))
    }
}

/// Label of the drag chip ("3 tracks" / "album: Name").
pub fn ghost_label(p: &DragPayload) -> String {
    match p {
        DragPayload::Tracks(ids) => describe(p, ids.len()),
        DragPayload::Album { name, .. } => tr!(DragAlbum, name = name.as_str()),
        DragPayload::Artist { name, .. } => tr!(DragArtist, name = name.as_str()),
    }
}

/// The `on_drag` constructor of every ASPHONIX drag source: the chip with `ghost_label`.
pub fn ghost(th: Theme) -> impl Fn(&AsphonixDrag, Point<Pixels>, &mut Window, &mut App) -> Entity<Ghost> + 'static {
    move |d, _, _, cx| {
        let label: SharedString = ghost_label(&d.0).into();
        cx.new(|_| Ghost { label, th })
    }
}
