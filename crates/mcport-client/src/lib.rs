//! Primary Rust interface for Silicon MCPort.
//!
//! [`Client`] is stateless: request credentials are explicit, and no login,
//! refresh or mutation retry is implicit. Enable `local` for caller-owned
//! registry configuration and local daemon operations at explicit paths.
//!
//! ```no_run
//! # async fn example() -> Result<(), mcport_client::Error> {
//! use mcport_client::{Client, RequestContext};
//! let client = Client::new("http://127.0.0.1:4380")?;
//! let session = client.login(&RequestContext::default(), "app-bound-slt").await?;
//! let context = RequestContext::authenticated(&session.access_token);
//! let connections = client.connections(&context).await?;
//! # Ok(()) }
//! ```
pub use mcport_api::*;

#[cfg(feature = "local")]
pub mod local;
