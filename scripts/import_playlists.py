#!/usr/bin/env python3
"""
Import Spotify playlists from JSON exports created by export_playlists.py.

Usage:
    python scripts/import_playlists.py [--input DIR] [--prefix PREFIX]
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Iterable, List

CLIENT_ID = "65b708073fc0480ea92a077233ca87bd"
TOKEN_PATH = Path(os.environ.get("SPOTIFY_TOKEN_FILE", Path.home() / ".cache/spotify-player/oauth_token.json"))
API_BASE = "https://api.spotify.com/v1"
TOKEN_URL = "https://accounts.spotify.com/api/token"


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


def api_request(url: str, access_token: str, method: str = "GET", body: dict | None = None) -> dict:
    data_bytes = None
    headers = {"Authorization": f"Bearer {access_token}", "Content-Type": "application/json"}
    if body is not None:
        data_bytes = json.dumps(body).encode("utf-8")

    request = urllib.request.Request(url, data=data_bytes, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request) as resp:
            if resp.status == 204:
                return {}
            return json.load(resp)
    except urllib.error.HTTPError as err:
        detail = err.read().decode("utf-8", "ignore")
        raise SystemExit(f"API request failed ({err.code}) for {url}: {detail or err.reason}") from err


def chunked(iterable: Iterable[str], size: int) -> Iterable[List[str]]:
    chunk: List[str] = []
    for item in iterable:
        chunk.append(item)
        if len(chunk) == size:
            yield chunk
            chunk = []
    if chunk:
        yield chunk


def import_playlists(input_dir: Path, prefix: str | None) -> None:
    if not input_dir.exists():
        raise SystemExit(f"Input directory not found: {input_dir}")

    files = sorted(p for p in input_dir.iterdir() if p.suffix == ".json" and p.name != "index.json")
    if not files:
        raise SystemExit(f"No playlist JSON files found in {input_dir}")

    token = refresh_access_token()
    me = api_request(f"{API_BASE}/me", token)
    user_id = me.get("id")
    print(f"Importing playlists for user {user_id!r}")

    for path in files:
        with path.open("r", encoding="utf-8") as f:
            data = json.load(f)

        name = data.get("name", "Untitled")
        if prefix:
            name = f"{prefix}{name}"
        description = data.get("description")
        public = data.get("public")
        collaborative = data.get("collaborative") or False
        if collaborative and public:
            public = False

        body = {"name": name}
        if description is not None:
            body["description"] = description
        if public is not None:
            body["public"] = public
        if collaborative:
            body["collaborative"] = True

        print(f"  Creating playlist {name!r} with {len(data.get('tracks', []))} tracks")
        playlist = api_request(f"{API_BASE}/users/{user_id}/playlists", token, method="POST", body=body)
        playlist_id = playlist.get("id")
        if not playlist_id:
            raise SystemExit("Failed to create playlist; missing id in response")

        uris = [track.get("uri") for track in data.get("tracks", []) if track.get("uri")]
        for chunk in chunked(uris, 100):
            api_request(
                f"{API_BASE}/playlists/{playlist_id}/tracks",
                token,
                method="POST",
                body={"uris": chunk},
            )

    print("Import completed.")


def main() -> None:
    parser = argparse.ArgumentParser(description="Import playlists from JSON exports.")
    parser.add_argument(
        "--input",
        type=Path,
        default=Path("./playlist_exports"),
        help="Directory containing exported playlist JSON files (default: ./playlist_exports)",
    )
    parser.add_argument(
        "--prefix",
        type=str,
        default=None,
        help="Optional prefix to add to each imported playlist name.",
    )
    args = parser.parse_args()
    import_playlists(args.input, args.prefix)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(1)
