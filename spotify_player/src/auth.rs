use crate::config;
use anyhow::{bail, Result};
use librespot_core::{authentication::Credentials, cache::Cache, config::SessionConfig, Session};
use librespot_oauth::OAuthClientBuilder;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

pub const SPOTIFY_CLIENT_ID: &str = "65b708073fc0480ea92a077233ca87bd";
// based on https://developer.spotify.com/documentation/web-api/concepts/scopes#list-of-scopes
const OAUTH_SCOPES: &[&str] = &[
    // Spotify Connect
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    // Playback
    "app-remote-control",
    "streaming",
    // Playlists
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
    // Follow
    "user-follow-modify",
    "user-follow-read",
    // Listening History
    "user-read-playback-position",
    "user-top-read",
    "user-read-recently-played",
    // Library
    "user-library-modify",
    "user-library-read",
    // Users
    "user-personalized",
];

#[derive(Clone)]
pub struct AuthConfig {
    pub cache: Cache,
    pub session_config: SessionConfig,
    pub login_redirect_uri: String,
    pub cache_folder: PathBuf,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            cache: Cache::new(None::<String>, None, None, None).unwrap(),
            session_config: SessionConfig::default(),
            login_redirect_uri: "http://127.0.0.1:8989/login".to_string(),
            cache_folder: dirs_next::home_dir().unwrap_or_default(),
        }
    }
}

impl AuthConfig {
    /// Create a `librespot::Session` from authentication configs
    pub fn session(&self) -> Session {
        Session::new(self.session_config.clone(), Some(self.cache.clone()))
    }

    pub fn new(configs: &config::Configs) -> Result<AuthConfig> {
        let audio_cache_folder = if configs.app_config.device.audio_cache {
            Some(configs.cache_folder.join("audio"))
        } else {
            None
        };

        let cache = Cache::new(
            Some(configs.cache_folder.clone()),
            None,
            audio_cache_folder,
            None,
        )?;

        Ok(AuthConfig {
            cache,
            session_config: configs.app_config.session_config(),
            login_redirect_uri: configs.app_config.login_redirect_uri.clone(),
            cache_folder: configs.cache_folder.clone(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredOAuthToken {
    refresh_token: String,
}

/// Get Spotify credentials to authenticate the application
///
/// # Args
/// - `auth_config`: authentication configuration
/// - `reauth`: whether to re-authenticate the application if no cached credentials are found
// - `use_cached`: whether to use cached credentials if available
pub fn get_creds(auth_config: &AuthConfig, reauth: bool, use_cached: bool) -> Result<Credentials> {
    let token_path = auth_config.cache_folder.join("oauth_token.json");

    if use_cached {
        if let Ok(contents) = fs::read_to_string(&token_path) {
            if let Ok(stored) = serde_json::from_str::<StoredOAuthToken>(&contents) {
                if !stored.refresh_token.is_empty() {
                    tracing::info!("Attempting to refresh OAuth token from cache");
                    let oauth_client = OAuthClientBuilder::new(
                        SPOTIFY_CLIENT_ID,
                        &auth_config.login_redirect_uri,
                        OAUTH_SCOPES.to_vec(),
                    )
                    .build()?;

                    match oauth_client.refresh_token(&stored.refresh_token) {
                        Ok(token) => {
                            let updated = StoredOAuthToken {
                                refresh_token: token.refresh_token.clone(),
                            };
                            if let Err(err) =
                                fs::write(&token_path, serde_json::to_string(&updated)?)
                            {
                                tracing::warn!("Failed to update cached refresh token: {err:#}");
                            }

                            return Ok(Credentials::with_access_token(token.access_token));
                        }
                        Err(err) => {
                            tracing::warn!("Refresh token was rejected: {err:#}");
                        }
                    }
                }
            }
        }
    }

    let msg = "No cached credentials found, please authenticate the application first.";
    if !reauth {
        bail!(msg);
    }

    eprintln!("{msg}");
    let oauth_client = OAuthClientBuilder::new(
        SPOTIFY_CLIENT_ID,
        &auth_config.login_redirect_uri,
        OAUTH_SCOPES.to_vec(),
    )
    .open_in_browser()
    .build()?;
    let token = oauth_client.get_access_token()?;

    let stored = StoredOAuthToken {
        refresh_token: token.refresh_token.clone(),
    };
    if let Err(err) = fs::create_dir_all(&auth_config.cache_folder) {
        tracing::warn!("Failed to create cache folder: {err:#}");
    }
    if let Err(err) = fs::write(&token_path, serde_json::to_string(&stored)?) {
        tracing::warn!("Failed to cache refresh token: {err:#}");
    }

    Ok(Credentials::with_access_token(token.access_token))
}
