#!/usr/bin/env python3
"""Import Spotify library data (playlists, saved tracks/albums/etc.) from JSON."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Iterable, List

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from export_playlists import (  # type: ignore  # pylint: disable=import-error
    API_BASE,
    refresh_access_token,
    api_request,
)
from import_playlists import import_playlists as import_playlist_dir  # type: ignore  # pylint: disable=import-error


def chunked(items: Iterable[str], size: int) -> Iterable[List[str]]:
    batch: List[str] = []
    for item in items:
        batch.append(item)
        if len(batch) == size:
            yield batch
            batch = []
    if batch:
        yield batch


def load_ids(path: Path, key: str = "id") -> List[str]:
    if not path.exists():
        print(f"  Skipping {path.name} (file not found)")
        return []
    data = json.loads(path.read_text(encoding="utf-8"))
    return [item.get(key) for item in data if item.get(key)]


def import_saved_tracks(token: str, path: Path) -> int:
    ids = list(dict.fromkeys(load_ids(path)))
    count = 0
    for chunk in chunked(ids, 50):
        api_request(f"{API_BASE}/me/tracks", token, method="PUT", body={"ids": chunk})
        count += len(chunk)
    return count


def import_saved_albums(token: str, path: Path) -> int:
    ids = list(dict.fromkeys(load_ids(path)))
    count = 0
    for chunk in chunked(ids, 50):
        api_request(f"{API_BASE}/me/albums", token, method="PUT", body={"ids": chunk})
        count += len(chunk)
    return count


def import_followed_artists(token: str, path: Path) -> int:
    ids = list(dict.fromkeys(load_ids(path)))
    count = 0
    for chunk in chunked(ids, 50):
        api_request(f"{API_BASE}/me/following?type=artist", token, method="PUT", body={"ids": chunk})
        count += len(chunk)
    return count


def import_saved_shows(token: str, path: Path) -> int:
    ids = list(dict.fromkeys(load_ids(path)))
    count = 0
    for chunk in chunked(ids, 50):
        api_request(f"{API_BASE}/me/shows", token, method="PUT", body={"ids": chunk})
        count += len(chunk)
    return count


def import_saved_episodes(token: str, path: Path) -> int:
    ids = list(dict.fromkeys(load_ids(path)))
    count = 0
    for chunk in chunked(ids, 50):
        api_request(f"{API_BASE}/me/episodes", token, method="PUT", body={"ids": chunk})
        count += len(chunk)
    return count


def import_library(input_dir: Path, prefix: str | None, include_playlists: bool) -> None:
    if not input_dir.exists():
        raise SystemExit(f"Input directory not found: {input_dir}")

    token = refresh_access_token()
    summary = {}

    summary["saved_tracks"] = import_saved_tracks(token, input_dir / "saved_tracks.json")
    summary["saved_albums"] = import_saved_albums(token, input_dir / "saved_albums.json")
    summary["followed_artists"] = import_followed_artists(token, input_dir / "followed_artists.json")
    summary["saved_shows"] = import_saved_shows(token, input_dir / "saved_shows.json")
    summary["saved_episodes"] = import_saved_episodes(token, input_dir / "saved_episodes.json")

    if include_playlists:
        playlists_dir = input_dir / "playlists"
        if playlists_dir.exists():
            print("Importing playlists ...")
            import_playlist_dir(playlists_dir, prefix=prefix)
        else:
            print("No playlists directory found; skipping playlist import.")

    print("Import summary:")
    for key, value in summary.items():
        print(f"  {key}: {value}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Import Spotify library data from JSON exports.")
    parser.add_argument(
        "--input",
        type=Path,
        default=Path("./library_exports"),
        help="Directory containing exported library JSON (default: ./library_exports)",
    )
    parser.add_argument(
        "--prefix",
        type=str,
        default=None,
        help="Optional prefix for imported playlist names.",
    )
    parser.add_argument(
        "--skip-playlists",
        action="store_true",
        help="Skip importing playlists (only restore saved tracks/albums/etc.)",
    )
    args = parser.parse_args()

    import_library(args.input, prefix=args.prefix, include_playlists=not args.skip_playlists)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(1)
