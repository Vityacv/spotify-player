 # OAuth Helpers

 ## Why this exists

 Spotify recently tightened OAuth restrictions. The built-in client is affected, so we added standalone scripts to reproduce the OAuth handshake and debug issues (redirect URIs, token exchange, etc).

 ## scripts/oauth_listener.py

 - Generate authorization URL (defaults to the same client-id and scopes the app uses).
 - Optional browser launch (`--open-browser`).
 - Runs a local HTTP server to capture the redirect (`--redirect-uri` defaults to http://127.0.0.1:8989/login).
 - Prints state, authorization code, and PKCE code verifier.
 - Optional `--exchange-token` to swap the auth code for an access+refresh token with Spotify’s token endpoint.
 - Optional `--save-token path` writes token JSON to the given path instead of stdout.

 Example:

 ```bash
 python scripts/oauth_listener.py \
   --client-id YOUR_CLIENT_ID \
   --redirect-uri http://127.0.0.1:8989/login \
   --open-browser \
   --exchange-token \
   --save-token /tmp/spotify-token.json \
   --json
 ```
