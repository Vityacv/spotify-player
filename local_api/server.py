#!/usr/bin/env python3

"""
Simple local playlist API server that mimics a subset of Spotify playlist endpoints.
All data is stored on disk as JSON, so the application can work without Spotify's backend.
"""

from __future__ import annotations

import argparse
import copy
import json
import threading
import uuid
from datetime import datetime, timezone
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Dict, List, Optional
from urllib.parse import urlparse


DATA_ROOT = Path(__file__).resolve().parent / "data"
DEFAULT_DB_FILE = DATA_ROOT / "playlists.json"


def utc_now_iso() -> str:
    """Return an ISO-8601 timestamp in UTC."""
    return datetime.now(timezone.utc).isoformat()


class LocalPlaylistStore:
    """Persist playlists to a JSON file with simple in-memory caching."""

    def __init__(self, db_file: Path):
        self._db_file = db_file
        self._lock = threading.Lock()
        self._state: Dict[str, Any] = {"playlists": []}
        self._load()

    def _load(self) -> None:
        if self._db_file.exists():
            try:
                with self._db_file.open("r", encoding="utf-8") as handle:
                    self._state = json.load(handle)
            except json.JSONDecodeError:
                # Corrupted file; start fresh but keep the old file as a backup.
                backup_path = self._db_file.with_suffix(".corrupt")
                self._db_file.rename(backup_path)
                self._state = {"playlists": []}
        else:
            self._db_file.parent.mkdir(parents=True, exist_ok=True)
            self._persist()

    def _persist(self) -> None:
        self._db_file.parent.mkdir(parents=True, exist_ok=True)
        tmp_path = self._db_file.with_suffix(".tmp")
        with tmp_path.open("w", encoding="utf-8") as handle:
            json.dump(self._state, handle, indent=2, ensure_ascii=True)
        tmp_path.replace(self._db_file)

    def _find_playlist_index(self, playlist_id: str) -> Optional[int]:
        for idx, playlist in enumerate(self._state["playlists"]):
            if playlist["id"] == playlist_id:
                return idx
        return None

    def list_playlists(self) -> List[Dict[str, Any]]:
        with self._lock:
            return copy.deepcopy(self._state["playlists"])

    def get_playlist(self, playlist_id: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            idx = self._find_playlist_index(playlist_id)
            if idx is None:
                return None
            return copy.deepcopy(self._state["playlists"][idx])

    def create_playlist(
        self,
        name: str,
        description: str,
        public: bool,
        collaborative: bool,
    ) -> Dict[str, Any]:
        playlist = {
            "id": f"pl_{uuid.uuid4().hex}",
            "name": name,
            "description": description,
            "public": public,
            "collaborative": collaborative,
            "tracks": [],
            "created_at": utc_now_iso(),
            "updated_at": utc_now_iso(),
        }
        with self._lock:
            self._state["playlists"].append(playlist)
            self._persist()
            return copy.deepcopy(playlist)

    def update_playlist(
        self,
        playlist_id: str,
        *,
        name: Optional[str] = None,
        description: Optional[str] = None,
        public: Optional[bool] = None,
        collaborative: Optional[bool] = None,
    ) -> Optional[Dict[str, Any]]:
        with self._lock:
            idx = self._find_playlist_index(playlist_id)
            if idx is None:
                return None
            playlist = self._state["playlists"][idx]
            if name is not None:
                playlist["name"] = name
            if description is not None:
                playlist["description"] = description
            if public is not None:
                playlist["public"] = public
            if collaborative is not None:
                playlist["collaborative"] = collaborative
            playlist["updated_at"] = utc_now_iso()
            self._persist()
            return copy.deepcopy(playlist)

    def delete_playlist(self, playlist_id: str) -> bool:
        with self._lock:
            idx = self._find_playlist_index(playlist_id)
            if idx is None:
                return False
            self._state["playlists"].pop(idx)
            self._persist()
            return True

    def add_tracks(self, playlist_id: str, tracks: List[Dict[str, Any]]) -> Optional[Dict[str, Any]]:
        with self._lock:
            idx = self._find_playlist_index(playlist_id)
            if idx is None:
                return None
            playlist = self._state["playlists"][idx]

            for raw_track in tracks:
                track_id = raw_track.get("id") or f"trk_{uuid.uuid4().hex}"
                track = {
                    "id": track_id,
                    "name": raw_track.get("name", ""),
                    "artists": raw_track.get("artists", []),
                    "album": raw_track.get("album", ""),
                    "added_at": utc_now_iso(),
                }
                playlist["tracks"].append(track)

            playlist["updated_at"] = utc_now_iso()
            self._persist()
            return copy.deepcopy(playlist)

    def remove_track(self, playlist_id: str, track_id: str) -> Optional[Dict[str, Any]]:
        with self._lock:
            idx = self._find_playlist_index(playlist_id)
            if idx is None:
                return None
            playlist = self._state["playlists"][idx]
            tracks = playlist["tracks"]
            for i, track in enumerate(tracks):
                if track["id"] == track_id:
                    tracks.pop(i)
                    playlist["updated_at"] = utc_now_iso()
                    self._persist()
                    return copy.deepcopy(playlist)
            return None


class PlaylistRequestHandler(BaseHTTPRequestHandler):
    """HTTP handler that exposes PlaylistStore operations via JSON requests."""

    store: LocalPlaylistStore = LocalPlaylistStore(DEFAULT_DB_FILE)

    server_version = "LocalPlaylistServer/0.1"

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        parts = self._split_path(parsed.path)

        if parts == ["health"]:
            self._write_json(HTTPStatus.OK, {"status": "ok"})
            return

        if not parts or parts[0] != "playlists":
            self._write_json(HTTPStatus.NOT_FOUND, {"error": "Not Found"})
            return

        if len(parts) == 1:
            playlists = self.store.list_playlists()
            self._write_json(HTTPStatus.OK, {"items": playlists})
            return

        if len(parts) == 2:
            playlist = self.store.get_playlist(parts[1])
            if playlist is None:
                self._write_json(HTTPStatus.NOT_FOUND, {"error": "Playlist not found"})
                return
            self._write_json(HTTPStatus.OK, playlist)
            return

        self._write_json(HTTPStatus.NOT_FOUND, {"error": "Endpoint not found"})

    def do_POST(self) -> None:
        parsed = urlparse(self.path)
        parts = self._split_path(parsed.path)
        payload = self._read_json_body()
        if payload is None:
            return

        if parts == ["playlists"]:
            try:
                playlist = self.store.create_playlist(
                    name=str(payload.get("name", "")).strip(),
                    description=str(payload.get("description", "")).strip(),
                    public=bool(payload.get("public", False)),
                    collaborative=bool(payload.get("collaborative", False)),
                )
            except Exception as exc:
                self._write_json(
                    HTTPStatus.BAD_REQUEST,
                    {"error": f"Failed to create playlist: {exc}"},
                )
                return
            self._write_json(HTTPStatus.CREATED, playlist)
            return

        if len(parts) == 3 and parts[0] == "playlists" and parts[2] == "tracks":
            playlist_id = parts[1]
            tracks = payload.get("tracks", [])
            if not isinstance(tracks, list):
                self._write_json(HTTPStatus.BAD_REQUEST, {"error": "tracks must be a list"})
                return
            playlist = self.store.add_tracks(playlist_id, tracks)
            if playlist is None:
                self._write_json(HTTPStatus.NOT_FOUND, {"error": "Playlist not found"})
                return
            self._write_json(HTTPStatus.OK, playlist)
            return

        self._write_json(HTTPStatus.NOT_FOUND, {"error": "Endpoint not found"})

    def do_PUT(self) -> None:
        parsed = urlparse(self.path)
        parts = self._split_path(parsed.path)
        payload = self._read_json_body()
        if payload is None:
            return

        if len(parts) == 2 and parts[0] == "playlists":
            playlist_id = parts[1]
            playlist = self.store.update_playlist(
                playlist_id,
                name=payload.get("name"),
                description=payload.get("description"),
                public=payload.get("public"),
                collaborative=payload.get("collaborative"),
            )
            if playlist is None:
                self._write_json(HTTPStatus.NOT_FOUND, {"error": "Playlist not found"})
                return
            self._write_json(HTTPStatus.OK, playlist)
            return

        self._write_json(HTTPStatus.NOT_FOUND, {"error": "Endpoint not found"})

    def do_DELETE(self) -> None:
        parsed = urlparse(self.path)
        parts = self._split_path(parsed.path)

        if len(parts) == 2 and parts[0] == "playlists":
            playlist_id = parts[1]
            deleted = self.store.delete_playlist(playlist_id)
            if not deleted:
                self._write_json(HTTPStatus.NOT_FOUND, {"error": "Playlist not found"})
                return
            self._write_json(HTTPStatus.NO_CONTENT, None)
            return

        if len(parts) == 4 and parts[0] == "playlists" and parts[2] == "tracks":
            playlist_id = parts[1]
            track_id = parts[3]
            playlist = self.store.remove_track(playlist_id, track_id)
            if playlist is None:
                self._write_json(HTTPStatus.NOT_FOUND, {"error": "Playlist or track not found"})
                return
            self._write_json(HTTPStatus.OK, playlist)
            return

        self._write_json(HTTPStatus.NOT_FOUND, {"error": "Endpoint not found"})

    def do_OPTIONS(self) -> None:
        self.send_response(HTTPStatus.NO_CONTENT)
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET,POST,PUT,DELETE,OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "Content-Type")
        self.end_headers()

    def log_message(self, fmt: str, *args: Any) -> None:  # noqa: D401 - standard override
        """Quiet logging to keep CLI output clean."""
        return

    def _split_path(self, path: str) -> List[str]:
        return [part for part in path.split("/") if part]

    def _read_json_body(self) -> Optional[Dict[str, Any]]:
        length = int(self.headers.get("Content-Length", "0"))
        if length == 0:
            return {}
        try:
            body = self.rfile.read(length)
            data = json.loads(body.decode("utf-8"))
            if not isinstance(data, dict):
                raise ValueError("JSON payload must be an object")
            return data
        except (json.JSONDecodeError, ValueError) as exc:
            self._write_json(HTTPStatus.BAD_REQUEST, {"error": str(exc)})
            return None

    def _write_json(self, status: HTTPStatus, payload: Optional[Any]) -> None:
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        if payload is not None:
            self.wfile.write(json.dumps(payload).encode("utf-8"))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Local playlist API server.")
    parser.add_argument("--host", default="127.0.0.1", help="Bind address (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=8765, help="Port to listen on (default: 8765)")
    parser.add_argument(
        "--db",
        type=Path,
        default=DEFAULT_DB_FILE,
        help="Path to the playlist JSON file (default: local_api/data/playlists.json)",
    )
    return parser.parse_args()


def run_server(host: str, port: int, db_file: Path) -> None:
    PlaylistRequestHandler.store = LocalPlaylistStore(db_file)
    server = ThreadingHTTPServer((host, port), PlaylistRequestHandler)
    print(f"Local playlist server listening on http://{host}:{port}")
    print(f"Data file: {db_file}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nShutting down.")
    finally:
        server.server_close()


def main() -> None:
    args = parse_args()
    run_server(args.host, args.port, args.db.resolve())


if __name__ == "__main__":
    main()
