use crate::{
    config::{self, Theme},
    key,
    ui::{self, Orientation},
    utils::filtered_items_from_query,
};

pub type UIStateGuard<'a> = parking_lot::MutexGuard<'a, UIState>;

mod page;
mod popup;

use super::{model::SearchResultCategory, TracksId};

pub use page::*;
pub use popup::*;

#[derive(Default, Debug)]
#[cfg(feature = "image")]
pub struct ImageRenderInfo {
    pub url: String,
    pub render_area: ratatui::layout::Rect,
    /// indicates if the image is rendered
    pub rendered: bool,
}

/// Application's UI state
#[derive(Debug)]
pub struct UIState {
    pub is_running: bool,
    pub theme: config::Theme,
    pub input_key_sequence: key::KeySequence,
    pub orientation: ui::Orientation,
    pub last_mouse_click: Option<(std::time::Instant, u16, u16)>,
    pub last_mouse_position: Option<(u16, u16)>,
    pub pending_client_requests: Vec<PendingClientRequest>,

    pub history: Vec<PageState>,
    pub popup: Option<PopupState>,

    /// The rectangle representing the playback progress bar,
    /// which is mainly used to handle mouse click events (for seeking command)
    pub playback_progress_bar_rect: ratatui::layout::Rect,
    pub search_layout: SearchLayout,
    pub library_layout: LibraryLayout,
    pub browse_layout: BrowseLayout,
    pub context_track_table_rect: Option<ratatui::layout::Rect>,

    /// Count prefix for vim-style navigation (e.g., 5j, 10k)
    pub count_prefix: Option<usize>,

    #[cfg(feature = "image")]
    pub last_cover_image_render_info: ImageRenderInfo,
}

impl UIState {
    pub fn current_page(&self) -> &PageState {
        self.history.last().expect("non-empty history")
    }

    pub fn current_page_mut(&mut self) -> &mut PageState {
        self.history.last_mut().expect("non-empty history")
    }

    pub fn new_search_popup(&mut self) {
        self.current_page_mut().select(0);
        self.popup = Some(PopupState::Search {
            query: String::new(),
        });
    }

    pub fn new_page(&mut self, page: PageState) {
        self.history.push(page);
        self.popup = None;
    }

    pub fn new_radio_page(&mut self, uri: &str) {
        self.new_page(PageState::Context {
            id: None,
            context_page_type: ContextPageType::Browsing(super::ContextId::Tracks(TracksId::new(
                format!("radio:{uri}"),
                "Recommendations",
            ))),
            state: None,
        });
    }

    /// Return whether there exists a focused popup.
    ///
    /// Currently, only search popup is not focused when it's opened.
    pub fn has_focused_popup(&self) -> bool {
        match self.popup.as_ref() {
            None => false,
            Some(popup) => !matches!(popup, PopupState::Search { .. }),
        }
    }

    /// Get a list of items possibly filtered by a search query if exists a search popup
    pub fn search_filtered_items<'a, T: std::fmt::Display>(&self, items: &'a [T]) -> Vec<&'a T> {
        match self.popup {
            Some(PopupState::Search { ref query }) => filtered_items_from_query(query, items),
            _ => items.iter().collect::<Vec<_>>(),
        }
    }
}

use ratatui::layout::Rect;

impl Default for UIState {
    fn default() -> Self {
        Self {
            is_running: true,
            theme: Theme::default(),
            input_key_sequence: key::KeySequence { keys: vec![] },
            orientation: match crossterm::terminal::size() {
                Ok((columns, rows)) => ui::Orientation::from_size(columns, rows),
                Err(err) => {
                    tracing::warn!("Unable to get terminal size, error: {err:#}");
                    Orientation::default()
                }
            },
            last_mouse_click: None,
            last_mouse_position: None,

            history: vec![PageState::Library {
                state: LibraryPageUIState::new(),
            }],
            popup: None,
            pending_client_requests: Vec::new(),

            playback_progress_bar_rect: Rect::default(),
            search_layout: SearchLayout::default(),
            library_layout: LibraryLayout::default(),
            browse_layout: BrowseLayout::default(),
            context_track_table_rect: None,

            count_prefix: None,

            #[cfg(feature = "image")]
            last_cover_image_render_info: ImageRenderInfo::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SearchLayout {
    pub valid: bool,
    pub input: Rect,
    pub tracks: Rect,
    pub albums: Rect,
    pub artists: Rect,
    pub playlists: Rect,
    pub shows: Rect,
    pub episodes: Rect,
}

impl Default for SearchLayout {
    fn default() -> Self {
        Self {
            valid: false,
            input: Rect::default(),
            tracks: Rect::default(),
            albums: Rect::default(),
            artists: Rect::default(),
            playlists: Rect::default(),
            shows: Rect::default(),
            episodes: Rect::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LibraryLayout {
    pub valid: bool,
    pub playlists: Rect,
    pub albums: Rect,
    pub artists: Rect,
}

#[derive(Debug, Clone, Copy)]
pub struct BrowseLayout {
    pub valid: bool,
    pub list: Rect,
}

#[derive(Debug)]
pub enum PendingClientRequest {
    SearchMore {
        query: String,
        category: SearchResultCategory,
    },
}

impl Default for LibraryLayout {
    fn default() -> Self {
        Self {
            valid: false,
            playlists: Rect::default(),
            albums: Rect::default(),
            artists: Rect::default(),
        }
    }
}

impl Default for BrowseLayout {
    fn default() -> Self {
        Self {
            valid: false,
            list: Rect::default(),
        }
    }
}
