#!/usr/bin/env python3
"""Export Spotify library (playlists, saved tracks/albums, etc.) to JSON files."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Dict, Iterable, List

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from export_playlists import (  # type: ignore  # pylint: disable=import-error
    API_BASE,
    fetch_all_items,
    refresh_access_token,
    api_request,
    export_playlists,
)


def export_saved_tracks(token: str, output_path: Path) -> int:
    tracks: List[Dict] = []
    for item in fetch_all_items(f"{API_BASE}/me/tracks?limit=50", token):
        track = item.get("track")
        if not track or not track.get("id"):
            continue
        tracks.append(
            {
                "id": track.get("id"),
                "uri": track.get("uri"),
                "name": track.get("name"),
                "artists": [artist.get("name") for artist in track.get("artists", []) if artist.get("name")],
                "album": track.get("album", {}).get("name"),
                "duration_ms": track.get("duration_ms"),
                "explicit": track.get("explicit"),
                "added_at": item.get("added_at"),
            }
        )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(tracks, indent=2, ensure_ascii=False), encoding="utf-8")
    return len(tracks)


def export_saved_albums(token: str, output_path: Path) -> int:
    albums: List[Dict] = []
    for item in fetch_all_items(f"{API_BASE}/me/albums?limit=50", token):
        album = item.get("album")
        if not album or not album.get("id"):
            continue
        albums.append(
            {
                "id": album.get("id"),
                "uri": album.get("uri"),
                "name": album.get("name"),
                "artists": [artist.get("name") for artist in album.get("artists", []) if artist.get("name")],
                "release_date": album.get("release_date"),
                "total_tracks": album.get("total_tracks"),
                "added_at": item.get("added_at"),
            }
        )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(albums, indent=2, ensure_ascii=False), encoding="utf-8")
    return len(albums)


def fetch_followed_artists(token: str) -> Iterable[Dict]:
    url = f"{API_BASE}/me/following?type=artist&limit=50"
    while url:
        page = api_request(url, token)
        artists_page = page.get("artists", {})
        for artist in artists_page.get("items", []):
            yield artist
        url = artists_page.get("next")


def export_followed_artists(token: str, output_path: Path) -> int:
    artists = []
    for artist in fetch_followed_artists(token):
        if not artist.get("id"):
            continue
        artists.append(
            {
                "id": artist.get("id"),
                "uri": artist.get("uri"),
                "name": artist.get("name"),
                "genres": artist.get("genres", []),
                "popularity": artist.get("popularity"),
            }
        )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(artists, indent=2, ensure_ascii=False), encoding="utf-8")
    return len(artists)


def export_saved_shows(token: str, output_path: Path) -> int:
    shows: List[Dict] = []
    for item in fetch_all_items(f"{API_BASE}/me/shows?limit=50", token):
        show = item.get("show") or item
        if not show or not show.get("id"):
            continue
        shows.append(
            {
                "id": show.get("id"),
                "uri": show.get("uri"),
                "name": show.get("name"),
                "publisher": show.get("publisher"),
                "languages": show.get("languages", []),
                "media_type": show.get("media_type"),
                "added_at": item.get("added_at"),
            }
        )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(shows, indent=2, ensure_ascii=False), encoding="utf-8")
    return len(shows)


def export_saved_episodes(token: str, output_path: Path) -> int:
    episodes: List[Dict] = []
    for item in fetch_all_items(f"{API_BASE}/me/episodes?limit=50", token):
        episode = item.get("episode") or item.get("item") or item
        if not episode or not episode.get("id"):
            continue
        episodes.append(
            {
                "id": episode.get("id"),
                "uri": episode.get("uri"),
                "name": episode.get("name"),
                "show": episode.get("show", {}).get("name"),
                "duration_ms": episode.get("duration_ms"),
                "release_date": episode.get("release_date"),
                "explicit": episode.get("explicit"),
                "added_at": item.get("added_at"),
            }
        )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(episodes, indent=2, ensure_ascii=False), encoding="utf-8")
    return len(episodes)


def export_library(output_dir: Path, include_playlists: bool) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)

    token = refresh_access_token()
    me = api_request(f"{API_BASE}/me", token)
    user_id = me.get("id")
    print(f"Exporting library data for user {user_id!r}")

    summary = {
        "user": {"id": user_id, "display_name": me.get("display_name")},
        "counts": {},
    }

    playlists_dir = output_dir / "playlists"
    if include_playlists:
        print("Exporting playlists ...")
        export_playlists(playlists_dir)
        summary["counts"]["playlists"] = len(list(playlists_dir.glob("*.json")))

    counts = {
        "saved_tracks": export_saved_tracks(token, output_dir / "saved_tracks.json"),
        "saved_albums": export_saved_albums(token, output_dir / "saved_albums.json"),
        "followed_artists": export_followed_artists(token, output_dir / "followed_artists.json"),
        "saved_shows": export_saved_shows(token, output_dir / "saved_shows.json"),
        "saved_episodes": export_saved_episodes(token, output_dir / "saved_episodes.json"),
    }

    summary["counts"].update(counts)
    (output_dir / "index.json").write_text(json.dumps(summary, indent=2, ensure_ascii=False), encoding="utf-8")
    print("Export complete.")


def main() -> None:
    parser = argparse.ArgumentParser(description="Export Spotify library (playlists, saved tracks/albums/etc.).")
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("./library_exports"),
        help="Directory to write exported data (default: ./library_exports)",
    )
    parser.add_argument(
        "--skip-playlists",
        action="store_true",
        help="Skip exporting playlists (only export saved tracks/albums/artists/etc.)",
    )
    args = parser.parse_args()

    export_library(args.output, include_playlists=not args.skip_playlists)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(1)
