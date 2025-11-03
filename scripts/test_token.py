#!/usr/bin/env python3
"""Validate a Spotify access token (useful for debugging local AUTH issues)."""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


def default_credentials_path() -> Path:
    cache_root = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return cache_root / "spotify_player" / "credentials.json"


def load_token_from_file(path: Path) -> str:
    try:
        with path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
    except FileNotFoundError as exc:
        raise SystemExit(f"Credentials file not found: {path}") from exc
    except json.JSONDecodeError as exc:
        raise SystemExit(f"Failed to parse JSON credentials file: {exc}") from exc

    # Support multiple formats: prefer explicit `access_token`, otherwise fall back to librespot cache layout.
    if isinstance(data, dict):
        if "access_token" in data:
            return str(data["access_token"])
        if "auth_data" in data:
            return str(data["auth_data"])

    raise SystemExit(f"No access token found in credentials file: {path}")


def validate_token(token: str) -> None:
    request = urllib.request.Request(
        "https://api.spotify.com/v1/me",
        headers={"Authorization": f"Bearer {token}"},
    )

    try:
        with urllib.request.urlopen(request) as response:
            payload = json.load(response)
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", "ignore")
        raise SystemExit(
            f"Spotify API rejected the token (status {exc.code}): {detail or exc.reason}"
        ) from exc
    except urllib.error.URLError as exc:
        raise SystemExit(f"Network error while contacting Spotify: {exc.reason}") from exc

    user_id = payload.get("id", "<unknown>")
    display_name = payload.get("display_name") or "<unnamed>"
    print(f"Token is valid. User: {display_name} ({user_id})")


def main(argv: list[str]) -> None:
    parser = argparse.ArgumentParser(
        description="Validate a Spotify access token by calling the Web API."
    )
    parser.add_argument(
        "--token",
        help="Access token to test (overrides --file).",
    )
    parser.add_argument(
        "--file",
        type=Path,
        default=default_credentials_path(),
        help="Path to credentials JSON file to read (default: %(default)s).",
    )
    args = parser.parse_args(argv)

    token = args.token or load_token_from_file(args.file)
    if not token:
        raise SystemExit("No token supplied.")

    validate_token(token)


if __name__ == "__main__":
    main(sys.argv[1:])
