use clap::{Args, Parser, Subcommand, ValueEnum};

const LINKS: &str = "Repository: https://github.com/teamofsilicons/silicon-mcport\nDocumentation: https://github.com/teamofsilicons/silicon-mcport/tree/main/docs\nRust package (publication pending): https://crates.io/crates/mcport-client\n\nExample: mcport iam --json\nThen obtain an app-bound SLT from IAM and run mcport login <app-bound-slt>.\nFresh profiles use https://backend.mcport.teamofsilicons.com; config set backend selects another deployment.\nRelated: mcport docs, mcport config set --help, mcport connection --help, mcport tool --help\nApplication login uses IAM; account connect handles upstream MCP authorization.";

#[derive(Debug, Parser)]
#[command(name = "mcport", version, about = "Configure MCP connections and use their tools from any authorized machine", long_about = None, after_help = LINKS, propagate_version = true)]
pub struct Cli {
    /// Print complete machine-readable JSON, including structured errors.
    #[arg(long, global = true)]
    pub json: bool,
    /// Select an isolated test environment; requires that environment's credentials.
    #[arg(long = "test", global = true)]
    pub test_id: Option<String>,
    /// Backend URL; --backend overrides MCPORT_URL, then saved configuration, then https://backend.mcport.teamofsilicons.com.
    #[arg(long = "backend", env = "MCPORT_URL", global = true)]
    pub url: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read bundled usage or development documentation without logging in.
    #[command(
        after_help = "Example: mcport docs development\nRelated: mcport --help, mcport config --help"
    )]
    Docs {
        /// Bundled guide to read; development also accepts the alias dev.
        #[arg(value_enum, default_value = "usage")]
        topic: DocsTopic,
    },
    /// Discover the app ID and official IAM login details before signing in.
    #[command(
        after_help = "Example: mcport iam --json\nUses the selected backend. Obtain an app-bound SLT from official IAM for the returned app ID.\nRelated: mcport config set --help, mcport login --help"
    )]
    Iam,
    /// Exchange an app-bound short-lived token, or inspect login status.
    #[command(
        args_conflicts_with_subcommands = true,
        after_help = "Example: mcport login '<app-bound-slt>'\nObtain the SLT from official IAM for the app reported by mcport iam.\nRelated: mcport iam, mcport login status, mcport session --help, mcport logout"
    )]
    Login(LoginArgs),
    /// Revoke the current application session and remove its local credentials.
    #[command(
        after_help = "Example: mcport logout\nApplies to the selected backend, environment and session.\nRelated: mcport login status, mcport session ls, mcport login --help"
    )]
    Logout,
    /// Inspect or switch locally stored account and organization sessions.
    #[command(
        subcommand,
        after_help = "Example: mcport session ls --json\nSelect an existing identity with session use; sign in a new identity with login.\nRelated: mcport session use --help, mcport login --help"
    )]
    Session(SessionCommand),
    /// Configure, inspect and share saved MCP connections.
    #[command(
        subcommand,
        after_help = "Example: mcport connection new docs --transport http --url https://provider.example/mcp\nFor local HTTP or stdio, register a host first and supply --host.\nRelated: mcport connection new --help, mcport host --help, mcport account --help, mcport access --help"
    )]
    Connection(ConnectionCommand),
    /// Discover tool schemas, execute tools and change allowed tools.
    #[command(
        subcommand,
        after_help = "Example: mcport tool ls notes\nInspect a tool with show before supplying its arguments to call.\nRelated: mcport tool show --help, mcport tool call --help, mcport tool set --help"
    )]
    Tool(ToolCommand),
    /// Authorize or disconnect the upstream account for a connection.
    #[command(
        subcommand,
        after_help = "Example: mcport account connect notes\nProvider authorization is separate from IAM login; shared accounts require connection ownership.\nRelated: mcport account connect --help, mcport account show --help, mcport login --help"
    )]
    Account(AccountCommand),
    /// Grant, list or revoke explicit access to a connection.
    #[command(
        subcommand,
        after_help = "Example: mcport access new notes --principal si:researcher\nThe connection owner manages invitations; the invitee signs in independently.\nRelated: mcport connection set --help, mcport access ls --help, mcport tool set --help"
    )]
    Access(AccessCommand),
    /// Register and manage machines serving local MCPs.
    #[command(
        subcommand,
        after_help = "Example: mcport host new laptop\nRun on the machine that will execute the local MCP; host creation starts its daemon.\nRelated: mcport connection new --help, mcport daemon --help"
    )]
    Host(HostCommand),
    /// Read the resources and templates exposed by an MCP.
    #[command(
        subcommand,
        after_help = "Example: mcport resource ls notes\nUse listed URIs with read; use templates to discover parameterized URIs.\nRelated: mcport resource read --help, mcport resource templates --help, mcport completion get --help"
    )]
    Resource(ResourceCommand),
    /// Discover and expand MCP prompts.
    #[command(
        subcommand,
        after_help = "Example: mcport prompt ls notes\nInspect argument descriptions in the list before expanding a prompt with get.\nRelated: mcport prompt get --help, mcport completion get --help"
    )]
    Prompt(PromptCommand),
    /// Complete MCP prompt or resource-template arguments.
    #[command(
        subcommand,
        after_help = "Example: mcport completion get notes --input @completion.json\nDiscover prompt names or resource-template URIs before building the completion params.\nRelated: mcport completion get --help, mcport prompt ls --help, mcport resource templates --help"
    )]
    Completion(CompletionCommand),
    /// Inspect activity and cancel an in-progress operation.
    #[command(
        subcommand,
        after_help = "Example: mcport activity ls --connection notes\nUse the returned invocation ID with show, cancel or asset ls.\nRelated: mcport activity show --help, mcport activity cancel --help, mcport asset --help"
    )]
    Activity(ActivityCommand),
    /// List or save downloadable content from an authorized invocation.
    #[command(
        subcommand,
        after_help = "Example: mcport asset ls <call-id>\nSelect an index from the list, then save it with asset get.\nRelated: mcport asset get --help, mcport activity ls --help, mcport activity show --help"
    )]
    Asset(AssetCommand),
    /// Manage the local daemon serving registered MCPs.
    #[command(
        subcommand,
        after_help = "Example: mcport daemon status\nCommands use host registries in the selected local backend and test context.\nRelated: mcport host new --help, mcport daemon start --help, mcport daemon stop --help"
    )]
    Daemon(DaemonCommand),
    /// View local settings or change home, backend URL and telemetry.
    #[command(
        subcommand,
        after_help = "Example: mcport config set backend https://your-mcport-backend.example\nSettings select the backend and telemetry; home selects an existing storage base.\nRelated: mcport config show, mcport config set --help, mcport config home --help"
    )]
    Config(ConfigCommand),
    /// Submit a bug report, optionally with a pull request containing a fix.
    #[command(
        after_help = "Example: mcport report 'Tool call fails after refresh' --pr https://github.com/teamofsilicons/silicon-mcport/pull/1\nReports without a PR are welcome. Include reproduction details without credentials.\nRelated: mcport activity show --help, mcport docs development"
    )]
    Report {
        /// What failed and the steps needed to reproduce it; omit credentials.
        message: String,
        /// URL of an optional pull request with a proposed fix.
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
    /// Verify the selected session and show its identity and organization.
    #[command(
        after_help = "Example: mcport login status --json\nRelated: mcport session ls, mcport session use --help, mcport login --help"
    )]
    Status,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// List saved identities for the selected backend and test environment.
    #[command(
        after_help = "Example: mcport session ls --json\nRelated: mcport session use --help, mcport login status"
    )]
    Ls,
    /// Select a previously authenticated principal; does not copy credentials.
    #[command(
        after_help = "Example: mcport session use si:researcher --org <org-id>\nCopy principal and organization IDs from session ls.\nRelated: mcport session ls, mcport login status, mcport login --help"
    )]
    Use {
        /// Principal ID from session ls, such as si:researcher or c:alice.
        principal: String,
        /// Organization ID from session ls when selecting an organization session.
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Transport {
    /// Connect to an HTTP MCP endpoint; add --host for a host-local endpoint.
    Http,
    /// Launch an explicitly registered process on a local execution host.
    Stdio,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AuthMode {
    /// Use the MCP without a managed provider account grant.
    None,
    /// Authorized callers use the connection owner's provider account.
    Shared,
    /// Each caller must connect their own provider account.
    PerUser,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Visibility {
    /// Limit access to the connection owner.
    Private,
    /// Allow eligible identities in the connection's organization.
    Org,
    /// Allow the owner and explicitly invited principals.
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
    #[command(
        after_help = "Example: mcport connection register desktop\nRun as the host and connection owner on the registered execution machine.\nRelated: mcport connection show --help, mcport host show --help, mcport daemon status"
    )]
    Register {
        /// Local connection name or ID, as shown by connection ls.
        connection: String,
        /// Local-only process environment. Repeat KEY=VALUE entries; credentials are never uploaded.
        #[arg(long = "env")]
        environment: Vec<String>,
    },
    /// Create a remote or host-local MCP connection.
    #[command(
        after_help = "Example: mcport connection new docs --transport http --url https://provider.example/mcp --auth none\nLocal stdio: mcport connection new files --host laptop --transport stdio --command /absolute/path/to/server --arg /absolute/path/to/config\nLocal connections created here are also registered on their host.\nRelated: mcport host new --help, mcport account connect --help, mcport access new --help, mcport tool ls --help"
    )]
    New {
        /// Connection name to use in later commands.
        name: String,
        /// Optional description displayed with the connection.
        #[arg(long, default_value = "")]
        description: String,
        /// HTTP endpoint or local process transport.
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
        /// Provider account mode; shared and per-user accounts connect separately.
        #[arg(long, value_enum, default_value = "none")]
        auth: AuthMode,
        /// Who may access the connection, subject to its tool restrictions.
        #[arg(long, value_enum, default_value = "private")]
        visibility: Visibility,
    },
    /// List connections currently visible to the selected identity.
    #[command(
        after_help = "Example: mcport connection ls --json\nRelated: mcport connection show --help, mcport connection new --help, mcport tool ls --help"
    )]
    Ls,
    /// Show a connection's current configuration without credentials.
    #[command(
        after_help = "Example: mcport connection show notes\nRelated: mcport connection ls, mcport connection set --help, mcport account show --help"
    )]
    Show {
        /// Connection name or ID from connection ls.
        connection: String,
    },
    /// Change owned connection metadata and visibility.
    #[command(
        after_help = "Example: mcport connection set notes --name team-notes --visibility invited\nSupply at least one of --name, --description or --visibility.\nRelated: mcport connection show --help, mcport access new --help, mcport connection rm --help"
    )]
    Set {
        /// Owned connection name or ID from connection ls.
        connection: String,
        /// New connection name.
        #[arg(long)]
        name: Option<String>,
        /// New description; an empty string clears it.
        #[arg(long)]
        description: Option<String>,
        /// New access visibility; use access new for explicit invitations.
        #[arg(long, value_enum)]
        visibility: Option<Visibility>,
    },
    /// Delete an owned connection and revoke its access.
    #[command(
        after_help = "Example: mcport connection rm notes\nAlso removes saved provider grants and invalidates pending work for this connection.\nRelated: mcport connection show --help, mcport access rm --help, mcport account disconnect --help"
    )]
    Rm {
        /// Owned connection name or ID from connection ls.
        connection: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ToolCommand {
    /// Discover enabled tools and their schemas.
    #[command(
        after_help = "Example: mcport tool ls notes\nIf the response has nextCursor, pass it unchanged with --cursor to read another page.\nRelated: mcport tool show --help, mcport tool call --help, mcport tool set --help"
    )]
    Ls {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous tool ls response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one tool's required input and output schema before calling it.
    #[command(
        after_help = "Example: mcport tool show notes search\nUse the returned input schema to build the JSON arguments for tool call.\nRelated: mcport tool ls --help, mcport tool call --help"
    )]
    Show {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact tool name from tool ls.
        tool: String,
    },
    /// Invoke a tool once. Inputs: JSON, @file.json or - for stdin. Mutations are never automatically retried.
    #[command(
        after_help = "Example: mcport tool call notes search --input '{\"query\":\"release\"}'\nFile: mcport tool call notes search --input @request.json\nStdin: printf '%s\\n' '{\"query\":\"release\"}' | mcport tool call notes search --input -\nCheck the tool schema first. Inspect activity if waiting stops or the outcome is unknown.\nRelated: mcport tool show --help, mcport activity ls --help, mcport activity cancel --help, mcport asset ls --help"
    )]
    Call {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact tool name from tool ls.
        tool: String,
        /// Tool arguments as a JSON object, @path to a JSON file, or - for stdin.
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
    #[command(
        after_help = "Example: mcport tool set notes delete_note --enabled false --principal si:researcher\nOwner only. Omit --principal to apply to everyone. A principal's enabled=true never overrides a global deny, and a policy does not grant connection access.\nRelated: mcport tool ls --help, mcport access new --help, mcport connection show --help"
    )]
    Set {
        /// Owned connection name or ID from connection ls.
        connection: String,
        /// Exact MCP tool name to permit or restrict.
        tool: String,
        /// Set true to allow or false to deny within the selected policy scope.
        #[arg(long, action = clap::ArgAction::Set)]
        enabled: bool,
        /// Limit this policy to one principal ID; omit for the global policy.
        #[arg(long)]
        principal: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Connect upstream credentials or start provider consent; distinct from MCPort IAM login.
    #[command(
        after_help = "Example: mcport account connect notes\nRemote OAuth returns a consent URL to open in a browser. A pre-registered public client ID is needed only when neither supported client metadata nor dynamic registration is available.\nManual HTTP credential: mcport account connect notes --input @credentials.json\nThe file may contain {\"kind\":\"bearer\",\"secret\":\"...\"} or {\"kind\":\"header\",\"header_name\":\"X-API-Key\",\"secret\":\"...\"}.\nRun local account setup on its host: HTTP accepts bearer/header; stdio accepts {\"kind\":\"env\",\"env\":{\"API_TOKEN\":\"...\"}}. Local shared mode with no input selects the host's existing application account; per-user mode requires its own credentials.\nRelated: mcport account show --help, mcport account disconnect --help, mcport connection new --help, mcport login --help"
    )]
    Connect {
        /// Connection name or ID with shared or per-user authentication.
        connection: String,
        /// Manual credential JSON object, @protected-file, or - for stdin; use separately from --token/--client-id.
        #[arg(long, allow_hyphen_values = true)]
        input: Option<String>,
        /// Prompt for a bearer token without terminal echo; use separately from --input/--client-id.
        #[arg(long)]
        token: bool,
        /// Pre-registered public client ID for remote OAuth; no manual credentials or local hosts.
        #[arg(long)]
        client_id: Option<String>,
    },
    /// Disconnect the current caller's upstream account (shared account requires connection ownership).
    #[command(
        after_help = "Example: mcport account disconnect notes\nShared mode disconnects the shared grant; per-user mode disconnects only your grant.\nRelated: mcport account show --help, mcport account connect --help"
    )]
    Disconnect {
        /// Connection name or ID from connection ls.
        connection: String,
    },
    /// Inspect upstream account status without retrieving its credentials.
    #[command(
        after_help = "Example: mcport account show notes\nSaved credential status alone does not prove the provider will accept it.\nRelated: mcport account connect --help, mcport account disconnect --help, mcport tool ls --help"
    )]
    Show {
        /// Connection name or ID from connection ls.
        connection: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccessCommand {
    /// Invite a Carbon or Silicon to use the connection.
    #[command(
        after_help = "Example: mcport access new notes --principal si:researcher\nOnly the connection owner can grant an invitation. Tool restrictions and provider account requirements still apply.\nRelated: mcport access ls --help, mcport connection set --help, mcport tool set --help"
    )]
    New {
        /// Owned connection name or ID from connection ls.
        connection: String,
        /// Canonical Carbon or Silicon principal ID, such as c:alice or si:researcher.
        #[arg(long)]
        principal: String,
    },
    /// List direct invitations for a connection you own.
    #[command(
        after_help = "Example: mcport access ls notes\nRelated: mcport access new --help, mcport access rm --help, mcport connection show --help"
    )]
    Ls {
        /// Owned connection name or ID from connection ls.
        connection: String,
    },
    /// Revoke a direct invitation.
    #[command(
        after_help = "Example: mcport access rm notes --principal si:researcher\nOrganization visibility may still allow access; inspect connection show before changing visibility.\nRelated: mcport access ls --help, mcport connection set --help, mcport tool set --help"
    )]
    Rm {
        /// Owned connection name or ID from connection ls.
        connection: String,
        /// Invited principal ID from access ls.
        #[arg(long)]
        principal: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// Register this machine and start its background local-MCP daemon.
    #[command(
        after_help = "Example: mcport host new laptop\nNext create a local connection with --host laptop. The MCP application must be available on this machine.\nRelated: mcport connection new --help, mcport host show --help, mcport daemon status"
    )]
    New {
        /// Name for this execution host in the current backend and environment.
        name: String,
    },
    /// List visible registered execution hosts.
    #[command(
        after_help = "Example: mcport host ls --json\nRelated: mcport host show --help, mcport host new --help, mcport daemon status"
    )]
    Ls,
    /// Show host connectivity and ownership.
    #[command(
        after_help = "Example: mcport host show laptop\nRelated: mcport host ls, mcport daemon status, mcport daemon start"
    )]
    Show {
        /// Host name or ID from host ls.
        host: String,
    },
    /// Revoke a host and disconnect its runner.
    #[command(
        after_help = "Example: mcport host rm laptop\nUse daemon stop when you only want to pause local execution without revoking the host.\nRelated: mcport host show --help, mcport daemon stop, mcport connection rm --help"
    )]
    Rm {
        /// Owned host name or ID from host ls.
        host: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ResourceCommand {
    /// List resources the MCP makes available.
    #[command(
        after_help = "Example: mcport resource ls notes\nPass nextCursor unchanged with --cursor for another page. Read a returned URI with resource read.\nRelated: mcport resource read --help, mcport resource templates --help"
    )]
    Ls {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous resource ls response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// List URI templates for parameterized resources.
    #[command(
        after_help = "Example: mcport resource templates notes\nFill a returned URI template before passing the resulting URI to resource read.\nRelated: mcport resource read --help, mcport completion get --help, mcport resource ls --help"
    )]
    Templates {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous resource templates response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read a resource URI, preserving text, media and metadata.
    #[command(
        after_help = "Example: mcport resource read notes 'notes://recent'\nUse a URI advertised by the MCP or constructed from one of its resource templates.\nRelated: mcport resource ls --help, mcport resource templates --help, mcport asset ls --help"
    )]
    Read {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact MCP resource URI, not a local output filename.
        uri: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    /// List available prompts and argument descriptions.
    #[command(
        after_help = "Example: mcport prompt ls notes\nPass nextCursor unchanged with --cursor for another page.\nRelated: mcport prompt get --help, mcport completion get --help"
    )]
    Ls {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous prompt ls response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Expand a prompt with its arguments (JSON/@file/-).
    #[command(
        after_help = "Example: mcport prompt get notes summarize --input '{\"text\":\"Release notes\"}'\nUse a prompt and arguments advertised by prompt ls; the result is the MCP's expanded prompt content.\nRelated: mcport prompt ls --help, mcport completion get --help"
    )]
    Get {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact prompt name from prompt ls.
        prompt: String,
        /// Prompt argument JSON object (string values), @path to a JSON file, or - for stdin.
        #[arg(long, default_value = "{}", allow_hyphen_values = true)]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ActivityCommand {
    /// List recent invocations visible to the caller.
    #[command(
        after_help = "Example: mcport activity ls --connection notes\nUse a returned invocation ID with activity show, activity cancel or asset ls.\nRelated: mcport activity show --help, mcport activity cancel --help, mcport asset ls --help"
    )]
    Ls {
        /// Restrict activity to a connection name or ID; omit to include all visible calls.
        #[arg(long)]
        connection: Option<String>,
    },
    /// Inspect one invocation's status and result.
    #[command(
        after_help = "Example: mcport activity show <call-id> --json\nRelated: mcport activity ls --help, mcport activity cancel --help, mcport asset ls --help"
    )]
    Show {
        /// Invocation ID returned by a call or activity ls.
        id: String,
    },
    /// Request cancellation; completed upstream effects cannot be undone.
    #[command(
        after_help = "Example: mcport activity cancel <call-id>\nInspect the resulting status; cancellation is best effort and does not roll back provider effects.\nRelated: mcport activity ls --help, mcport activity show --help"
    )]
    Cancel {
        /// Invocation ID from activity ls or the original operation response.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon for registered hosts in this local context.
    #[command(
        after_help = "Example: mcport daemon start\nUses existing registries in the selected backend and test context; register a new host with host new.\nRelated: mcport daemon status, mcport host new --help, mcport connection register --help"
    )]
    Start,
    /// Check whether the local daemon is running.
    #[command(
        after_help = "Example: mcport daemon status\nReports local runner state and recent gateway connectivity for this context's registered hosts.\nRelated: mcport daemon start, mcport daemon stop, mcport host show --help"
    )]
    Status,
    /// Stop this context's daemon without deleting registered hosts.
    #[command(
        after_help = "Example: mcport daemon stop\nLocal connections cannot execute through a stopped runner; use daemon start to resume.\nRelated: mcport daemon status, mcport daemon start, mcport host rm --help"
    )]
    Stop,
    /// Run the daemon in the foreground (used by background launch).
    #[command(hide = true)]
    Run {
        /// Absolute path to this host's protected registry.json file.
        #[arg(long)]
        registry: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Select an existing directory as the base home for this CLI context.
    #[command(
        after_help = "Example: mcport config home /existing/work-directory\nThe directory must exist. Sessions and host grants are not copied to the new home.\nRelated: mcport config show, mcport config set --help, mcport session ls"
    )]
    Home {
        /// Existing storage base; MCPort uses its .mcport/dir subdirectory.
        location: std::path::PathBuf,
    },
    /// Print non-secret local settings and storage location.
    #[command(
        after_help = "Example: mcport config show --json\nWithout a saved backend or override, the backend is https://backend.mcport.teamofsilicons.com.\nRelated: mcport config set --help, mcport config home --help"
    )]
    Show,
    /// Save the backend URL or enable/disable telemetry for this local home.
    #[command(
        after_help = "Example: mcport config set backend https://your-mcport-backend.example\nTelemetry: mcport config set telemetry false\nSupported keys: backend (aliases url and backend_url), telemetry. --backend and MCPORT_URL override the saved backend.\nRelated: mcport config show, mcport config home --help, mcport iam"
    )]
    Set {
        /// Setting name: backend (also url or backend_url), or telemetry.
        key: String,
        /// Backend URL (HTTPS, or HTTP on loopback), or true/false for telemetry.
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum AssetCommand {
    /// List embedded downloadable assets in one invocation's result.
    #[command(
        after_help = "Example: mcport asset ls <call-id>\nCopy the desired zero-based index and media type from the list before saving an asset.\nRelated: mcport activity ls --help, mcport activity show --help, mcport asset get --help"
    )]
    Ls {
        /// Your completed invocation ID from a call or activity ls.
        call: String,
    },
    /// Save one asset to a new private file; existing files are never replaced.
    #[command(
        after_help = "Example: mcport asset get <call-id> 0 --output ./result.bin\nChoose the index and filename extension from asset ls. The parent directory must exist; --json changes status output, not saved bytes.\nRelated: mcport asset ls --help, mcport activity show --help"
    )]
    Get {
        /// Your completed invocation ID, not a connection name.
        call: String,
        /// Zero-based asset index returned by asset ls for this invocation.
        index: u32,
        /// New output file path; parent directory must exist and files are never overwritten.
        #[arg(long)]
        output: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum CompletionCommand {
    /// Request provider suggestions for a prompt or resource-template argument.
    #[command(
        after_help = "Example: mcport completion get notes --input '{\"ref\":{\"type\":\"ref/prompt\",\"name\":\"summarize\"},\"argument\":{\"name\":\"text\",\"value\":\"hel\"}}'\nUse a prompt/argument advertised by prompt ls. For a resource template use ref {\"type\":\"ref/resource\",\"uri\":\"<advertised-uri-template>\"} with its variable name. The MCP must support completions.\nRelated: mcport prompt ls --help, mcport prompt get --help, mcport resource templates --help"
    )]
    Get {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Full MCP completion params object with ref and argument; JSON, @file or - for stdin, without a JSON-RPC envelope.
        #[arg(long, allow_hyphen_values = true)]
        input: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DocsTopic {
    /// Setup, connections, account authorization and everyday CLI workflows.
    Usage,
    /// Architecture, local development, configuration, testing and release limits.
    #[value(alias = "dev")]
    Development,
}
impl DocsTopic {
    pub fn name(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Development => "development",
        }
    }
    pub fn content(self) -> &'static str {
        match self {
            Self::Usage => include_str!("../README.md"),
            Self::Development => include_str!("../docs/development.md"),
        }
    }
}
