//! Primary Rust interface for Silicon MCPort.
//!
//! [`Client`] is stateless: each request carries an explicit Silicon Accounts access
//! token for the `mcport` app, and no sign-in, refresh or mutation retry is implicit.
//!
//! - `accounts` (default feature): sign in as MCPort's public client: the device flow
//!   for Carbons, short-lived tokens for Silicons, refresh and sign-out.
//! - `session`: keep a sign-in in one explicit file with single-flight refresh.
//! - `local`: explicit host registries and the embedded host connector.
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use mcport_client::{Client, RequestContext};
//! use mcport_client::accounts::{APP_ID, DEFAULT_ACCOUNTS_URL, SignIn};
//! // A Silicon: SLT from `silicon-accounts login --app mcport -q`.
//! let slt = std::env::var("SLT")?;
//! let tokens = SignIn::new(DEFAULT_ACCOUNTS_URL, APP_ID)?.exchange_slt(&slt).await?;
//! let client = Client::new("https://api.mcport.teamofsilicons.com")?;
//! let context = RequestContext::authenticated(tokens.access_token.expose());
//! let connections = client.connections(&context).await?;
//! # Ok(()) }
//! ```
pub use mcport_api::*;

#[cfg(feature = "accounts")]
pub mod accounts;
#[cfg(feature = "local")]
pub mod local;
#[cfg(feature = "session")]
pub mod session;
