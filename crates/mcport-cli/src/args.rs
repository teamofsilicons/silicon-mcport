use clap::{Args, Parser, Subcommand, ValueEnum};

const LINKS: &str = "Repository: https://github.com/teamofsilicons/silicon-mcport\nDocumentation: https://github.com/teamofsilicons/silicon-mcport/tree/main/docs\nRust package: https://crates.io/crates/mcport-client\n\nStart with: mcport iam --json, then mcport login <app-bound-slt>.\nUse mcport <service> --help to discover commands and examples.\nApplication login uses IAM; account connect handles upstream MCP authorization.";

#[derive(Debug, Parser)]
#[command(name = "mcport", version, about = "Configure MCP connections and use their tools from any authorized machine", long_about = None, after_help = LINKS, propagate_version = true)]
pub struct Cli {
    /// Print complete machine-readable JSON, including structured errors.
    #[arg(long, global = true)]
    pub json: bool,
    /// Select an isolated test environment; requires that environment's credentials.
    #[arg(long = "test", global = true)]
    pub test_id: Option<String>,
    /// MCPort backend URL. Defaults to saved configuration, then http://127.0.0.1:4380.
    #[arg(long = "backend", env = "MCPORT_URL", global = true)]
    pub url: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Discover IAM login details before signing in. Example: mcport iam --json.
    Iam,
    /// Exchange an app-bound short-lived token, or inspect login status.
    #[command(args_conflicts_with_subcommands = true)]
    Login(LoginArgs),
    /// Revoke the current application session and remove its local credentials.
    Logout,
    /// Inspect or switch locally stored account and organization sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// Configure, inspect and share saved MCP connections.
    #[command(subcommand)]
    Connection(ConnectionCommand),
    /// Discover tool schemas, execute tools and change allowed tools.
    #[command(subcommand)]
    Tool(ToolCommand),
    /// Authorize or disconnect the upstream account for a connection.
    #[command(subcommand)]
    Account(AccountCommand),
    /// Grant, list or revoke explicit access to a connection.
    #[command(subcommand)]
    Access(AccessCommand),
    /// Register and manage machines serving local MCPs.
    #[command(subcommand)]
    Host(HostCommand),
    /// Read the resources and templates exposed by an MCP.
    #[command(subcommand)]
    Resource(ResourceCommand),
    /// Discover and expand MCP prompts.
    #[command(subcommand)]
    Prompt(PromptCommand),
    /// Complete MCP prompt or resource-template arguments.
    #[command(subcommand)]
    Completion(CompletionCommand),
    /// Inspect activity and cancel an in-progress operation.
    #[command(subcommand)]
    Activity(ActivityCommand),
    /// List or save downloadable content from an authorized invocation.
    #[command(subcommand)]
    Asset(AssetCommand),
    /// Manage the local daemon serving registered MCPs.
    #[command(subcommand)]
    Daemon(DaemonCommand),
    /// View local settings or change home, backend URL and telemetry.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Submit a bug report, optionally with a pull request containing a fix.
    #[command(
        after_help = "Example: mcport report 'Tool call fails after refresh' --pr https://github.com/teamofsilicons/silicon-mcport/pull/1\nReports without a PR are welcome. Include reproduction details without credentials."
    )]
    Report {
        message: String,
        #[arg(long)]
        pr: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// App-bound SLT obtained from the official IAM CLI or website.
    pub slt: Option<String>,
    #[command(subcommand)]
    pub action: Option<LoginAction>,
}

#[derive(Debug, Subcommand)]
pub enum LoginAction {
    /// Verify the selected session. Example: mcport login status --json.
    Status,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List saved identities for the selected backend and test environment.
    Ls,
    /// Select a previously authenticated principal; does not copy credentials.
    Use {
        principal: String,
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Transport {
    Http,
    Stdio,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AuthMode {
    None,
    Shared,
    PerUser,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Visibility {
    Private,
    Org,
    Invited,
}

impl Transport {
    pub fn value(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Stdio => "stdio",
        }
    }
}
impl AuthMode {
    pub fn value(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Shared => "shared",
            Self::PerUser => "per-user",
        }
    }
}
impl Visibility {
    pub fn value(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Org => "org",
            Self::Invited => "invited",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum ConnectionCommand {
    /// Approve a centrally created local connection on this execution host before it may run.
    Register {
        connection: String,
        /// Local-only process environment. Repeat KEY=VALUE entries; credentials are never uploaded.
        #[arg(long = "env")]
        environment: Vec<String>,
    },
    /// Create a connection. Example: mcport connection new docs --transport http --url https://example.com/mcp.
    New {
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, value_enum)]
        transport: Transport,
        /// MCP endpoint URL (not the MCPort backend).
        #[arg(long = "url")]
        endpoint: Option<String>,
        /// Registered local host name or ID. Required for stdio or local HTTP.
        #[arg(long)]
        host: Option<String>,
        /// Executable to launch on the registered host; never interpreted by a shell.
        #[arg(long)]
        command: Option<String>,
        /// One process argument; repeat for multiple arguments.
        #[arg(long = "arg", allow_hyphen_values = true)]
        arguments: Vec<String>,
        /// Process environment entry KEY=VALUE. Do not place secrets in shell history.
        #[arg(long = "env")]
        environment: Vec<String>,
        #[arg(long, value_enum, default_value = "none")]
        auth: AuthMode,
        #[arg(long, value_enum, default_value = "private")]
        visibility: Visibility,
    },
    /// List connections currently visible to the selected identity.
    Ls,
    /// Show a connection's current configuration without credentials.
    Show { connection: String },
    /// Change owned connection metadata and visibility.
    Set {
        connection: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, value_enum)]
        visibility: Option<Visibility>,
    },
    /// Delete an owned connection and revoke its access.
    Rm { connection: String },
}

#[derive(Debug, Subcommand)]
pub enum ToolCommand {
    /// Discover enabled tools and their schemas.
    Ls {
        connection: String,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one tool's required input and output schema before calling it.
    Show { connection: String, tool: String },
    /// Invoke a tool once. Inputs: JSON, @file.json or - for stdin. Mutations are never automatically retried.
    Call {
        connection: String,
        tool: String,
        #[arg(long, default_value = "{}", allow_hyphen_values = true)]
        input: String,
        /// Stable key for one logical call; reusing it with different inputs is rejected.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Maximum server execution time in milliseconds.
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Enable/disable a tool for everyone or one principal; shared restrictions always apply.
    Set {
        connection: String,
        tool: String,
        #[arg(long, action = clap::ArgAction::Set)]
        enabled: bool,
        #[arg(long)]
        principal: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Connect upstream credentials or start provider consent; distinct from MCPort IAM login.
    Connect {
        connection: String,
        /// Load provider configuration from JSON/@file/-; otherwise prompt for a protected token when required.
        #[arg(long, allow_hyphen_values = true)]
        input: Option<String>,
        /// Read a bearer token without terminal echo rather than starting provider OAuth.
        #[arg(long)]
        token: bool,
        /// Pre-registered provider OAuth client ID when dynamic registration is unavailable.
        #[arg(long)]
        client_id: Option<String>,
    },
    /// Disconnect the current caller's upstream account (shared account requires connection ownership).
    Disconnect { connection: String },
    /// Inspect upstream account status without retrieving its credentials.
    Show { connection: String },
}

#[derive(Debug, Subcommand)]
pub enum AccessCommand {
    /// Invite a Carbon or Silicon to use the connection.
    New {
        connection: String,
        #[arg(long)]
        principal: String,
    },
    /// List direct invitations for a connection you own.
    Ls { connection: String },
    /// Revoke a direct invitation.
    Rm {
        connection: String,
        #[arg(long)]
        principal: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// Register this machine and start its background local-MCP daemon.
    New { name: String },
    /// List visible registered execution hosts.
    Ls,
    /// Show host connectivity and ownership.
    Show { host: String },
    /// Revoke a host and disconnect its runner.
    Rm { host: String },
}

#[derive(Debug, Subcommand)]
pub enum ResourceCommand {
    /// List resources the MCP makes available.
    Ls {
        connection: String,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// List URI templates for parameterized resources.
    Templates {
        connection: String,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read a resource URI, preserving text, media and metadata.
    Read { connection: String, uri: String },
}

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    /// List available prompts and argument descriptions.
    Ls {
        connection: String,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Expand a prompt with its arguments (JSON/@file/-).
    Get {
        connection: String,
        prompt: String,
        #[arg(long, default_value = "{}", allow_hyphen_values = true)]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ActivityCommand {
    /// List recent invocations visible to the caller.
    Ls {
        #[arg(long)]
        connection: Option<String>,
    },
    /// Inspect one invocation's status and result.
    Show { id: String },
    /// Request cancellation; completed upstream effects cannot be undone.
    Cancel { id: String },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon for registered hosts in this local context.
    Start,
    /// Check whether the local daemon is running.
    Status,
    /// Stop this context's daemon without deleting registered hosts.
    Stop,
    /// Run the daemon in the foreground (used by background launch).
    #[command(hide = true)]
    Run {
        #[arg(long)]
        registry: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Select an existing directory as the base home for this CLI context.
    Home { location: std::path::PathBuf },
    /// Print non-secret local settings and storage location.
    Show,
    /// Change a setting. Example: mcport config set telemetry false.
    Set { key: String, value: String },
}

#[derive(Debug, Subcommand)]
pub enum AssetCommand {
    /// List embedded downloadable assets in one invocation's result.
    Ls { call: String },
    /// Save one asset to a new private file; existing files are never replaced.
    Get {
        call: String,
        index: u32,
        #[arg(long)]
        output: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum CompletionCommand {
    /// Supply full MCP completion params as JSON, @file or - for stdin.
    Get {
        connection: String,
        #[arg(long, allow_hyphen_values = true)]
        input: String,
    },
}
