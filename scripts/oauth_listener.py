#!/usr/bin/env python3
"""Minimal Spotify OAuth helper.

The script can:
  • Generate an authorization URL (using the same default scopes as spotify_player).
  • Optionally open that URL in your browser.
  • Run a tiny HTTP server that listens for the redirect and prints the returned parameters.

Example:
    python scripts/oauth_listener.py --open-browser
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import socket
import sys
import threading
import urllib.parse
import webbrowser
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib import request as urllib_request, error as urllib_error
from pathlib import Path
from secrets import token_urlsafe
from typing import Dict, Iterable, Optional

DEFAULT_SCOPES = [
    # Spotify Connect
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    # Playback
    "app-remote-control",
    "streaming",
    # Playlists
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    # Follow
    "user-follow-modify",
    "user-follow-read",
    # Listening History
    "user-read-playback-position",
    "user-top-read",
    "user-read-recently-played",
    # Library
    "user-library-modify",
    "user-library-read",
    # Users
    "user-personalized",
]

DEFAULT_CLIENT_ID = "65b708073fc0480ea92a077233ca87bd"  # same as spotify_player


class OAuthListener(BaseHTTPRequestHandler):
    """HTTP handler that records the first OAuth redirect and then stops."""

    redirect_data: Optional[Dict[str, str]] = None
    done_event: threading.Event = threading.Event()

    def do_GET(self) -> None:  # noqa: N802 (BaseHTTPRequestHandler signature)
        parsed = urllib.parse.urlparse(self.path)
        params = {k: v[0] for k, v in urllib.parse.parse_qs(parsed.query).items()}

        # Store only the first redirect.
        if not OAuthListener.done_event.is_set():
            OAuthListener.redirect_data = params
            OAuthListener.done_event.set()

        body = """<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <title>Spotify Authorization Received</title>
    <style>
      body { font-family: sans-serif; margin: 2em; color: #1DB954; background: #121212; }
      .card { background: #181818; padding: 2em; border-radius: 8px; max-width: 28rem; }
      h1 { margin-top: 0; }
      code { color: #1DB954; }
    </style>
  </head>
  <body>
    <div class="card">
      <h1>Authorization Received</h1>
      <p>You can close this tab and return to the terminal.</p>
    </div>
  </body>
</html>
"""
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body.encode("utf-8"))))
        self.end_headers()
        self.wfile.write(body.encode("utf-8"))

    def log_message(self, format: str, *args) -> None:  # noqa: A003 (shadow built-in)
        # Silence default logging to keep terminal clean.
        return


def build_authorize_url(
    client_id: str,
    redirect_uri: str,
    scopes: Iterable[str],
    state: str,
    show_dialog: bool,
    code_challenge: str,
) -> str:
    params = {
        "response_type": "code",
        "client_id": client_id,
        "redirect_uri": redirect_uri,
        "scope": " ".join(scopes),
        "state": state,
        "code_challenge_method": "S256",
        "code_challenge": code_challenge,
    }
    if show_dialog:
        params["show_dialog"] = "true"

    return "https://accounts.spotify.com/authorize?" + urllib.parse.urlencode(params)


def wait_for_redirect(host: str, port: int, timeout: Optional[float]) -> Dict[str, str]:
    server = HTTPServer((host, port), OAuthListener)

    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    print(f"Listening on http://{host}:{port}/login ...")
    print("Waiting for browser redirect (Ctrl+C to abort).")

    try:
        OAuthListener.done_event.wait(timeout=timeout)
    except KeyboardInterrupt:
        print("\nInterrupted.")
    finally:
        server.shutdown()
        server.server_close()

    if OAuthListener.redirect_data is None:
        raise TimeoutError("No redirect received.")

    return OAuthListener.redirect_data


def pick_free_port(port: int) -> int:
    if port != 0:
        return port

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def parse_args(argv: Optional[list[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default=None, help="Bind address (default: derived from redirect URI or 127.0.0.1)")
    parser.add_argument(
        "--port",
        type=int,
        default=None,
        help="Port to listen on (default: derived from redirect URI or 8989; set 0 to pick a free port).",
    )
    parser.add_argument(
        "--redirect-uri",
        help="Redirect URI to embed in the authorization URL (default: http://HOST:PORT/login).",
    )
    parser.add_argument(
        "--client-id",
        default=DEFAULT_CLIENT_ID,
        help="Spotify application client ID (default: spotify_player client ID).",
    )
    parser.add_argument(
        "--scope",
        action="append",
        dest="scopes",
        help="Additional scope to request (repeatable). Defaults to the spotify_player scope set.",
    )
    parser.add_argument(
        "--state",
        help="Optional OAuth state value (default: random).",
    )
    parser.add_argument(
        "--show-dialog",
        action="store_true",
        help="Force Spotify to always show the approval dialog.",
    )
    parser.add_argument(
        "--open-browser",
        action="store_true",
        help="Open the generated authorization URL in your default browser.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=None,
        help="Seconds to wait for the redirect (default: wait indefinitely).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Print captured parameters as JSON instead of key=value lines.",
    )
    parser.add_argument(
        "--no-server",
        action="store_true",
        help="Only generate the authorization URL; do not start the redirect listener.",
    )
    parser.add_argument(
        "--code-verifier",
        help="Explicit PKCE code verifier (default: random).",
    )
    parser.add_argument(
        "--exchange-token",
        action="store_true",
        help="After receiving the authorization code, exchange it for an access token.",
    )
    parser.add_argument(
        "--save-token",
        type=Path,
        help="Optional path to store the token JSON when --exchange-token is used.",
    )
    return parser.parse_args(argv)


def main(argv: Optional[list[str]] = None) -> None:
    args = parse_args(argv)
    redirect_uri = args.redirect_uri

    host = args.host or "127.0.0.1"
    port = args.port

    if redirect_uri:
        parsed = urllib.parse.urlparse(redirect_uri)
        if parsed.hostname:
            host = parsed.hostname
        if parsed.port is not None:
            port = parsed.port
        elif port is None:
            port = 80 if parsed.scheme == "http" else 443
    else:
        if port is None:
            port = 8989

    port = pick_free_port(port)

    if not redirect_uri:
        redirect_uri = f"http://{host}:{port}/login"

    scopes = args.scopes or DEFAULT_SCOPES
    state = args.state or token_urlsafe(16)
    code_verifier = args.code_verifier or token_urlsafe(64)
    code_challenge = base64.urlsafe_b64encode(
        hashlib.sha256(code_verifier.encode("utf-8")).digest()
    ).rstrip(b"=").decode("ascii")

    auth_url = build_authorize_url(
        client_id=args.client_id,
        redirect_uri=redirect_uri,
        scopes=scopes,
        state=state,
        show_dialog=args.show_dialog,
        code_challenge=code_challenge,
    )

    print("Authorization URL:")
    print(auth_url)
    print(f"\nState: {state}")
    print(f"Code verifier: {code_verifier}")
    print(f"Redirect URI: {redirect_uri}")

    if args.open_browser:
        print("\nOpening browser...")
        webbrowser.open(auth_url)

    if args.no_server:
        return

    try:
        params = wait_for_redirect(host, port, args.timeout)
    except TimeoutError as exc:
        print(exc, file=sys.stderr)
        sys.exit(1)

    if state and params.get("state") != state:
        print(
            f"Warning: state mismatch (expected {state}, got {params.get('state')})",
            file=sys.stderr,
        )

    if args.json:
        output = params.copy()
        output["state_expected"] = state
        output["code_verifier"] = code_verifier
        print(json.dumps(output, indent=2))
    else:
        for key, value in params.items():
            print(f"{key}={value}")
        print(f"state_expected={state}")
        print(f"code_verifier={code_verifier}")

    if args.exchange_token:
        auth_code = params.get("code")
        if not auth_code:
            print("No authorization code received; cannot exchange token.", file=sys.stderr)
            sys.exit(1)
        token_payload = urllib.parse.urlencode(
            {
                "grant_type": "authorization_code",
                "code": auth_code,
                "client_id": args.client_id,
                "redirect_uri": redirect_uri,
                "code_verifier": code_verifier,
            }
        ).encode("utf-8")
        token_request = urllib_request.Request(
            "https://accounts.spotify.com/api/token",
            data=token_payload,
            headers={"Content-Type": "application/x-www-form-urlencoded"},
        )
        try:
            with urllib_request.urlopen(token_request) as resp:
                token_data = json.load(resp)
        except urllib_error.HTTPError as err:
            detail = err.read().decode("utf-8", "ignore")
            print(
                f"Token exchange failed ({err.code}): {detail or err.reason}",
                file=sys.stderr,
            )
            sys.exit(1)
        if args.save_token:
            args.save_token.parent.mkdir(parents=True, exist_ok=True)
            args.save_token.write_text(json.dumps(token_data, indent=2), encoding="utf-8")
            print(f"Token saved to {args.save_token}")
        else:
            print(json.dumps(token_data, indent=2))


if __name__ == "__main__":
    main()
