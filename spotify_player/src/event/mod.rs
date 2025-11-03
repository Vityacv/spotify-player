use crate::{
    client::{ClientRequest, PlayerRequest},
    command::{
        self, construct_artist_actions, Action, ActionContext, ActionTarget, Command,
        CommandOrAction,
    },
    config,
    key::{Key, KeySequence},
    state::{
        ActionListItem, Album, AlbumId, Artist, ArtistFocusState, ArtistId, ArtistPopupAction,
        BrowsePageUIState, Context, ContextId, ContextPageType, ContextPageUIState, DataReadGuard,
        Focusable, Id, Item, ItemId, LibraryFocusState, LibraryPageUIState, PageState, PageType,
        PendingClientRequest, PlayableId, Playback, PlaylistCreateCurrentField, PlaylistFolderItem,
        PlaylistId, PlaylistPopupAction, PopupState, SearchFocusState, SearchPageUIState,
        SearchResultCategory, SharedState, ShowId, Track, TrackId, TrackOrder, UIStateGuard,
        LIBRARY_REFRESH_TTL, TTL_CACHE_DURATION, USER_LIKED_TRACKS_ID,
        USER_RECENTLY_PLAYED_TRACKS_ID, USER_TOP_TRACKS_ID,
    },
    ui::{single_line_input::LineInput, Orientation},
    utils::parse_uri,
};

use crate::utils::map_join;
use anyhow::{Context as _, Result};
use crossterm::event::KeyCode;

use clipboard::{execute_copy_command, get_clipboard_content};
use ratatui::widgets::ListState;

mod clipboard;
mod page;
mod popup;
mod window;

#[derive(Clone, Copy, Debug)]
enum ContextTrackSource {
    Album,
    Playlist,
    Tracks,
    ArtistTopTracks,
}

const SEARCH_PREFETCH_THRESHOLD: usize = 5;

pub(super) fn is_downward_command(command: &Command) -> bool {
    matches!(
        command,
        Command::SelectNextOrScrollDown
            | Command::PageSelectNextOrScrollDown
            | Command::SelectLastOrScrollToBottom
    )
}

pub(super) fn maybe_queue_search_prefetch(
    ui: &mut UIStateGuard,
    data: &DataReadGuard,
    query: &str,
    category: SearchResultCategory,
    list_len: usize,
) {
    if list_len == 0 {
        return;
    }

    let Some(selected) = ui.current_page_mut().selected() else {
        return;
    };

    if selected + SEARCH_PREFETCH_THRESHOLD < list_len {
        return;
    }

    if let Some(entry) = data.caches.search.get(query) {
        let info = entry.pagination.info(category);
        if info.is_exhausted || info.is_fetching {
            return;
        }
    } else {
        return;
    }

    if ui.pending_client_requests.iter().any(|req| {
        matches!(
            req,
            PendingClientRequest::SearchMore {
                query: existing_query,
                category: existing_category,
            } if existing_query == query && *existing_category == category
        )
    }) {
        return;
    }

    ui.pending_client_requests
        .push(PendingClientRequest::SearchMore {
            query: query.to_string(),
            category,
        });
}

impl ContextTrackSource {
    fn required_focus(self) -> Option<ArtistFocusState> {
        match self {
            ContextTrackSource::ArtistTopTracks => Some(ArtistFocusState::TopTracks),
            _ => None,
        }
    }
}

/// Start a terminal event handler (key pressed, mouse clicked, etc)
pub fn start_event_handler(state: &SharedState, client_pub: &flume::Sender<ClientRequest>) {
    while let Ok(event) = crossterm::event::read() {
        let _enter = tracing::info_span!("terminal_event", event = ?event).entered();
        if let Err(err) = match event {
            crossterm::event::Event::Mouse(event) => handle_mouse_event(event, client_pub, state),
            crossterm::event::Event::Resize(columns, rows) => {
                state.ui.lock().orientation = Orientation::from_size(columns, rows);
                Ok(())
            }
            crossterm::event::Event::Key(event) => {
                if event.kind == crossterm::event::KeyEventKind::Press {
                    // only handle key press event to avoid handling a key event multiple times
                    // context:
                    // - https://github.com/crossterm-rs/crossterm/issues/752
                    // - https://github.com/aome510/spotify-player/issues/136
                    handle_key_event(event, client_pub, state)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        } {
            tracing::error!("Failed to handle terminal event: {err:#}");
        }
    }
}

// Handle a terminal mouse event
fn handle_mouse_event(
    event: crossterm::event::MouseEvent,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
) -> Result<()> {
    use crossterm::event::{MouseButton, MouseEventKind};
    use std::time::{Duration, Instant};

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            tracing::debug!("Handling mouse event: {event:?}");
            let rect = state.ui.lock().playback_progress_bar_rect;
            if event.row == rect.y {
                // calculate the seek position (in ms) based on the mouse click position,
                // the progress bar's width and the track's duration (in ms)
                let player = state.player.read();
                let duration = match player.currently_playing() {
                    Some(rspotify::model::PlayableItem::Track(track)) => Some(track.duration),
                    Some(rspotify::model::PlayableItem::Episode(episode)) => Some(episode.duration),
                    Some(rspotify::model::PlayableItem::Unknown(_)) | None => None,
                };
                if let Some(duration) = duration {
                    let position_ms = (duration.num_milliseconds()) * i64::from(event.column)
                        / i64::from(rect.width);
                    client_pub.send(ClientRequest::Player(PlayerRequest::SeekTrack(
                        chrono::Duration::try_milliseconds(position_ms).unwrap(),
                    )))?;
                }
            }

            // Handle search page list selection
            let mut ui = state.ui.lock();
            let now = Instant::now();
            let is_double_click = ui
                .last_mouse_click
                .map(|(ts, _, row)| ts.elapsed() <= Duration::from_millis(500) && row == event.row)
                .unwrap_or(false);
            ui.last_mouse_click = Some((now, event.column, event.row));

            if let PageState::Search { .. } = ui.current_page() {
                let layout = ui.search_layout;
                if layout.valid {
                    let current_query = match ui.current_page() {
                        PageState::Search { current_query, .. } => current_query.clone(),
                        _ => unreachable!(),
                    };

                    let data = state.data.read();
                    let search_results = data.caches.search.get(&current_query);

                    let filtered_tracks = search_results
                        .map(|s| ui.search_filtered_items(&s.results.tracks))
                        .unwrap_or_default();
                    let filtered_albums = search_results
                        .map(|s| ui.search_filtered_items(&s.results.albums))
                        .unwrap_or_default();
                    let filtered_artists = search_results
                        .map(|s| ui.search_filtered_items(&s.results.artists))
                        .unwrap_or_default();
                    let playlist_items: Vec<PlaylistFolderItem> = search_results
                        .map(|s| {
                            ui.search_filtered_items(&s.results.playlists)
                                .into_iter()
                                .map(|p| PlaylistFolderItem::Playlist(p.clone()))
                                .collect()
                        })
                        .unwrap_or_default();
                    let filtered_playlists: Vec<&PlaylistFolderItem> =
                        playlist_items.iter().collect();
                    let filtered_shows = search_results
                        .map(|s| ui.search_filtered_items(&s.results.shows))
                        .unwrap_or_default();
                    let filtered_episodes = search_results
                        .map(|s| ui.search_filtered_items(&s.results.episodes))
                        .unwrap_or_default();

                    let PageState::Search {
                        state: search_state,
                        ..
                    } = ui.current_page_mut()
                    else {
                        unreachable!();
                    };

                    let within = |rect: ratatui::layout::Rect| -> bool {
                        event.column >= rect.x
                            && event.column < rect.x.saturating_add(rect.width)
                            && event.row >= rect.y
                            && event.row < rect.y.saturating_add(rect.height)
                    };

                    let mut handled = false;
                    let mut clicked_focus: Option<SearchFocusState> = None;

                    if within(layout.input) {
                        search_state.focus = SearchFocusState::Input;
                        handled = true;
                    }

                    let mut handle_list_click =
                        |rect: ratatui::layout::Rect,
                         list_state: &mut ratatui::widgets::ListState,
                         len: usize,
                         focus: SearchFocusState| {
                            if !within(rect) {
                                return;
                            }

                            search_state.focus = focus;
                            handled = true;
                            clicked_focus = Some(focus);

                            if len == 0 || rect.height == 0 {
                                return;
                            }

                            let relative = event.row.saturating_sub(rect.y) as usize;
                            let height = rect.height as usize;
                            if relative >= height {
                                return;
                            }

                            let mut index = list_state.offset().saturating_add(relative);
                            if index >= len {
                                index = len - 1;
                            }

                            let mut new_offset = list_state.offset();
                            if index < new_offset {
                                new_offset = index;
                            } else if height > 0 && index >= new_offset + height {
                                new_offset = index + 1 - height;
                            }

                            list_state.select(Some(index));
                            *list_state.offset_mut() = new_offset;
                        };

                    handle_list_click(
                        layout.tracks,
                        &mut search_state.track_list,
                        filtered_tracks.len(),
                        SearchFocusState::Tracks,
                    );
                    handle_list_click(
                        layout.albums,
                        &mut search_state.album_list,
                        filtered_albums.len(),
                        SearchFocusState::Albums,
                    );
                    handle_list_click(
                        layout.artists,
                        &mut search_state.artist_list,
                        filtered_artists.len(),
                        SearchFocusState::Artists,
                    );
                    handle_list_click(
                        layout.playlists,
                        &mut search_state.playlist_list,
                        filtered_playlists.len(),
                        SearchFocusState::Playlists,
                    );
                    handle_list_click(
                        layout.shows,
                        &mut search_state.show_list,
                        filtered_shows.len(),
                        SearchFocusState::Shows,
                    );
                    handle_list_click(
                        layout.episodes,
                        &mut search_state.episode_list,
                        filtered_episodes.len(),
                        SearchFocusState::Episodes,
                    );

                    if handled {
                        if let Some(focus) = clicked_focus {
                            let (category, len) = match focus {
                                SearchFocusState::Tracks => {
                                    (SearchResultCategory::Tracks, filtered_tracks.len())
                                }
                                SearchFocusState::Albums => {
                                    (SearchResultCategory::Albums, filtered_albums.len())
                                }
                                SearchFocusState::Artists => {
                                    (SearchResultCategory::Artists, filtered_artists.len())
                                }
                                SearchFocusState::Playlists => {
                                    (SearchResultCategory::Playlists, filtered_playlists.len())
                                }
                                SearchFocusState::Shows => {
                                    (SearchResultCategory::Shows, filtered_shows.len())
                                }
                                SearchFocusState::Episodes => {
                                    (SearchResultCategory::Episodes, filtered_episodes.len())
                                }
                                SearchFocusState::Input => (SearchResultCategory::Tracks, 0),
                            };
                            if focus != SearchFocusState::Input {
                                maybe_queue_search_prefetch(
                                    &mut ui,
                                    &data,
                                    &current_query,
                                    category,
                                    len,
                                );
                            }
                        }

                        if is_double_click {
                            if let Some(focus) = clicked_focus {
                                match focus {
                                    SearchFocusState::Tracks => {
                                        let _ = window::handle_command_for_track_list_window(
                                            Command::ChooseSelected,
                                            client_pub,
                                            &filtered_tracks,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                    SearchFocusState::Albums => {
                                        let _ = window::handle_command_for_album_list_window(
                                            Command::ChooseSelected,
                                            &filtered_albums,
                                            &data,
                                            &mut ui,
                                            client_pub,
                                        );
                                    }
                                    SearchFocusState::Artists => {
                                        window::handle_command_for_artist_list_window(
                                            Command::ChooseSelected,
                                            &filtered_artists,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                    SearchFocusState::Playlists => {
                                        window::handle_command_for_playlist_list_window(
                                            Command::ChooseSelected,
                                            &filtered_playlists,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                    SearchFocusState::Shows => {
                                        window::handle_command_for_show_list_window(
                                            Command::ChooseSelected,
                                            &filtered_shows,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                    SearchFocusState::Episodes => {
                                        let _ = window::handle_command_for_episode_list_window(
                                            Command::ChooseSelected,
                                            client_pub,
                                            &filtered_episodes,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                    SearchFocusState::Input => {}
                                }
                            }
                            ui.count_prefix = None;
                            return Ok(());
                        }

                        ui.count_prefix = None;
                        return Ok(());
                    }
                    drop(data);
                }
            }

            // Handle library page list selection
            if let PageState::Library { .. } = ui.current_page() {
                let layout = ui.library_layout;
                if layout.valid {
                    let playlist_folder_id = match ui.current_page() {
                        PageState::Library { state } => state.playlist_folder_id,
                        _ => unreachable!(),
                    };

                    let data = state.data.read();
                    let folder_items = data.user_data.folder_playlists_items(playlist_folder_id);
                    let filtered_playlists = ui
                        .search_filtered_items(&folder_items)
                        .into_iter()
                        .map(|item| *item)
                        .collect::<Vec<&PlaylistFolderItem>>();
                    let filtered_albums = ui.search_filtered_items(&data.user_data.saved_albums);
                    let filtered_artists =
                        ui.search_filtered_items(&data.user_data.followed_artists);

                    let PageState::Library { state: page_state } = ui.current_page_mut() else {
                        unreachable!();
                    };

                    let mut handled = false;
                    let mut clicked_focus: Option<LibraryFocusState> = None;

                    let within = |rect: ratatui::layout::Rect| -> bool {
                        event.column >= rect.x
                            && event.column < rect.x.saturating_add(rect.width)
                            && event.row >= rect.y
                            && event.row < rect.y.saturating_add(rect.height)
                    };

                    let mut handle_list_click =
                        |rect: ratatui::layout::Rect,
                         list_state: &mut ratatui::widgets::ListState,
                         len: usize,
                         focus: LibraryFocusState| {
                            if !within(rect) {
                                return;
                            }

                            page_state.focus = focus;
                            handled = true;
                            clicked_focus = Some(focus);

                            if len == 0 || rect.height == 0 {
                                return;
                            }

                            let relative = event.row.saturating_sub(rect.y) as usize;
                            let height = rect.height as usize;
                            if relative >= height {
                                return;
                            }

                            let mut index = list_state.offset().saturating_add(relative);
                            if index >= len {
                                index = len - 1;
                            }

                            let mut new_offset = list_state.offset();
                            if index < new_offset {
                                new_offset = index;
                            } else if height > 0 && index >= new_offset + height {
                                new_offset = index + 1 - height;
                            }

                            list_state.select(Some(index));
                            *list_state.offset_mut() = new_offset;
                        };

                    handle_list_click(
                        layout.playlists,
                        &mut page_state.playlist_list,
                        filtered_playlists.len(),
                        LibraryFocusState::Playlists,
                    );
                    handle_list_click(
                        layout.albums,
                        &mut page_state.saved_album_list,
                        filtered_albums.len(),
                        LibraryFocusState::SavedAlbums,
                    );
                    handle_list_click(
                        layout.artists,
                        &mut page_state.followed_artist_list,
                        filtered_artists.len(),
                        LibraryFocusState::FollowedArtists,
                    );

                    if handled {
                        if is_double_click {
                            if let Some(focus) = clicked_focus {
                                match focus {
                                    LibraryFocusState::Playlists => {
                                        let _ = window::handle_command_for_playlist_list_window(
                                            Command::ChooseSelected,
                                            &filtered_playlists,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                    LibraryFocusState::SavedAlbums => {
                                        let _ = window::handle_command_for_album_list_window(
                                            Command::ChooseSelected,
                                            &filtered_albums,
                                            &data,
                                            &mut ui,
                                            client_pub,
                                        );
                                    }
                                    LibraryFocusState::FollowedArtists => {
                                        window::handle_command_for_artist_list_window(
                                            Command::ChooseSelected,
                                            &filtered_artists,
                                            &data,
                                            &mut ui,
                                        );
                                    }
                                }
                            }
                            ui.count_prefix = None;
                            return Ok(());
                        }

                        ui.count_prefix = None;
                        return Ok(());
                    }
                    drop(data);
                }
            }

            let within = |rect: ratatui::layout::Rect| -> bool {
                event.column >= rect.x
                    && event.column < rect.x.saturating_add(rect.width)
                    && event.row >= rect.y
                    && event.row < rect.y.saturating_add(rect.height)
            };

            if let Some(track_rect) = ui.context_track_table_rect {
                if within(track_rect) {
                    let relative = event.row.saturating_sub(track_rect.y);
                    if relative > 0 {
                        let row_in_view = (relative - 1) as usize;
                        let context_info = || -> Option<(ContextId, ContextTrackSource, usize)> {
                            if let PageState::Context {
                                id: Some(context_id),
                                state: Some(context_state),
                                ..
                            } = ui.current_page()
                            {
                                match context_state {
                                    ContextPageUIState::Album { track_table } => Some((
                                        context_id.clone(),
                                        ContextTrackSource::Album,
                                        track_table.offset(),
                                    )),
                                    ContextPageUIState::Playlist { track_table } => Some((
                                        context_id.clone(),
                                        ContextTrackSource::Playlist,
                                        track_table.offset(),
                                    )),
                                    ContextPageUIState::Tracks { track_table } => Some((
                                        context_id.clone(),
                                        ContextTrackSource::Tracks,
                                        track_table.offset(),
                                    )),
                                    ContextPageUIState::Artist {
                                        top_track_table, ..
                                    } => Some((
                                        context_id.clone(),
                                        ContextTrackSource::ArtistTopTracks,
                                        top_track_table.offset(),
                                    )),
                                    ContextPageUIState::Show { .. } => None,
                                }
                            } else {
                                None
                            }
                        };

                        if let Some((context_id_clone, source, offset)) = context_info() {
                            let mut handled_context = false;
                            {
                                let data = state.data.read();
                                let context_uri = context_id_clone.uri();
                                if let Some(context) = data.caches.context.get(&context_uri) {
                                    let tracks_slice = match (source, context) {
                                        (
                                            ContextTrackSource::Album,
                                            Context::Album { tracks, .. },
                                        ) => Some(tracks.as_slice()),
                                        (
                                            ContextTrackSource::Playlist,
                                            Context::Playlist { tracks, .. },
                                        ) => Some(tracks.as_slice()),
                                        (
                                            ContextTrackSource::Tracks,
                                            Context::Tracks { tracks, .. },
                                        ) => Some(tracks.as_slice()),
                                        (
                                            ContextTrackSource::ArtistTopTracks,
                                            Context::Artist { top_tracks, .. },
                                        ) => Some(top_tracks.as_slice()),
                                        _ => None,
                                    };

                                    if let Some(tracks_slice) = tracks_slice {
                                        let filtered_tracks =
                                            ui.search_filtered_items(tracks_slice);
                                        let target_index = offset.saturating_add(row_in_view);
                                        if target_index < filtered_tracks.len() {
                                            if let PageState::Context {
                                                state: Some(context_state_mut),
                                                ..
                                            } = ui.current_page_mut()
                                            {
                                                match (context_state_mut, source) {
                                                    (
                                                        ContextPageUIState::Album { track_table },
                                                        ContextTrackSource::Album,
                                                    ) => {
                                                        track_table.select(Some(target_index));
                                                    }
                                                    (
                                                        ContextPageUIState::Playlist {
                                                            track_table,
                                                        },
                                                        ContextTrackSource::Playlist,
                                                    ) => {
                                                        track_table.select(Some(target_index));
                                                    }
                                                    (
                                                        ContextPageUIState::Tracks { track_table },
                                                        ContextTrackSource::Tracks,
                                                    ) => {
                                                        track_table.select(Some(target_index));
                                                    }
                                                    (
                                                        ContextPageUIState::Artist {
                                                            top_track_table,
                                                            focus,
                                                            ..
                                                        },
                                                        ContextTrackSource::ArtistTopTracks,
                                                    ) => {
                                                        *focus = ArtistFocusState::TopTracks;
                                                        top_track_table.select(Some(target_index));
                                                    }
                                                    _ => {}
                                                }
                                            }

                                            ui.count_prefix = None;

                                            if is_double_click {
                                                let _ = window::handle_command_for_focused_context_window(
                                                    Command::ChooseSelected,
                                                    client_pub,
                                                    &mut ui,
                                                    state,
                                                );
                                            }

                                            handled_context = true;
                                        }
                                    }
                                }
                            }
                            if handled_context {
                                return Ok(());
                            }
                        }
                    }
                }
            }

            Ok(())
        }
        MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
            let mut ui = state.ui.lock();
            let command = if matches!(event.kind, MouseEventKind::ScrollDown) {
                Command::PageSelectNextOrScrollDown
            } else {
                Command::PageSelectPreviousOrScrollUp
            };
            let within = |rect: ratatui::layout::Rect| -> bool {
                event.column >= rect.x
                    && event.column < rect.x.saturating_add(rect.width)
                    && event.row >= rect.y
                    && event.row < rect.y.saturating_add(rect.height)
            };

            if let Some(track_rect) = ui.context_track_table_rect {
                if within(track_rect) {
                    let source = if let PageState::Context {
                        state: Some(context_state),
                        ..
                    } = ui.current_page()
                    {
                        match context_state {
                            ContextPageUIState::Album { .. } => Some(ContextTrackSource::Album),
                            ContextPageUIState::Playlist { .. } => {
                                Some(ContextTrackSource::Playlist)
                            }
                            ContextPageUIState::Tracks { .. } => Some(ContextTrackSource::Tracks),
                            ContextPageUIState::Artist { .. } => {
                                Some(ContextTrackSource::ArtistTopTracks)
                            }
                            ContextPageUIState::Show { .. } => None,
                        }
                    } else {
                        None
                    };

                    if let Some(source) = source {
                        if let Some(required_focus) = source.required_focus() {
                            if let PageState::Context {
                                state: Some(ContextPageUIState::Artist { focus, .. }),
                                ..
                            } = ui.current_page_mut()
                            {
                                *focus = required_focus;
                            }
                        }

                        if window::handle_command_for_focused_context_window(
                            command, client_pub, &mut ui, state,
                        )? {
                            ui.count_prefix = None;
                            return Ok(());
                        }
                    }
                }
            }

            if let PageState::Search { .. } = ui.current_page() {
                let layout = ui.search_layout;
                if layout.valid {
                    let within = |rect: ratatui::layout::Rect| -> bool {
                        event.column >= rect.x
                            && event.column < rect.x.saturating_add(rect.width)
                            && event.row >= rect.y
                            && event.row < rect.y.saturating_add(rect.height)
                    };

                    let target_focus = if within(layout.tracks) {
                        Some(SearchFocusState::Tracks)
                    } else if within(layout.albums) {
                        Some(SearchFocusState::Albums)
                    } else if within(layout.artists) {
                        Some(SearchFocusState::Artists)
                    } else if within(layout.playlists) {
                        Some(SearchFocusState::Playlists)
                    } else if within(layout.shows) {
                        Some(SearchFocusState::Shows)
                    } else if within(layout.episodes) {
                        Some(SearchFocusState::Episodes)
                    } else {
                        None
                    };

                    if let Some(focus) = target_focus {
                        let current_query = match ui.current_page() {
                            PageState::Search { current_query, .. } => current_query.clone(),
                            _ => unreachable!(),
                        };

                        if let PageState::Search { state, .. } = ui.current_page_mut() {
                            state.focus = focus;
                        }

                        let data = state.data.read();
                        let search_results = data.caches.search.get(&current_query);

                        let filtered_tracks = search_results
                            .map(|s| ui.search_filtered_items(&s.results.tracks))
                            .unwrap_or_default();
                        let filtered_albums = search_results
                            .map(|s| ui.search_filtered_items(&s.results.albums))
                            .unwrap_or_default();
                        let filtered_artists = search_results
                            .map(|s| ui.search_filtered_items(&s.results.artists))
                            .unwrap_or_default();
                        let playlist_items: Vec<PlaylistFolderItem> = search_results
                            .map(|s| {
                                ui.search_filtered_items(&s.results.playlists)
                                    .into_iter()
                                    .map(|p| PlaylistFolderItem::Playlist(p.clone()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let filtered_playlists: Vec<&PlaylistFolderItem> =
                            playlist_items.iter().collect();
                        let filtered_shows = search_results
                            .map(|s| ui.search_filtered_items(&s.results.shows))
                            .unwrap_or_default();
                        let filtered_episodes = search_results
                            .map(|s| ui.search_filtered_items(&s.results.episodes))
                            .unwrap_or_default();

                        let handled = match focus {
                            SearchFocusState::Tracks => {
                                window::handle_command_for_track_list_window(
                                    command,
                                    client_pub,
                                    &filtered_tracks,
                                    &data,
                                    &mut ui,
                                )?
                            }
                            SearchFocusState::Albums => {
                                window::handle_command_for_album_list_window(
                                    command,
                                    &filtered_albums,
                                    &data,
                                    &mut ui,
                                    client_pub,
                                )?
                            }
                            SearchFocusState::Artists => {
                                window::handle_command_for_artist_list_window(
                                    command,
                                    &filtered_artists,
                                    &data,
                                    &mut ui,
                                )
                            }
                            SearchFocusState::Playlists => {
                                window::handle_command_for_playlist_list_window(
                                    command,
                                    &filtered_playlists,
                                    &data,
                                    &mut ui,
                                )
                            }
                            SearchFocusState::Shows => window::handle_command_for_show_list_window(
                                command,
                                &filtered_shows,
                                &data,
                                &mut ui,
                            ),
                            SearchFocusState::Episodes => {
                                window::handle_command_for_episode_list_window(
                                    command,
                                    client_pub,
                                    &filtered_episodes,
                                    &data,
                                    &mut ui,
                                )?
                            }
                            SearchFocusState::Input => false,
                        };

                        if handled {
                            ui.count_prefix = None;
                            return Ok(());
                        }
                    }
                }
            }

            if let PageState::Library { .. } = ui.current_page() {
                let layout = ui.library_layout;
                if layout.valid {
                    let within = |rect: ratatui::layout::Rect| -> bool {
                        event.column >= rect.x
                            && event.column < rect.x.saturating_add(rect.width)
                            && event.row >= rect.y
                            && event.row < rect.y.saturating_add(rect.height)
                    };

                    let target_focus = if within(layout.playlists) {
                        Some(LibraryFocusState::Playlists)
                    } else if within(layout.albums) {
                        Some(LibraryFocusState::SavedAlbums)
                    } else if within(layout.artists) {
                        Some(LibraryFocusState::FollowedArtists)
                    } else {
                        None
                    };

                    if let Some(focus) = target_focus {
                        let playlist_folder_id = match ui.current_page() {
                            PageState::Library { state } => state.playlist_folder_id,
                            _ => unreachable!(),
                        };

                        if let PageState::Library { state } = ui.current_page_mut() {
                            state.focus = focus;
                        }

                        let data = state.data.read();
                        let folder_items =
                            data.user_data.folder_playlists_items(playlist_folder_id);
                        let filtered_playlists = ui
                            .search_filtered_items(&folder_items)
                            .into_iter()
                            .copied()
                            .collect::<Vec<_>>();
                        let filtered_playlists_refs =
                            filtered_playlists.iter().copied().collect::<Vec<_>>();
                        let filtered_albums =
                            ui.search_filtered_items(&data.user_data.saved_albums);
                        let filtered_artists =
                            ui.search_filtered_items(&data.user_data.followed_artists);

                        let handled = match focus {
                            LibraryFocusState::Playlists => {
                                window::handle_command_for_playlist_list_window(
                                    command,
                                    &filtered_playlists_refs,
                                    &data,
                                    &mut ui,
                                )
                            }
                            LibraryFocusState::SavedAlbums => {
                                window::handle_command_for_album_list_window(
                                    command,
                                    &filtered_albums,
                                    &data,
                                    &mut ui,
                                    client_pub,
                                )?
                            }
                            LibraryFocusState::FollowedArtists => {
                                window::handle_command_for_artist_list_window(
                                    command,
                                    &filtered_artists,
                                    &data,
                                    &mut ui,
                                )
                            }
                        };

                        if handled {
                            ui.count_prefix = None;
                            return Ok(());
                        }
                    }
                }
            }

            Ok(())
        }
        _ => Ok(()),
    }
}

// Handle a terminal key pressed event
fn handle_key_event(
    event: crossterm::event::KeyEvent,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
) -> Result<()> {
    let key: Key = event.into();
    let mut ui = state.ui.lock();

    if event.code == KeyCode::Esc && event.modifiers.is_empty() {
        if ui.popup.is_some() {
            ui.popup = None;
            ui.input_key_sequence.keys.clear();
            ui.count_prefix = None;
            return Ok(());
        }
        if ui.history.len() > 1 {
            ui.history.pop();
            ui.popup = None;
            ui.input_key_sequence.keys.clear();
            ui.count_prefix = None;
            return Ok(());
        }
    }

    let mut key_sequence = ui.input_key_sequence.clone();
    key_sequence.keys.push(key);

    // check if the current key sequence matches any keymap's prefix
    // if not, reset the key sequence
    let keymap_config = &config::get_config().keymap_config;
    if !keymap_config.has_matched_prefix(&key_sequence) {
        key_sequence = KeySequence { keys: vec![key] };
    }

    tracing::debug!(
        "Handling key event: {event:?}, current key sequence: {key_sequence:?}, count prefix: {:?}",
        ui.count_prefix
    );
    let handled = {
        if ui.popup.is_none() {
            page::handle_key_sequence_for_page(&key_sequence, client_pub, state, &mut ui)?
        } else {
            popup::handle_key_sequence_for_popup(&key_sequence, client_pub, state, &mut ui)?
        }
    };

    // if the key sequence is not handled, let the global handler handle it
    let handled = if handled {
        true
    } else {
        match keymap_config.find_command_or_action_from_key_sequence(&key_sequence) {
            Some(CommandOrAction::Action(action, target)) => {
                handle_global_action(action, target, client_pub, state, &mut ui)?
            }
            Some(CommandOrAction::Command(command)) => {
                handle_global_command(command, client_pub, state, &mut ui)?
            }
            None => false,
        }
    };

    // if handled, clear the key sequence and count prefix
    // otherwise, the current key sequence can be a prefix of a command's shortcut
    if handled {
        ui.input_key_sequence.keys = vec![];
        ui.count_prefix = None;
    } else {
        // update the count prefix if the key is a digit
        match key {
            Key::None(KeyCode::Char(c)) if c.is_ascii_digit() => {
                let digit = c.to_digit(10).unwrap() as usize;
                ui.input_key_sequence.keys = vec![];
                ui.count_prefix = match ui.count_prefix {
                    Some(count) => Some(count * 10 + digit),
                    None => {
                        if digit > 0 {
                            Some(digit)
                        } else {
                            None
                        }
                    }
                };
            }
            _ => {
                ui.input_key_sequence = key_sequence;
                ui.count_prefix = None;
            }
        }
    }

    let pending_requests = std::mem::take(&mut ui.pending_client_requests);

    drop(ui);

    process_pending_client_requests(pending_requests, state, client_pub)?;

    Ok(())
}

fn process_pending_client_requests(
    requests: Vec<PendingClientRequest>,
    state: &SharedState,
    client_pub: &flume::Sender<ClientRequest>,
) -> Result<()> {
    for request in requests {
        match request {
            PendingClientRequest::SearchMore { query, category } => {
                let maybe_offset = {
                    let mut data = state.data.write();
                    if let Some(mut entry) = data.caches.search.remove(&query) {
                        let info = entry.pagination.info_mut(category);
                        if info.is_fetching || info.is_exhausted {
                            data.caches
                                .search
                                .insert(query.clone(), entry, *TTL_CACHE_DURATION);
                            None
                        } else {
                            let offset = entry.results.len_for_category(category);
                            info.is_fetching = true;
                            data.caches
                                .search
                                .insert(query.clone(), entry, *TTL_CACHE_DURATION);
                            Some(offset)
                        }
                    } else {
                        None
                    }
                };

                if let Some(offset) = maybe_offset {
                    client_pub.send(ClientRequest::SearchMore {
                        query,
                        category,
                        offset,
                    })?;
                }
            }
        }
    }
    Ok(())
}

pub fn handle_action_in_context(
    action: Action,
    context: ActionContext,
    client_pub: &flume::Sender<ClientRequest>,
    data: &DataReadGuard,
    ui: &mut UIStateGuard,
) -> Result<bool> {
    match context {
        ActionContext::Track(track) => match action {
            Action::GoToAlbum => {
                if let Some(album) = track.album {
                    let context_id = ContextId::Album(
                        AlbumId::from_uri(&parse_uri(&album.id.uri()))?.into_static(),
                    );
                    ui.new_page(PageState::Context {
                        id: None,
                        context_page_type: ContextPageType::Browsing(context_id),
                        state: None,
                    });
                    return Ok(true);
                }
                Ok(false)
            }
            Action::GoToArtist => {
                handle_go_to_artist(track.artists, ui);
                Ok(true)
            }
            Action::AddToQueue => {
                client_pub.send(ClientRequest::AddPlayableToQueue(track.id.into()))?;
                ui.popup = None;
                Ok(true)
            }
            Action::CopyLink => {
                let track_url = format!("https://open.spotify.com/track/{}", track.id.id());
                execute_copy_command(track_url)?;
                ui.popup = None;
                Ok(true)
            }
            Action::AddToPlaylist => {
                client_pub.send(ClientRequest::GetUserPlaylists)?;
                ui.popup = Some(PopupState::UserPlaylistList(
                    PlaylistPopupAction::AddTrack {
                        folder_id: 0,
                        track_id: track.id,
                        search_query: String::new(),
                    },
                    ListState::default(),
                ));
                Ok(true)
            }
            Action::ToggleLiked => {
                if data.user_data.is_liked_track(&track) {
                    tracing::info!(
                        "DeleteFromLibrary (toggle) for track '{}' ({})",
                        track.name,
                        track.id.uri()
                    );
                    client_pub.send(ClientRequest::DeleteFromLibrary(ItemId::Track(track.id)))?;
                } else {
                    client_pub.send(ClientRequest::AddToLibrary(Item::Track(track)))?;
                }
                ui.popup = None;
                Ok(true)
            }
            Action::AddToLiked => {
                client_pub.send(ClientRequest::AddToLibrary(Item::Track(track)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::DeleteFromLiked => {
                tracing::info!(
                    "DeleteFromLibrary for track '{}' ({})",
                    track.name,
                    track.id.uri()
                );
                client_pub.send(ClientRequest::DeleteFromLibrary(ItemId::Track(track.id)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::GoToRadio => {
                let uri = track.id.uri();
                let name = track.name;
                ui.new_radio_page(&uri);
                client_pub.send(ClientRequest::GetRadioTracks {
                    seed_uri: uri,
                    seed_name: name,
                })?;
                Ok(true)
            }
            Action::ShowActionsOnArtist => {
                handle_show_actions_on_artist(track.artists, data, ui);
                Ok(true)
            }
            Action::ShowActionsOnAlbum => {
                if let Some(album) = track.album {
                    let context = ActionContext::Album(album.clone());
                    ui.popup = Some(PopupState::ActionList(
                        Box::new(ActionListItem::Album(
                            album,
                            context.get_available_actions(data),
                        )),
                        ListState::default(),
                    ));
                    return Ok(true);
                }
                Ok(false)
            }
            Action::DeleteFromPlaylist => {
                if let PageState::Context {
                    id: Some(ContextId::Playlist(playlist_id)),
                    ..
                } = ui.current_page()
                {
                    client_pub.send(ClientRequest::DeleteTrackFromPlaylist(
                        playlist_id.clone_static(),
                        track.id,
                    ))?;
                }
                ui.popup = None;
                Ok(true)
            }
            _ => Ok(false),
        },
        ActionContext::Album(album) => match action {
            Action::GoToArtist => {
                handle_go_to_artist(album.artists, ui);
                Ok(true)
            }
            Action::GoToRadio => {
                let uri = album.id.uri();
                let name = album.name;
                ui.new_radio_page(&uri);
                client_pub.send(ClientRequest::GetRadioTracks {
                    seed_uri: uri,
                    seed_name: name,
                })?;
                Ok(true)
            }
            Action::ShowActionsOnArtist => {
                handle_show_actions_on_artist(album.artists, data, ui);
                Ok(true)
            }
            Action::AddToLibrary => {
                client_pub.send(ClientRequest::AddToLibrary(Item::Album(album)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::DeleteFromLibrary => {
                if !data
                    .user_data
                    .saved_albums
                    .iter()
                    .any(|saved| saved.id == album.id)
                {
                    tracing::debug!(
                        "Skip deleting album '{}' ({}) because it is not saved in library",
                        album.name,
                        album.id.uri()
                    );
                    return Ok(true);
                }
                client_pub.send(ClientRequest::DeleteFromLibrary(ItemId::Album(album.id)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::CopyLink => {
                let album_url = format!("https://open.spotify.com/album/{}", album.id.id());
                execute_copy_command(album_url)?;
                ui.popup = None;
                Ok(true)
            }
            Action::AddToQueue => {
                client_pub.send(ClientRequest::AddAlbumToQueue(album.id))?;
                ui.popup = None;
                Ok(true)
            }
            _ => Ok(false),
        },
        ActionContext::Artist(artist) => match action {
            Action::Follow => {
                client_pub.send(ClientRequest::AddToLibrary(Item::Artist(artist)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::Unfollow => {
                if !data
                    .user_data
                    .followed_artists
                    .iter()
                    .any(|saved| saved.id == artist.id)
                {
                    tracing::debug!(
                        "Skip unfollowing artist '{}' ({}) because it is not followed",
                        artist.name,
                        artist.id.uri()
                    );
                    return Ok(true);
                }
                client_pub.send(ClientRequest::DeleteFromLibrary(ItemId::Artist(artist.id)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::CopyLink => {
                let artist_url = format!("https://open.spotify.com/artist/{}", artist.id.id());
                execute_copy_command(artist_url)?;
                ui.popup = None;
                Ok(true)
            }
            Action::GoToRadio => {
                let uri = artist.id.uri();
                let name = artist.name;
                ui.new_radio_page(&uri);
                client_pub.send(ClientRequest::GetRadioTracks {
                    seed_uri: uri,
                    seed_name: name,
                })?;
                Ok(true)
            }
            _ => Ok(false),
        },
        ActionContext::Playlist(playlist) => match action {
            Action::AddToLibrary => {
                client_pub.send(ClientRequest::AddToLibrary(Item::Playlist(playlist)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::GoToRadio => {
                let uri = playlist.id.uri();
                let name = playlist.name;
                ui.new_radio_page(&uri);
                client_pub.send(ClientRequest::GetRadioTracks {
                    seed_uri: uri,
                    seed_name: name,
                })?;
                Ok(true)
            }
            Action::CopyLink => {
                let playlist_url =
                    format!("https://open.spotify.com/playlist/{}", playlist.id.id());
                execute_copy_command(playlist_url)?;
                ui.popup = None;
                Ok(true)
            }
            Action::DeleteFromLibrary => {
                let is_saved = data.user_data.playlists.iter().any(
                    |item| matches!(item, PlaylistFolderItem::Playlist(p) if p.id == playlist.id),
                );
                if !is_saved {
                    tracing::debug!(
                        "Skip deleting playlist '{}' ({}) because it is not saved in library",
                        playlist.name,
                        playlist.id.uri()
                    );
                    return Ok(true);
                }
                client_pub.send(ClientRequest::DeleteFromLibrary(ItemId::Playlist(
                    playlist.id,
                )))?;
                ui.popup = None;
                Ok(true)
            }
            _ => Ok(false),
        },
        ActionContext::Show(show) => match action {
            Action::CopyLink => {
                let show_url = format!("https://open.spotify.com/show/{}", show.id.id());
                execute_copy_command(show_url)?;
                ui.popup = None;
                Ok(true)
            }
            Action::AddToLibrary => {
                client_pub.send(ClientRequest::AddToLibrary(Item::Show(show)))?;
                ui.popup = None;
                Ok(true)
            }
            Action::DeleteFromLibrary => {
                if !data
                    .user_data
                    .saved_shows
                    .iter()
                    .any(|saved| saved.id == show.id)
                {
                    tracing::debug!(
                        "Skip deleting show '{}' ({}) because it is not saved in library",
                        show.name,
                        show.id.uri()
                    );
                    return Ok(true);
                }
                client_pub.send(ClientRequest::DeleteFromLibrary(ItemId::Show(show.id)))?;
                ui.popup = None;
                Ok(true)
            }
            _ => Ok(false),
        },
        ActionContext::Episode(episode) => match action {
            Action::GoToShow => {
                if let Some(show) = episode.show {
                    let context_id = ContextId::Show(
                        ShowId::from_uri(&parse_uri(&show.id.uri()))?.into_static(),
                    );
                    ui.new_page(PageState::Context {
                        id: None,
                        context_page_type: ContextPageType::Browsing(context_id),
                        state: None,
                    });
                    return Ok(true);
                }
                Ok(false)
            }
            Action::AddToQueue => {
                client_pub.send(ClientRequest::AddPlayableToQueue(episode.id.into()))?;
                ui.popup = None;
                Ok(true)
            }
            Action::CopyLink => {
                let episode_url = format!("https://open.spotify.com/episode/{}", episode.id.id());
                execute_copy_command(episode_url)?;
                ui.popup = None;
                Ok(true)
            }
            Action::AddToPlaylist => {
                client_pub.send(ClientRequest::GetUserPlaylists)?;
                ui.popup = Some(PopupState::UserPlaylistList(
                    PlaylistPopupAction::AddEpisode {
                        folder_id: 0,
                        episode_id: episode.id,
                        search_query: String::new(),
                    },
                    ListState::default(),
                ));
                Ok(true)
            }
            Action::ShowActionsOnShow => {
                if let Some(show) = episode.show {
                    let context = ActionContext::Show(show.clone());
                    ui.popup = Some(PopupState::ActionList(
                        Box::new(ActionListItem::Show(
                            show,
                            context.get_available_actions(data),
                        )),
                        ListState::default(),
                    ));
                    return Ok(true);
                }
                Ok(false)
            }
            _ => Ok(false),
        },
        // TODO: support actions for playlist folders
        ActionContext::PlaylistFolder(_) => Ok(false),
    }
}

fn handle_go_to_artist(artists: Vec<Artist>, ui: &mut UIStateGuard) {
    if artists.len() == 1 {
        let context_id = ContextId::Artist(artists[0].id.clone());
        ui.new_page(PageState::Context {
            id: None,
            context_page_type: ContextPageType::Browsing(context_id),
            state: None,
        });
    } else {
        ui.popup = Some(PopupState::ArtistList(
            ArtistPopupAction::Browse,
            artists,
            ListState::default(),
        ));
    }
}

fn handle_show_actions_on_artist(
    artists: Vec<Artist>,
    data: &DataReadGuard,
    ui: &mut UIStateGuard,
) {
    if artists.len() == 1 {
        let actions = construct_artist_actions(&artists[0], data);
        ui.popup = Some(PopupState::ActionList(
            Box::new(ActionListItem::Artist(artists[0].clone(), actions)),
            ListState::default(),
        ));
    } else {
        ui.popup = Some(PopupState::ArtistList(
            ArtistPopupAction::ShowActions,
            artists,
            ListState::default(),
        ));
    }
}

/// Handle a global action, currently this is only used to target
/// the currently playing item instead of the selection.
fn handle_global_action(
    action: Action,
    target: ActionTarget,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
    ui: &mut UIStateGuard,
) -> Result<bool> {
    if target == ActionTarget::PlayingTrack {
        let player = state.player.read();
        let data = state.data.read();

        if let Some(currently_playing) = player.currently_playing() {
            match currently_playing {
                rspotify::model::PlayableItem::Track(track) => {
                    if let Some(track) = Track::try_from_full_track(track.clone()) {
                        return handle_action_in_context(
                            action,
                            ActionContext::Track(track),
                            client_pub,
                            &data,
                            ui,
                        );
                    }
                }
                rspotify::model::PlayableItem::Episode(episode) => {
                    return handle_action_in_context(
                        action,
                        ActionContext::Episode(episode.clone().into()),
                        client_pub,
                        &data,
                        ui,
                    );
                }
                rspotify::model::PlayableItem::Unknown(_) => {
                    return Ok(false);
                }
            }
        }
    }

    Ok(false)
}

/// Handle a global command that is not specific to any page/popup
fn handle_global_command(
    command: Command,
    client_pub: &flume::Sender<ClientRequest>,
    state: &SharedState,
    ui: &mut UIStateGuard,
) -> Result<bool> {
    match command {
        Command::Quit => {
            ui.is_running = false;
        }
        Command::NextTrack => {
            client_pub.send(ClientRequest::Player(PlayerRequest::NextTrack))?;
        }
        Command::PreviousTrack => {
            client_pub.send(ClientRequest::Player(PlayerRequest::PreviousTrack))?;
        }
        Command::ResumePause => {
            client_pub.send(ClientRequest::Player(PlayerRequest::ResumePause))?;
        }
        Command::Repeat => {
            client_pub.send(ClientRequest::Player(PlayerRequest::Repeat))?;
        }
        Command::ToggleFakeTrackRepeatMode => {
            let mut player = state.player.write();
            if let Some(playback) = &mut player.buffered_playback {
                playback.fake_track_repeat_state = !playback.fake_track_repeat_state;
            }
        }
        Command::Shuffle => {
            client_pub.send(ClientRequest::Player(PlayerRequest::Shuffle))?;
        }
        Command::VolumeChange { offset } => {
            if let Some(ref playback) = state.player.read().buffered_playback {
                if let Some(volume) = playback.volume {
                    let volume = std::cmp::min(volume as i32 + offset, 100_i32);
                    client_pub.send(ClientRequest::Player(PlayerRequest::Volume(volume as u8)))?;
                }
            }
        }
        Command::Mute => {
            client_pub.send(ClientRequest::Player(PlayerRequest::ToggleMute))?;
        }
        Command::SeekForward { duration } => {
            if let Some(progress) = state.player.read().playback_progress() {
                let duration =
                    duration.unwrap_or(config::get_config().app_config.seek_duration_secs);
                client_pub.send(ClientRequest::Player(PlayerRequest::SeekTrack(
                    progress + chrono::Duration::try_seconds(i64::from(duration)).unwrap(),
                )))?;
            }
        }
        Command::SeekBackward { duration } => {
            if let Some(progress) = state.player.read().playback_progress() {
                let duration =
                    duration.unwrap_or(config::get_config().app_config.seek_duration_secs);
                client_pub.send(ClientRequest::Player(PlayerRequest::SeekTrack(
                    std::cmp::max(
                        chrono::Duration::zero(),
                        progress - chrono::Duration::try_seconds(i64::from(duration)).unwrap(),
                    ),
                )))?;
            }
        }
        Command::OpenCommandHelp => {
            ui.new_page(PageState::CommandHelp { scroll_offset: 0 });
        }
        Command::RefreshPlayback => {
            client_pub.send(ClientRequest::GetCurrentPlayback)?;
        }
        Command::ShowActionsOnCurrentTrack => {
            if let Some(currently_playing) = state.player.read().currently_playing() {
                match currently_playing {
                    rspotify::model::PlayableItem::Track(track) => {
                        if let Some(track) = Track::try_from_full_track(track.clone()) {
                            let data = state.data.read();
                            let actions = command::construct_track_actions(&track, &data);
                            ui.popup = Some(PopupState::ActionList(
                                Box::new(ActionListItem::Track(track, actions)),
                                ListState::default(),
                            ));
                        }
                    }
                    rspotify::model::PlayableItem::Episode(episode) => {
                        let episode = episode.clone().into();
                        let data = state.data.read();
                        let actions = command::construct_episode_actions(&episode, &data);
                        ui.popup = Some(PopupState::ActionList(
                            Box::new(ActionListItem::Episode(episode, actions)),
                            ListState::default(),
                        ));
                    }
                    rspotify::model::PlayableItem::Unknown(_) => {}
                }
            }
        }
        Command::CurrentlyPlayingContextPage => {
            ui.new_page(PageState::Context {
                id: None,
                context_page_type: ContextPageType::CurrentPlaying,
                state: None,
            });
        }
        Command::BrowseUserPlaylists => {
            client_pub.send(ClientRequest::GetUserPlaylists)?;
            ui.popup = Some(PopupState::UserPlaylistList(
                PlaylistPopupAction::Browse {
                    folder_id: 0,
                    search_query: String::new(),
                },
                ListState::default(),
            ));
        }
        Command::BrowseUserFollowedArtists => {
            client_pub.send(ClientRequest::GetUserFollowedArtists)?;
            ui.popup = Some(PopupState::UserFollowedArtistList(ListState::default()));
        }
        Command::BrowseUserSavedAlbums => {
            client_pub.send(ClientRequest::GetUserSavedAlbums)?;
            ui.popup = Some(PopupState::UserSavedAlbumList(ListState::default()));
        }
        Command::TopTrackPage => {
            ui.new_page(PageState::Context {
                id: None,
                context_page_type: ContextPageType::Browsing(ContextId::Tracks(
                    USER_TOP_TRACKS_ID.to_owned(),
                )),
                state: None,
            });
            client_pub.send(ClientRequest::GetUserTopTracks)?;
        }
        Command::RecentlyPlayedTrackPage => {
            ui.new_page(PageState::Context {
                id: None,
                context_page_type: ContextPageType::Browsing(ContextId::Tracks(
                    USER_RECENTLY_PLAYED_TRACKS_ID.to_owned(),
                )),
                state: None,
            });
            client_pub.send(ClientRequest::GetUserRecentlyPlayedTracks)?;
        }
        Command::LikedTrackPage => {
            ui.new_page(PageState::Context {
                id: None,
                context_page_type: ContextPageType::Browsing(ContextId::Tracks(
                    USER_LIKED_TRACKS_ID.to_owned(),
                )),
                state: None,
            });
            client_pub.send(ClientRequest::GetUserSavedTracks)?;
        }
        Command::LibraryPage => {
            ui.new_page(PageState::Library {
                state: LibraryPageUIState::new(),
            });
            let data = state.data.read();
            let refresh_playlists = data
                .user_data
                .playlists_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            let refresh_saved_albums = data
                .user_data
                .saved_albums_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            let refresh_followed_artists = data
                .user_data
                .followed_artists_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            let refresh_saved_shows = data
                .user_data
                .saved_shows_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            drop(data);

            if refresh_playlists {
                client_pub.send(ClientRequest::GetUserPlaylists)?;
            }
            if refresh_saved_albums {
                client_pub.send(ClientRequest::GetUserSavedAlbums)?;
            }
            if refresh_followed_artists {
                client_pub.send(ClientRequest::GetUserFollowedArtists)?;
            }
            if refresh_saved_shows {
                client_pub.send(ClientRequest::GetUserSavedShows)?;
            }
        }
        Command::SearchPage => {
            ui.new_page(PageState::Search {
                line_input: LineInput::default(),
                current_query: String::new(),
                state: SearchPageUIState::new(),
            });
            let data = state.data.read();
            let refresh_playlists = data
                .user_data
                .playlists_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            let refresh_saved_albums = data
                .user_data
                .saved_albums_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            let refresh_saved_shows = data
                .user_data
                .saved_shows_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            let refresh_followed_artists = data
                .user_data
                .followed_artists_last_sync
                .map(|t| t.elapsed() >= *LIBRARY_REFRESH_TTL)
                .unwrap_or(true);
            drop(data);

            if refresh_playlists {
                client_pub.send(ClientRequest::GetUserPlaylists)?;
            }
            if refresh_saved_albums {
                client_pub.send(ClientRequest::GetUserSavedAlbums)?;
            }
            if refresh_saved_shows {
                client_pub.send(ClientRequest::GetUserSavedShows)?;
            }
            if refresh_followed_artists {
                client_pub.send(ClientRequest::GetUserFollowedArtists)?;
            }
        }
        Command::BrowsePage => {
            ui.new_page(PageState::Browse {
                state: BrowsePageUIState::CategoryList {
                    state: ListState::default(),
                },
            });
            client_pub.send(ClientRequest::GetBrowseCategories)?;
        }
        Command::PreviousPage => {
            if ui.history.len() > 1 {
                ui.history.pop();
                ui.popup = None;
            }
        }
        Command::OpenSpotifyLinkFromClipboard => {
            let content = get_clipboard_content().context("get clipboard's content")?;
            let re = regex::Regex::new(
                r"https://open.spotify.com/(?P<type>.*?)/(?P<id>[[:alnum:]]*).*",
            )?;
            if let Some(cap) = re.captures(&content) {
                let typ = cap.name("type").expect("valid capture").as_str();
                let id = cap.name("id").expect("valid capture").as_str();
                match typ {
                    // for track link, play the song
                    "track" => {
                        let id = TrackId::from_id(id)?.into_static();
                        client_pub.send(ClientRequest::Player(PlayerRequest::StartPlayback(
                            Playback::URIs(vec![id.into()], None),
                            None,
                        )))?;
                    }
                    // for playlist/artist/album link, go to the corresponding context page
                    "playlist" => {
                        let id = PlaylistId::from_id(id)?.into_static();
                        ui.new_page(PageState::Context {
                            id: None,
                            context_page_type: ContextPageType::Browsing(ContextId::Playlist(id)),
                            state: None,
                        });
                    }
                    "artist" => {
                        let id = ArtistId::from_id(id)?.into_static();
                        ui.new_page(PageState::Context {
                            id: None,
                            context_page_type: ContextPageType::Browsing(ContextId::Artist(id)),
                            state: None,
                        });
                    }
                    "album" => {
                        let id = AlbumId::from_id(id)?.into_static();
                        ui.new_page(PageState::Context {
                            id: None,
                            context_page_type: ContextPageType::Browsing(ContextId::Album(id)),
                            state: None,
                        });
                    }
                    e => anyhow::bail!("unsupported Spotify type {e}!"),
                }
            } else {
                tracing::warn!("clipboard's content ({content}) is not a valid Spotify link!");
            }
        }
        Command::LyricsPage => {
            if let Some(rspotify::model::PlayableItem::Track(track)) =
                state.player.read().currently_playing()
            {
                if let Some(id) = &track.id {
                    let artists = map_join(&track.artists, |a| &a.name, ", ");
                    ui.new_page(PageState::Lyrics {
                        track_uri: id.uri(),
                        track: track.name.clone(),
                        artists,
                    });

                    client_pub.send(ClientRequest::GetLyrics {
                        track_id: id.clone_static(),
                    })?;
                }
            }
        }
        Command::SwitchDevice => {
            ui.popup = Some(PopupState::DeviceList(ListState::default()));
            client_pub.send(ClientRequest::GetDevices)?;
        }
        Command::SwitchTheme => {
            // get the available themes with the current theme moved to the first position
            let mut themes = config::get_config().theme_config.themes.clone();
            let id = themes.iter().position(|t| t.name == ui.theme.name);
            if let Some(id) = id {
                let theme = themes.remove(id);
                themes.insert(0, theme);
            }

            ui.popup = Some(PopupState::ThemeList(themes, ListState::default()));
        }
        #[cfg(feature = "streaming")]
        Command::RestartIntegratedClient => {
            client_pub.send(ClientRequest::RestartIntegratedClient)?;
        }
        Command::FocusNextWindow => {
            if !ui.has_focused_popup() {
                ui.current_page_mut().next();
            }
        }
        Command::FocusPreviousWindow => {
            if !ui.has_focused_popup() {
                ui.current_page_mut().previous();
            }
        }
        Command::Queue => {
            ui.new_page(PageState::Queue { scroll_offset: 0 });
            client_pub.send(ClientRequest::GetCurrentUserQueue)?;
        }
        Command::CreatePlaylist => {
            ui.popup = Some(PopupState::PlaylistCreate {
                name: LineInput::default(),
                desc: LineInput::default(),
                current_field: PlaylistCreateCurrentField::Name,
            });
        }
        Command::JumpToCurrentTrackInContext => {
            let track_id = match state.player.read().currently_playing() {
                Some(rspotify::model::PlayableItem::Track(track)) => {
                    PlayableId::Track(track.id.clone().expect("all non-local tracks have ids"))
                }
                Some(rspotify::model::PlayableItem::Episode(episode)) => {
                    PlayableId::Episode(episode.id.clone())
                }
                Some(rspotify::model::PlayableItem::Unknown(_)) | None => return Ok(false),
            };

            if let PageState::Context {
                id: Some(context_id),
                ..
            } = ui.current_page()
            {
                let context_track_pos = state
                    .data
                    .read()
                    .context_tracks(context_id)
                    .and_then(|tracks| tracks.iter().position(|t| t.id.uri() == track_id.uri()));

                if let Some(p) = context_track_pos {
                    ui.current_page_mut().select(p);
                }
            }
        }
        Command::ClosePopup => {
            ui.popup = None;
        }
        _ => return Ok(false),
    }
    Ok(true)
}
