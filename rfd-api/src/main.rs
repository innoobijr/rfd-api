// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use context::RfdContext;
use rfd_model::storage::postgres::PostgresStore;
use server::{server, ServerConfig};
use std::{
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
};
use tap::TapFallible;
use tracing_appender::non_blocking::NonBlocking;
use tracing_subscriber::EnvFilter;
use v_api::{
    endpoints::login::oauth::{
        github::GitHubOAuthProvider, google::GoogleOAuthProvider, OAuthProviderName,
    },
    ApiContext, VContext,
};
use v_model::storage::postgres::PostgresStore as VApiPostgressStore;

use crate::{
    config::{AppConfig, ServerLogFormat},
    initial_data::InitialData,
};

mod caller;
mod config;
mod context;
mod endpoints;
mod error;
mod initial_data;
mod permissions;
mod search;
mod secrets;
mod server;
mod util;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args();
    let _ = args.next();
    let config_path = args.next();

    let config = AppConfig::new(config_path.map(|path| vec![path]))?;

    let (writer, _guard) = if let Some(log_directory) = config.log_directory {
        let file_appender = tracing_appender::rolling::daily(log_directory, "rfd-api.log");
        tracing_appender::non_blocking(file_appender)
    } else {
        NonBlocking::new(std::io::stdout())
    };

    let subscriber = tracing_subscriber::fmt()
        .with_file(false)
        .with_line_number(false)
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(writer);

    match config.log_format {
        ServerLogFormat::Json => subscriber.json().init(),
        ServerLogFormat::Pretty => subscriber.pretty().init(),
    };

    tracing::info!("Initialized logger");

    let mut v_ctx = VContext::new(
        config.public_url.clone(),
        Arc::new(
            VApiPostgressStore::new(&config.database_url)
                .await
                .tap_err(|err| {
                    tracing::error!(?err, "Failed to establish initial database connection");
                })?,
        ),
        config.jwt,
        config.keys,
    )
    .await?;

    if let Some(github) = config.authn.oauth.github {
        v_ctx.insert_oauth_provider(
            OAuthProviderName::GitHub,
            Box::new(move || {
                Box::new(GitHubOAuthProvider::new(
                    github.device.client_id.clone(),
                    github.device.client_secret.clone(),
                    github.web.client_id.clone(),
                    github.web.client_secret.clone(),
                    None,
                ))
            }),
        );

        tracing::info!("Added GitHub OAuth provider");
    }

    if let Some(google) = config.authn.oauth.google {
        v_ctx.insert_oauth_provider(
            OAuthProviderName::Google,
            Box::new(move || {
                Box::new(GoogleOAuthProvider::new(
                    google.device.client_id.clone(),
                    google.device.client_secret.clone(),
                    google.web.client_id.clone(),
                    google.web.client_secret.clone(),
                    None,
                ))
            }),
        );

        tracing::info!("Added Google OAuth provider");
    }

    let context = RfdContext::new(
        config.public_url,
        Arc::new(
            PostgresStore::new(&config.database_url)
                .await
                .tap_err(|err| {
                    tracing::error!(?err, "Failed to establish initial database connection");
                })?,
        ),
        config.search,
        config.content,
        config.services,
        v_ctx,
    )
    .await?;

    tracing::info!("Configured server context");

    let init_data = InitialData::new(config.initial_mappers.map(|p| vec![p])).tap_err(|err| {
        tracing::error!(?err, "Failed to load initial data from configuration");
    })?;
    init_data
        .initialize(&context.v_ctx())
        .await
        .tap_err(|err| {
            tracing::error!(?err, "Failed to install initial data");
        })?;

    tracing::info!("Loaded initial data");

    tracing::debug!(?config.spec, "Spec configuration");

    let config = ServerConfig {
        context,
        server_address: SocketAddr::V4(SocketAddrV4::new(
            "0.0.0.0"
                .parse()
                .tap_err(|err| tracing::error!(?err, "Failed to parse server address"))?,
            config.server_port,
        )),
        spec_output: config.spec,
    };

    let server = server(config)
        .tap_err(|err| {
            tracing::error!(?err, "Failed to construct server");
        })
        .expect("Failed to construct server")
        .start();

    server?
        .await
        .tap_err(|err| {
            tracing::error!(?err, "Server exited with an error");
        })
        .expect("Failed to start server");

    tracing::error!("Server completed without an error");

    Ok(())
}
