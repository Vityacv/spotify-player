#!/usr/bin/env python3
"""
Export Spotify playlists using the cached spotify-player OAuth refresh token.

Usage:
    python scripts/export_playlists.py [--output DIR]
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Dict, Iterable, List, Optional

CLIENT_ID = "65b708073fc0480ea92a077233ca87bd"
TOKEN_PATH = Path(os.environ.get("SPOTIFY_TOKEN_FILE", Path.home() / ".cache/spotify-player/oauth_token.json"))
API_BASE = "https://api.spotify.com/v1"
TOKEN_URL = "https://accounts.spotify.com/api/token"


def slugify(name: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9]+", "_", name).strip("_")
    if not slug:
        slug = "playlist"
    return slug[:80]


def read_refresh_token() -> str:
    if not TOKEN_PATH.exists():
        raise SystemExit(f"Refresh token file not found: {TOKEN_PATH}")
    with TOKEN_PATH.open("r", encoding="utf-8") as f:
        data = json.load(f)
    token = data.get("refresh_token")
    if not token:
        raise SystemExit("Refresh token missing in oauth_token.json")
    return token


def write_refresh_token(refresh_token: str) -> None:
    TOKEN_PATH.parent.mkdir(parents=True, exist_ok=True)
    with TOKEN_PATH.open("w", encoding="utf-8") as f:
        json.dump({"refresh_token": refresh_token}, f, indent=2)


def refresh_access_token() -> str:
    refresh_token = read_refresh_token()
    payload = urllib.parse.urlencode(
        {
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        TOKEN_URL,
        data=payload,
        method="POST",
        headers={"Content-Type": "application/x-www-form-urlencoded"},
    )
    try:
        with urllib.request.urlopen(request) as resp:
            data = json.load(resp)
    except urllib.error.HTTPError as err:
        detail = err.read().decode("utf-8", "ignore")
        raise SystemExit(f"Failed to refresh access token ({err.code}): {detail or err.reason}") from err
    new_refresh = data.get("refresh_token", refresh_token)
    write_refresh_token(new_refresh)
    access_token = data.get("access_token")
    if not access_token:
        raise SystemExit("Refresh response missing access_token")
    return access_token


def api_request(url: str, access_token: str, method: str = "GET", body: Optional[dict] = None) -> dict:
    data_bytes = None
    headers = {"Authorization": f"Bearer {access_token}", "Content-Type": "application/json"}
    if body is not None:
        data_bytes = json.dumps(body).encode("utf-8")

    request = urllib.request.Request(url, data=data_bytes, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request) as resp:
            if resp.status == 204:
                return {}
            body = resp.read()
            if not body:
                return {}
            return json.loads(body.decode("utf-8"))
    except urllib.error.HTTPError as err:
        detail = err.read().decode("utf-8", "ignore")
        raise SystemExit(f"API request failed ({err.code}) for {url}: {detail or err.reason}") from err


def fetch_all_items(initial_url: str, access_token: str) -> Iterable[dict]:
    url = initial_url
    while url:
        page = api_request(url, access_token)
        for item in page.get("items", []):
            yield item
        url = page.get("next")


def gather_playlists(access_token: str) -> List[dict]:
    playlists = list(fetch_all_items(f"{API_BASE}/me/playlists?limit=50", access_token))
    return playlists


def gather_tracks(playlist_id: str, access_token: str) -> List[dict]:
    tracks: List[dict] = []
    url = f"{API_BASE}/playlists/{playlist_id}/tracks?limit=100"
    for item in fetch_all_items(url, access_token):
        track = item.get("track")
        if not track or not track.get("uri"):
            continue
        tracks.append(
            {
                "uri": track["uri"],
                "name": track.get("name"),
                "artists": [artist.get("name") for artist in track.get("artists", []) if artist.get("name")],
                "album": track.get("album", {}).get("name"),
                "duration_ms": track.get("duration_ms"),
                "explicit": track.get("explicit"),
                "added_at": item.get("added_at"),
            }
        )
    return tracks


def export_playlists(output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)

    token = refresh_access_token()
    me = api_request(f"{API_BASE}/me", token)
    user_id = me.get("id")
    print(f"Exporting playlists for user {user_id!r}")

    playlists = gather_playlists(token)
    print(f"Found {len(playlists)} playlists")

    index: List[Dict[str, str]] = []
    for idx, playlist in enumerate(playlists, start=1):
        playlist_id = playlist["id"]
        name = playlist.get("name", "Untitled")
        print(f"  [{idx}/{len(playlists)}] {name}")
        tracks = gather_tracks(playlist_id, token)
        data = {
            "id": playlist_id,
            "name": name,
            "description": playlist.get("description"),
            "public": playlist.get("public"),
            "collaborative": playlist.get("collaborative"),
            "snapshot_id": playlist.get("snapshot_id"),
            "owner_id": playlist.get("owner", {}).get("id"),
            "tracks": tracks,
        }

        slug = slugify(name)
        filename = f"{idx:03d}_{slug}_{playlist_id}.json"
        with (output_dir / filename).open("w", encoding="utf-8") as f:
            json.dump(data, f, indent=2, ensure_ascii=False)

        index.append({"file": filename, "id": playlist_id, "name": name})

    with (output_dir / "index.json").open("w", encoding="utf-8") as f:
        json.dump(
            {
                "user": {"id": user_id, "display_name": me.get("display_name")},
                "playlists": index,
            },
            f,
            indent=2,
            ensure_ascii=False,
        )
    print(f"Export completed. Files written to {output_dir}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Export Spotify playlists to JSON files.")
    parser.add_argument(
        "--output",
        type=Path,
        default=Path("./playlist_exports"),
        help="Directory to write exported playlists (default: ./playlist_exports)",
    )
    args = parser.parse_args()
    export_playlists(args.output)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(1)
