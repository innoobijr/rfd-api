use context::ApiContext;
use permissions::ApiPermission;
use rfd_model::{
    permissions::{Caller, Permissions},
    storage::postgres::PostgresStore,
    ApiUser, ApiUserToken,
};
use server::{server, ServerConfig};
use std::{
    error::Error,
    net::{SocketAddr, SocketAddrV4},
    sync::Arc,
};
use tracing_subscriber::EnvFilter;

use crate::{
    config::{AppConfig, ServerLogFormat},
    email_validator::DomainValidator,
    endpoints::login::{
        jwt::{google::GoogleOidcJwks, JwtProviderName},
        oauth::{google::GoogleOAuthProvider, OAuthProviderName},
    },
};

mod authn;
mod config;
mod context;
mod email_validator;
mod endpoints;
mod error;
mod permissions;
mod server;
mod util;

pub type ApiCaller = Caller<ApiPermission>;
pub type ApiPermissions = Permissions<ApiPermission>;
pub type User = ApiUser<ApiPermission>;
pub type UserToken = ApiUserToken<ApiPermission>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let config = AppConfig::new()?;

    let subscriber = tracing_subscriber::fmt()
        .with_file(false)
        .with_line_number(false)
        .with_env_filter(EnvFilter::from_default_env());

    match config.log_format {
        ServerLogFormat::Json => subscriber.json().init(),
        ServerLogFormat::Pretty => subscriber.pretty().init(),
    };

    let mut context = ApiContext::new(
        Arc::new(DomainValidator::new(vec![])),
        config.public_url,
        Arc::new(
            PostgresStore::new(&config.database_url)
                .await
                .map_err(|err| {
                    format!("Failed to establish initial database connection: {:?}", err)
                })?,
        ),
        config.permissions,
        config.jwt,
    )
    .await?;

    if let Some(google) = config.authn.oauth.google {
        context.insert_oauth_provider(
            OAuthProviderName::Google,
            Box::new(move || {
                Box::new(GoogleOAuthProvider::new(
                    google.client_id.clone(),
                    google.client_secret.clone(),
                ))
            }),
        )
    }

    if let Some(google) = config.authn.jwt.google {
        context.insert_jwks_provider(
            JwtProviderName::Google,
            Box::new(GoogleOidcJwks::new(google.issuer, google.well_known_uri)),
        )
    }

    tracing::debug!(?config.spec, "Spec configuration");

    let config = ServerConfig {
        context,
        server_address: SocketAddr::V4(SocketAddrV4::new("0.0.0.0".parse()?, config.server_port)),
        spec_output: config.spec,
    };

    let server = server(config)?.start();

    server.await?;

    Ok(())
}
