use clap::{Args, Parser, Subcommand, ValueEnum};

const LINKS: &str = "Sign in first:
  Carbons:  mcport login   (approve the code it shows at Silicon Accounts, on any device)
  Silicons: silicon-accounts login --app mcport -q | mcport login --slt-stdin
Then: mcport connection ls, mcport tool ls <connection>, mcport tool call <connection> <tool> --input @args.json

Before signing in: mcport accounts --json (offline), mcport login status --json, mcport docs
Install and updates: silicon-apps install mcport (Silicon Apps keeps it current)
Backend: https://api.mcport.teamofsilicons.com unless --backend, MCPORT_URL or mcport config set backend says otherwise
Repository: https://github.com/teamofsilicons/silicon-mcport
Documentation: https://github.com/teamofsilicons/silicon-mcport/tree/main/docs
Rust package: https://crates.io/crates/mcport-client
Your MCPort sign-in (login) and a connection's provider account (mcport account) are separate.";

#[derive(Debug, Clone, Parser)]
#[command(
    name = "mcport",
    version,
    about = "Configure MCP connections once and use their tools from any machine, as a Carbon or a Silicon",
    long_about = None,
    after_help = LINKS,
    propagate_version = true
)]
pub struct Cli {
    /// Print compact machine-readable JSON, including structured errors.
    #[arg(long, global = true)]
    pub json: bool,
    /// MCPort backend URL; overrides MCPORT_URL, then saved configuration, then https://api.mcport.teamofsilicons.com.
    #[arg(
        long = "backend",
        env = "MCPORT_URL",
        global = true,
        value_name = "URL"
    )]
    pub url: Option<String>,
    /// Silicon Accounts URL; overrides saved configuration, then https://accounts.teamofsilicons.com.
    #[arg(
        long = "accounts-url",
        env = "ACCOUNTS_URL",
        global = true,
        value_name = "URL"
    )]
    pub accounts_url: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    /// Show how MCPort signs in with Silicon Accounts: app id, URLs and version. Offline; no sign-in needed.
    #[command(
        after_help = "Example: mcport accounts --json\nWorks before sign-in, without a network or a home directory, and always exits 0.\nRelated: mcport login --help, mcport login status --json"
    )]
    Accounts,
    /// Re-key this home's stopped host registries using a coordinated Accounts UUID export. Offline.
    MigrateAccountUuids {
        #[arg(long)]
        file: std::path::PathBuf,
        /// Apply the previewed export and forget affected sign-ins; defaults to dry run.
        #[arg(long)]
        apply: bool,
    },
    /// Hidden alias of `accounts` for runtimes released before Silicon Accounts.
    #[command(hide = true)]
    Iam,
    /// Sign in to MCPort: Carbons approve a code; Silicons hand over a short-lived token.
    #[command(
        args_conflicts_with_subcommands = true,
        after_help = "Carbon (device flow): mcport login\n  Prints a code and a page; approve the code there, on any device. --open also opens the page here.\nSilicon: silicon-accounts login --app mcport -q | mcport login --slt-stdin\n  The token is single use, lasts 2 minutes and works only for MCPort. --slt <token> and mcport login <token> also work, but put it in the process list.\nThe sign-in is kept for this home and backend (one account each); several identities on one machine use separate homes (SILICON_HOME).\nWith --json, a Carbon sign-in prints one JSON line per step, the last one the result.\nRelated: mcport login status --json, mcport logout, mcport accounts --json"
    )]
    Login(LoginArgs),
    /// Sign out: revoke this machine's sign-in at Silicon Accounts and forget it here.
    #[command(
        after_help = "Example: mcport logout\nOther machines and the website stay signed in. Host daemons keep running: they use their own host tokens.\nRelated: mcport login --help, mcport login status"
    )]
    Logout,
    /// Browse MCP references and manage your own directory entries.
    #[command(
        subcommand,
        after_help = "Example: mcport directory ls --search files\nReview an entry with directory show before connection new --from. Entries contain no credentials.\nYour entries are visible to you, your custodian (for a Silicon) and accounts you share them with.\nRelated: mcport directory new --help, mcport connection new --help"
    )]
    Directory(DirectoryCommand),
    /// Configure, inspect and share saved MCP connections.
    #[command(
        subcommand,
        after_help = "Example: mcport connection new docs --transport http --url https://provider.example/mcp\nFor local HTTP or stdio, register a host first and pass --host.\nRelated: mcport connection new --help, mcport host --help, mcport account --help, mcport access --help"
    )]
    Connection(ConnectionCommand),
    /// Discover tool schemas, call tools and switch tools on or off.
    #[command(
        subcommand,
        after_help = "Example: mcport tool ls notes\nInspect a tool with show before passing its arguments to call.\nRelated: mcport tool show --help, mcport tool call --help, mcport tool set --help"
    )]
    Tool(ToolCommand),
    /// Connect, inspect or disconnect the provider account a connection uses (not your MCPort sign-in).
    #[command(
        subcommand,
        after_help = "Example: mcport account connect notes\nShared connections run on their owner's provider account; per-user connections need each caller's own.\nRelated: mcport account connect --help, mcport account show --help, mcport login --help"
    )]
    Account(AccountCommand),
    /// Let specific Carbons and Silicons use a connection, by c:/si: id.
    #[command(
        subcommand,
        after_help = "Example: mcport access new notes --account si:researcher\nThe connection's owner (or the custodian of a Silicon owner) manages access; people who use it sign in themselves.\nRelated: mcport connection set --help, mcport allow --help, mcport tool set --help"
    )]
    Access(AccessCommand),
    /// Choose who outside your custodian's care may share connections and directory entries with a Silicon.
    #[command(
        subcommand,
        after_help = "Example (as a Silicon): mcport allow add c:ada\nExample (as its custodian): mcport allow add c:ada --silicon si:researcher\nSilicons only receive shares from their custodian, the custodian's other Silicons and the accounts allowed here. Carbons can receive shares from anyone signed in.\nRelated: mcport access new --help, mcport directory share --help"
    )]
    Allow(AllowCommand),
    /// Register and manage machines that serve local MCPs.
    #[command(
        subcommand,
        after_help = "Example: mcport host new laptop\nRun on the machine that will execute the local MCP; host new starts its daemon.\nRelated: mcport connection new --help, mcport daemon --help, mcport host migrate --help"
    )]
    Host(HostCommand),
    /// Read the resources and templates an MCP exposes.
    #[command(
        subcommand,
        after_help = "Example: mcport resource ls notes\nRead listed URIs with read; use templates to discover parameterized URIs.\nRelated: mcport resource read --help, mcport resource templates --help, mcport completion get --help"
    )]
    Resource(ResourceCommand),
    /// Discover and expand MCP prompts.
    #[command(
        subcommand,
        after_help = "Example: mcport prompt ls notes\nRead the argument descriptions in the list before expanding a prompt with get.\nRelated: mcport prompt get --help, mcport completion get --help"
    )]
    Prompt(PromptCommand),
    /// Complete MCP prompt or resource-template arguments.
    #[command(
        subcommand,
        after_help = "Example: mcport completion get notes --input @completion.json\nFind prompt names or resource-template URIs first, then build the completion params.\nRelated: mcport completion get --help, mcport prompt ls --help, mcport resource templates --help"
    )]
    Completion(CompletionCommand),
    /// Inspect your calls (and those of Silicons you look after) and cancel one in progress.
    #[command(
        subcommand,
        after_help = "Example: mcport activity ls --connection notes\nUse a returned call ID with show, cancel or asset ls.\nRelated: mcport activity show --help, mcport activity cancel --help, mcport asset --help"
    )]
    Activity(ActivityCommand),
    /// List, save or link files and media a call returned.
    #[command(
        subcommand,
        after_help = "Example: mcport asset ls <call-id>\nPick an index from the list, then save it with asset get or make a one-time link with asset link.\nRelated: mcport asset get --help, mcport asset link --help, mcport activity show --help"
    )]
    Asset(AssetCommand),
    /// Manage the local daemon that serves this machine's registered hosts.
    #[command(
        subcommand,
        after_help = "Example: mcport daemon status\nActs on the host registries of this home and backend.\nRelated: mcport host new --help, mcport daemon start --help, mcport host migrate --help"
    )]
    Daemon(DaemonCommand),
    /// View local settings or change home, backend, Silicon Accounts URL and telemetry.
    #[command(
        subcommand,
        after_help = "Example: mcport config set backend https://your-mcport-backend.example\nSettings choose the backend, Silicon Accounts and telemetry; home chooses an existing storage base.\nRelated: mcport config show, mcport config set --help, mcport config home --help"
    )]
    Config(ConfigCommand),
    /// Read the bundled usage or development guide, offline and without signing in.
    #[command(
        after_help = "Example: mcport docs development\nRelated: mcport --help, mcport config --help"
    )]
    Docs {
        /// Bundled guide to read; development also accepts the alias dev.
        #[arg(value_enum, default_value = "usage")]
        topic: DocsTopic,
    },
    /// Submit a bug report, optionally with a pull request that fixes it.
    #[command(
        after_help = "Example: mcport report 'Tool call fails after refresh' --pr https://github.com/teamofsilicons/silicon-mcport/pull/1\nReports without a PR are welcome. Include reproduction steps, never credentials.\nRelated: mcport activity show --help, mcport docs development"
    )]
    Report {
        /// What failed and the steps to reproduce it; leave out credentials.
        message: String,
        /// URL of an optional pull request with a proposed fix.
        #[arg(long)]
        pr: Option<String>,
    },
}

#[derive(Debug, Clone, Args)]
pub struct LoginArgs {
    /// A Silicon's short-lived token (slt_…), same as --slt. Prefer --slt-stdin.
    #[arg(value_name = "SLT", conflicts_with_all = ["slt", "slt_stdin"])]
    pub token: Option<String>,
    /// Sign a Silicon in with this short-lived token from `silicon-accounts login --app mcport -q`.
    #[arg(long, value_name = "TOKEN", conflicts_with = "slt_stdin")]
    pub slt: Option<String>,
    /// Read the short-lived token from stdin (keeps it out of the process list).
    #[arg(long)]
    pub slt_stdin: bool,
    /// Carbons: also open the approval page in this machine's browser.
    #[arg(long, conflicts_with_all = ["token", "slt", "slt_stdin"])]
    pub open: bool,
    /// Carbons: how this machine is named on the approval page (default: mcport CLI on <host name>).
    #[arg(long, value_name = "TEXT", conflicts_with_all = ["token", "slt", "slt_stdin"])]
    pub label: Option<String>,
    #[command(subcommand)]
    pub action: Option<LoginAction>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum LoginAction {
    /// Show who is signed in here: uuid, id, kind and when the sign-in expires.
    #[command(
        after_help = "Example: mcport login status --json\nSigned out: {\"authenticated\":false}. With --json it always exits 0; without it, it exits 1 when signed out.\nBy default it refreshes if needed and asks the backend to confirm the sign-in (verified); --offline only reads the stored sign-in.\nRelated: mcport login --help, mcport logout"
    )]
    Status {
        /// Read only the stored sign-in: no network, no refresh, no writes.
        #[arg(long)]
        offline: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Transport {
    /// Connect to an HTTP MCP endpoint; add --host for an endpoint on a registered machine.
    Http,
    /// Launch an explicitly registered process on a registered machine.
    Stdio,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AuthMode {
    /// Use the MCP without a provider account.
    None,
    /// Everyone allowed to use the connection runs on its owner's provider account.
    Shared,
    /// Each caller connects their own provider account; never falls back to anyone else's.
    PerUser,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Visibility {
    /// The owner and the accounts it adds with access new (owner-only until it adds any).
    Invited,
    /// Also the owner's own people: a Carbon and the Silicons it looks after, or a Silicon, its custodian and the custodian's other Silicons.
    Circle,
    #[value(hide = true)]
    Private,
    #[value(hide = true)]
    Org,
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
            Self::Invited => "invited",
            Self::Circle => "circle",
            Self::Private => "private",
            Self::Org => "org",
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum DirectoryCommand {
    /// List community references and the personal entries you can see.
    #[command(
        after_help = "Example: mcport directory ls --search database --json\nRelated: mcport directory show --help, mcport directory new --help"
    )]
    Ls {
        /// Search names, descriptions and categories.
        #[arg(long)]
        search: Option<String>,
    },
    /// Review an entry's source and optional connection template.
    #[command(
        after_help = "Example: mcport directory show <entry-id>\nAn entry is a setup suggestion; check its source and executable before use.\nRelated: mcport directory ls, mcport connection new --help"
    )]
    Show {
        /// Exact entry ID from directory ls.
        entry: String,
    },
    /// Add a personal directory entry that you own.
    #[command(
        after_help = "Example: mcport directory new --input @entry.json\nInput: {\"name\":\"Project docs\",\"description\":\"Search our documentation\",\"category\":\"Documentation\",\"source_url\":\"https://provider.example\",\"template\":{\"transport\":\"http\",\"url\":\"https://provider.example/mcp\",\"auth_mode\":\"per-user\"}}\nNever include tokens, headers or environment credentials. Share it with directory share.\nRelated: mcport directory show --help, mcport directory set --help, mcport directory share --help"
    )]
    New {
        /// Complete entry object as inline JSON, @file, or - for stdin.
        #[arg(long)]
        input: String,
    },
    /// Replace an entry you manage, using its current version.
    #[command(
        after_help = "Example: mcport directory set <entry-id> --input @entry.json\nTakes the same complete object as directory new. Community entries are read-only.\nRelated: mcport directory new --help, mcport directory show --help, mcport directory rm --help"
    )]
    Set {
        /// Exact ID of an entry you manage.
        entry: String,
        /// Replacement entry object as inline JSON, @file, or - for stdin.
        #[arg(long)]
        input: String,
    },
    /// Delete an entry you manage; connections made from it stay.
    #[command(
        after_help = "Example: mcport directory rm <entry-id>\nRelated: mcport directory show --help, mcport connection ls"
    )]
    Rm {
        /// Exact ID of an entry you manage.
        entry: String,
    },
    /// Share a personal entry with a Carbon or Silicon.
    #[command(
        after_help = "Example: mcport directory share <entry-id> --account c:ada\nSharing with a Silicon you do not look after needs that Silicon (or its custodian) to allow you first: mcport allow add.\nRelated: mcport directory access --help, mcport directory unshare --help"
    )]
    Share {
        /// Exact ID of an entry you manage.
        entry: String,
        /// The Carbon or Silicon, by id (c:ada, si:researcher) or uuid.
        #[arg(long, value_name = "ID")]
        account: String,
    },
    /// Stop sharing a personal entry with an account.
    #[command(
        after_help = "Example: mcport directory unshare <entry-id> --account c:ada\nRelated: mcport directory access --help, mcport directory share --help"
    )]
    Unshare {
        /// Exact ID of an entry you manage.
        entry: String,
        /// The account to remove, by id or uuid (see directory access).
        #[arg(long, value_name = "ID")]
        account: String,
    },
    /// List the accounts a personal entry is shared with.
    #[command(
        after_help = "Example: mcport directory access <entry-id> --json\nRelated: mcport directory share --help, mcport directory unshare --help"
    )]
    Access {
        /// Exact ID of an entry you manage.
        entry: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ConnectionCommand {
    /// Approve, on its machine, a local connection created elsewhere (for example on the website).
    #[command(
        after_help = "Example: mcport connection register desktop\nRun on the registered host, signed in as the owner of both the host and the connection.\nRelated: mcport connection show --help, mcport host show --help, mcport daemon status"
    )]
    Register {
        /// Local connection name or ID from connection ls.
        connection: String,
        /// Local-only process environment. Repeat KEY=VALUE; values are never uploaded.
        #[arg(long = "env")]
        environment: Vec<String>,
    },
    /// Create a remote or local MCP connection that you own.
    #[command(
        after_help = "Example: mcport connection new docs --transport http --url https://provider.example/mcp --auth none\nFrom the directory: mcport connection new docs --from <entry-id> --dry-run\nLocal stdio: mcport connection new files --host laptop --transport stdio --command /absolute/path/to/server --arg /absolute/path/to/config\nFlags override directory defaults. Local connections need a host you registered and are approved on it as they are created. Directory stdio templates still need an explicit absolute --command.\nNew connections are visible to you only (invited) until you add accounts with access new or choose --visibility circle.\nRelated: mcport directory show --help, mcport host new --help, mcport account connect --help, mcport access new --help"
    )]
    New {
        /// Connection name to use in later commands (unique among your connections).
        name: String,
        /// Optional description shown with the connection.
        #[arg(long)]
        description: Option<String>,
        /// Directory entry ID to use as defaults; inspect it with directory show.
        #[arg(long)]
        from: Option<String>,
        /// HTTP endpoint or local process; required without --from.
        #[arg(long, value_enum, required_unless_present = "from")]
        transport: Option<Transport>,
        /// MCP endpoint URL (not the MCPort backend).
        #[arg(long = "url")]
        endpoint: Option<String>,
        /// Registered host name or ID. Required for stdio or local HTTP.
        #[arg(long)]
        host: Option<String>,
        /// Executable to launch on the registered host; never run through a shell.
        #[arg(long)]
        command: Option<String>,
        /// One process argument; repeat. Any --arg replaces all template arguments.
        #[arg(long = "arg", allow_hyphen_values = true)]
        arguments: Vec<String>,
        /// Drop every suggested process argument from the directory template.
        #[arg(long, conflicts_with = "arguments")]
        clear_args: bool,
        /// Process environment entry KEY=VALUE (local stdio only). Keep secrets out of shell history.
        #[arg(long = "env")]
        environment: Vec<String>,
        /// Provider account mode; inherits the template's, otherwise none.
        #[arg(long, value_enum)]
        auth: Option<AuthMode>,
        /// Who may use it besides the accounts you invite (default invited).
        #[arg(long, value_enum)]
        visibility: Option<Visibility>,
        /// Print the resolved configuration without creating or registering anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// List the connections you can use: yours, those of Silicons you look after, your people's and those shared with you.
    #[command(
        after_help = "Example: mcport connection ls --json\nEach connection's access field says why you see it: owner, custodian, circle or invited.\nRelated: mcport connection show --help, mcport connection new --help, mcport tool ls --help"
    )]
    Ls,
    /// Show a connection's configuration, status and provider account, never credentials.
    #[command(
        after_help = "Example: mcport connection show notes\nRelated: mcport connection ls, mcport connection set --help, mcport account show --help"
    )]
    Show {
        /// Connection name or ID from connection ls.
        connection: String,
    },
    /// Rename, describe or change the visibility of a connection you manage.
    #[command(
        after_help = "Example: mcport connection set notes --name team-notes --visibility circle\nGive at least one of --name, --description or --visibility. Use access new to invite specific accounts.\nRelated: mcport connection show --help, mcport access new --help, mcport connection rm --help"
    )]
    Set {
        /// Connection name or ID you manage.
        connection: String,
        /// New connection name.
        #[arg(long)]
        name: Option<String>,
        /// New description; an empty string clears it.
        #[arg(long)]
        description: Option<String>,
        /// New visibility.
        #[arg(long, value_enum)]
        visibility: Option<Visibility>,
    },
    /// Delete a connection you manage, with its access, policies and saved provider accounts.
    #[command(
        after_help = "Example: mcport connection rm notes\nQueued calls on it are cancelled; on its host it is also removed from the local allowlist.\nRelated: mcport connection show --help, mcport access rm --help, mcport account disconnect --help"
    )]
    Rm {
        /// Connection name or ID you manage.
        connection: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ToolCommand {
    /// Discover the tools a connection offers you, with their schemas.
    #[command(
        after_help = "Example: mcport tool ls notes\nIf the result has nextCursor, pass it unchanged with --cursor for the next page.\nRelated: mcport tool show --help, mcport tool call --help, mcport tool set --help"
    )]
    Ls {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous tool ls result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Show one tool's input and output schema before calling it.
    #[command(
        after_help = "Example: mcport tool show notes search\nBuild the JSON arguments for tool call from the input schema.\nRelated: mcport tool ls --help, mcport tool call --help"
    )]
    Show {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact tool name from tool ls.
        tool: String,
    },
    /// Call a tool once. Input: JSON, @file.json or - for stdin. Changes are never retried automatically.
    #[command(
        after_help = "Example: mcport tool call notes search --input '{\"query\":\"release\"}'\nFile: mcport tool call notes search --input @request.json\nStdin: printf '%s\\n' '{\"query\":\"release\"}' | mcport tool call notes search --input -\nCheck the tool's schema first. If waiting stops or the outcome is unknown, inspect activity before trying again.\nRelated: mcport tool show --help, mcport activity ls --help, mcport activity cancel --help, mcport asset ls --help"
    )]
    Call {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact tool name from tool ls.
        tool: String,
        /// Tool arguments as a JSON object, @path to a JSON file, or - for stdin.
        #[arg(long, default_value = "{}", allow_hyphen_values = true)]
        input: String,
        /// Stable key for one logical call; reusing it with different input is refused.
        #[arg(long)]
        idempotency_key: Option<String>,
        /// Maximum execution time in milliseconds (100 to 600000).
        #[arg(long)]
        timeout_ms: Option<u64>,
    },
    /// Switch a tool on or off for everyone, or for one account. Connection-wide offs always win.
    #[command(
        after_help = "Example: mcport tool set notes delete_note --enabled false --account si:researcher\nFor connections you manage. Leave out --account to apply to everyone. An account's --enabled true never overrides a connection-wide false, and a policy never grants access.\nRelated: mcport tool ls --help, mcport access new --help, mcport connection show --help"
    )]
    Set {
        /// Connection name or ID you manage.
        connection: String,
        /// Exact MCP tool name.
        tool: String,
        /// true to allow, false to deny, within the chosen scope.
        #[arg(long, action = clap::ArgAction::Set)]
        enabled: bool,
        /// Limit the policy to one Carbon or Silicon (c:/si: id or uuid); leave out for everyone.
        #[arg(long, alias = "principal", value_name = "ID")]
        account: Option<String>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AccountCommand {
    /// Connect your provider account (or, as its manager, a connection's shared one), or start provider consent.
    #[command(
        after_help = "Example: mcport account connect notes\nRemote OAuth returns a consent URL to open in a browser. A pre-registered public client ID is needed only when the provider supports neither client metadata documents nor dynamic registration.\nManual HTTP credential: mcport account connect notes --input @credentials.json\nThe file holds {\"kind\":\"bearer\",\"secret\":\"...\"} or {\"kind\":\"header\",\"header_name\":\"X-API-Key\",\"secret\":\"...\"}.\nLocal connections are set up on their host: HTTP takes bearer/header; stdio takes {\"kind\":\"env\",\"env\":{\"API_TOKEN\":\"...\"}}. A shared local connection with no input uses the host's existing application account.\nSigned in with another home on the host's machine? Point --host-home at the home that registered the host.\nRelated: mcport account show --help, mcport account disconnect --help, mcport connection new --help"
    )]
    Connect {
        /// Connection name or ID with shared or per-user authentication.
        connection: String,
        /// Credential JSON object, @protected-file, or - for stdin; not with --token or --client-id.
        #[arg(long, allow_hyphen_values = true)]
        input: Option<String>,
        /// Prompt for a bearer token without echo; not with --input or --client-id.
        #[arg(long)]
        token: bool,
        /// Pre-registered public OAuth client ID for remote OAuth only.
        #[arg(long)]
        client_id: Option<String>,
        /// Local connections: the home on this machine that registered the host (default: this home).
        #[arg(long, value_name = "DIR")]
        host_home: Option<std::path::PathBuf>,
    },
    /// Disconnect your provider account, a shared one you manage, or that of a Silicon you look after.
    #[command(
        after_help = "Example: mcport account disconnect notes\nCustodian: mcport account disconnect notes --account si:researcher (remote connections).\nShared mode disconnects the shared account; per-user mode only the chosen account's.\nRelated: mcport account show --help, mcport account connect --help"
    )]
    Disconnect {
        /// Connection name or ID from connection ls.
        connection: String,
        /// A Silicon you look after, by si: id or uuid (default: yourself).
        #[arg(long, value_name = "ID")]
        account: Option<String>,
        /// Local connections: the home on this machine that registered the host.
        #[arg(long, value_name = "DIR")]
        host_home: Option<std::path::PathBuf>,
    },
    /// Show whose provider account a connection uses for you, without its credentials.
    #[command(
        after_help = "Example: mcport account show notes\nCustodian: mcport account show notes --account si:researcher\nA saved credential alone does not prove the provider still accepts it.\nRelated: mcport account connect --help, mcport account disconnect --help, mcport tool ls --help"
    )]
    Show {
        /// Connection name or ID from connection ls.
        connection: String,
        /// A Silicon you look after, by si: id or uuid (default: yourself).
        #[arg(long, value_name = "ID")]
        account: Option<String>,
        /// Local connections: the home on this machine that registered the host.
        #[arg(long, value_name = "DIR")]
        host_home: Option<std::path::PathBuf>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AccessCommand {
    /// Let a Carbon or Silicon use a connection you manage.
    #[command(
        after_help = "Example: mcport access new notes --account si:researcher\nUse never includes editing, sharing or deleting. Tool restrictions and provider account rules still apply.\nA Silicon you do not look after must allow you first (mcport allow add, run by it or its custodian); Carbons can always be added.\nRelated: mcport access ls --help, mcport connection set --help, mcport tool set --help"
    )]
    New {
        /// Connection name or ID you manage.
        connection: String,
        /// The Carbon or Silicon, by id (c:ada, si:researcher) or uuid.
        #[arg(long, alias = "principal", value_name = "ID")]
        account: String,
    },
    /// List the accounts invited to a connection you manage.
    #[command(
        after_help = "Example: mcport access ls notes\nRelated: mcport access new --help, mcport access rm --help, mcport connection show --help"
    )]
    Ls {
        /// Connection name or ID you manage.
        connection: String,
    },
    /// Remove an account's access to a connection.
    #[command(
        after_help = "Example: mcport access rm notes --account si:researcher\nA connection with visibility circle stays usable by the owner's own people; check connection show.\nRelated: mcport access ls --help, mcport connection set --help, mcport tool set --help"
    )]
    Rm {
        /// Connection name or ID you manage.
        connection: String,
        /// The account to remove, by id or uuid (see access ls).
        #[arg(long, alias = "principal", value_name = "ID")]
        account: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AllowCommand {
    /// Allow an account to share connections and directory entries with a Silicon.
    #[command(
        after_help = "Example (as a Silicon): mcport allow add c:ada\nExample (as its custodian): mcport allow add c:ada --silicon si:researcher\nRelated: mcport allow ls --help, mcport allow rm --help, mcport access new --help"
    )]
    Add {
        /// The account to allow, by id (c:ada, si:helper) or uuid.
        account: String,
        /// A Silicon you look after (default: yourself, when you are a Silicon).
        #[arg(long, value_name = "ID")]
        silicon: Option<String>,
    },
    /// List the accounts a Silicon accepts shares from.
    #[command(
        after_help = "Example: mcport allow ls --silicon si:researcher --json\nRelated: mcport allow add --help, mcport allow rm --help"
    )]
    Ls {
        /// A Silicon you look after (default: yourself, when you are a Silicon).
        #[arg(long, value_name = "ID")]
        silicon: Option<String>,
    },
    /// Stop accepting new shares from an account; what it already shared stays until its owner removes it.
    #[command(
        after_help = "Example: mcport allow rm c:ada --silicon si:researcher\nRelated: mcport allow ls --help, mcport access rm --help"
    )]
    Rm {
        /// The account, by id or uuid (see allow ls).
        account: String,
        /// A Silicon you look after (default: yourself, when you are a Silicon).
        #[arg(long, value_name = "ID")]
        silicon: Option<String>,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum HostCommand {
    /// Register this machine as a host and start its background daemon.
    #[command(
        after_help = "Example: mcport host new laptop\nThen create a local connection with --host laptop. The MCP application must be available on this machine.\nRelated: mcport connection new --help, mcport host show --help, mcport daemon status"
    )]
    New {
        /// Name for this host (letters, digits, - and _).
        name: String,
    },
    /// List the hosts you registered and those of Silicons you look after.
    #[command(
        after_help = "Example: mcport host ls --json\nRelated: mcport host show --help, mcport host new --help, mcport daemon status"
    )]
    Ls,
    /// Show a host's connectivity and owner.
    #[command(
        after_help = "Example: mcport host show laptop\nRelated: mcport host ls, mcport daemon status, mcport daemon start"
    )]
    Show {
        /// Host name or ID from host ls.
        host: String,
    },
    /// Revoke a host and disconnect its daemon (its owner, or the custodian of a Silicon owner).
    #[command(
        after_help = "Example: mcport host rm laptop\nUse daemon stop to pause local execution without revoking the host.\nRelated: mcport host show --help, mcport daemon stop, mcport connection rm --help"
    )]
    Rm {
        /// Host name or ID from host ls.
        host: String,
    },
    /// Re-key a host registered before Silicon Accounts so its local accounts follow uuids. Run once, on the host.
    #[command(
        after_help = "Example: mcport host migrate laptop --dry-run, then mcport host migrate laptop\nRun on the host's machine, in the home that registered it, signed in as its owner. It keeps a copy of the old registry (registry.v1.json), stops the daemon if it runs, rewrites the registry and starts the daemon again with this mcport.\nLocal provider accounts of old ids MCPort cannot link are refused unless --drop-unmapped; those accounts then connect again with account connect.\nRelated: mcport daemon status, mcport account show --help"
    )]
    Migrate {
        /// Host name or ID from host ls.
        host: String,
        /// Show what would change without changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Remove local provider accounts whose old ids cannot be linked to an account.
        #[arg(long)]
        drop_unmapped: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ResourceCommand {
    /// List the resources an MCP makes available.
    #[command(
        after_help = "Example: mcport resource ls notes\nPass nextCursor unchanged with --cursor for the next page. Read a listed URI with resource read.\nRelated: mcport resource read --help, mcport resource templates --help"
    )]
    Ls {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous resource ls result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// List URI templates for parameterized resources.
    #[command(
        after_help = "Example: mcport resource templates notes\nFill in a template, then pass the resulting URI to resource read.\nRelated: mcport resource read --help, mcport completion get --help, mcport resource ls --help"
    )]
    Templates {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous resource templates result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read a resource URI, keeping text, media and metadata.
    #[command(
        after_help = "Example: mcport resource read notes 'notes://recent'\nUse a URI the MCP listed or one built from its resource templates.\nRelated: mcport resource ls --help, mcport resource templates --help, mcport asset ls --help"
    )]
    Read {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact MCP resource URI, not a local file name.
        uri: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum PromptCommand {
    /// List available prompts and their argument descriptions.
    #[command(
        after_help = "Example: mcport prompt ls notes\nPass nextCursor unchanged with --cursor for the next page.\nRelated: mcport prompt get --help, mcport completion get --help"
    )]
    Ls {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Opaque nextCursor from a previous prompt ls result.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Expand a prompt with its arguments (JSON, @file or -).
    #[command(
        after_help = "Example: mcport prompt get notes summarize --input '{\"text\":\"Release notes\"}'\nUse a prompt and arguments from prompt ls; the result is the MCP's expanded prompt.\nRelated: mcport prompt ls --help, mcport completion get --help"
    )]
    Get {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Exact prompt name from prompt ls.
        prompt: String,
        /// Prompt arguments as a JSON object of strings, @path to a JSON file, or - for stdin.
        #[arg(long, default_value = "{}", allow_hyphen_values = true)]
        input: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum ActivityCommand {
    /// List recent calls: yours and those of Silicons you look after.
    #[command(
        after_help = "Example: mcport activity ls --connection notes\nUse a returned call ID with activity show, activity cancel or asset ls. Connection owners do not see other accounts' calls.\nRelated: mcport activity show --help, mcport activity cancel --help, mcport asset ls --help"
    )]
    Ls {
        /// Only calls on this connection (name or ID).
        #[arg(long)]
        connection: Option<String>,
    },
    /// Show one call's status and result.
    #[command(
        after_help = "Example: mcport activity show <call-id> --json\nRelated: mcport activity ls --help, mcport activity cancel --help, mcport asset ls --help"
    )]
    Show {
        /// Call ID from a call or activity ls.
        id: String,
    },
    /// Ask to cancel a call; completed effects at the provider cannot be undone.
    #[command(
        after_help = "Example: mcport activity cancel <call-id>\nCheck the resulting status; cancellation is best effort and does not roll back provider effects.\nRelated: mcport activity ls --help, mcport activity show --help"
    )]
    Cancel {
        /// Call ID from activity ls or the call's own output.
        id: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum DaemonCommand {
    /// Start the daemon for the hosts registered in this home and backend.
    #[command(
        after_help = "Example: mcport daemon start\nUses existing host registries; register a new host with host new.\nRelated: mcport daemon status, mcport host new --help, mcport connection register --help"
    )]
    Start,
    /// Check whether each local host's daemon runs and reaches the backend.
    #[command(
        after_help = "Example: mcport daemon status\nregistry_version 1 means the host was registered before Silicon Accounts: run mcport host migrate <host>.\nRelated: mcport daemon start, mcport daemon stop, mcport host show --help"
    )]
    Status,
    /// Stop this home's daemons without revoking the hosts.
    #[command(
        after_help = "Example: mcport daemon stop\nLocal connections cannot run while their daemon is stopped; daemon start resumes.\nRelated: mcport daemon status, mcport daemon start, mcport host rm --help"
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

#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Use an existing directory as the base home for this CLI context.
    #[command(
        after_help = "Example: mcport config home /existing/work-directory\nThe directory must exist. Sign-ins and host registries are not copied to the new home.\nRelated: mcport config show, mcport config set --help, mcport login --help"
    )]
    Home {
        /// Existing storage base; MCPort uses its .mcport/dir subdirectory.
        location: std::path::PathBuf,
    },
    /// Print the non-secret local settings and where state is stored.
    #[command(
        after_help = "Example: mcport config show --json\nRelated: mcport config set --help, mcport config home --help, mcport accounts --json"
    )]
    Show,
    /// Save the backend URL, the Silicon Accounts URL or the telemetry setting for this home.
    #[command(
        after_help = "Example: mcport config set backend https://your-mcport-backend.example\nSilicon Accounts: mcport config set accounts https://accounts.example (sign in again afterwards)\nTelemetry: mcport config set telemetry false\nKeys: backend (also url, backend_url), accounts (also accounts_url), telemetry. --backend/MCPORT_URL and --accounts-url/ACCOUNTS_URL override the saved values.\nRelated: mcport config show, mcport config home --help, mcport accounts"
    )]
    Set {
        /// Setting name: backend, accounts or telemetry.
        key: String,
        /// URL (https, or http on this machine), or true/false for telemetry.
        value: String,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AssetCommand {
    /// List the downloadable files and media embedded in one call's result.
    #[command(
        after_help = "Example: mcport asset ls <call-id>\nNote the zero-based index and media type before saving.\nRelated: mcport activity ls --help, mcport activity show --help, mcport asset get --help"
    )]
    Ls {
        /// A completed call ID (yours, or a Silicon's you look after).
        call: String,
    },
    /// Save one asset to a new private file; existing files are never replaced.
    #[command(
        after_help = "Example: mcport asset get <call-id> 0 --output ./result.bin\nChoose the index and extension from asset ls. The parent directory must exist; --json changes the status output, not the saved bytes.\nRelated: mcport asset ls --help, mcport asset link --help"
    )]
    Get {
        /// A completed call ID, not a connection name.
        call: String,
        /// Zero-based asset index from asset ls.
        index: u32,
        /// New output file; its parent must exist and existing files are never overwritten.
        #[arg(long)]
        output: std::path::PathBuf,
    },
    /// Make a one-time link to one asset that works without signing in, for 60 seconds.
    #[command(
        after_help = "Example: mcport asset link <call-id> 0\nOpen it in a browser or hand it to whoever needs the file. It works once and re-checks your access when used.\nRelated: mcport asset ls --help, mcport asset get --help"
    )]
    Link {
        /// A completed call ID.
        call: String,
        /// Zero-based asset index from asset ls.
        index: u32,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum CompletionCommand {
    /// Ask the MCP for suggestions for a prompt or resource-template argument.
    #[command(
        after_help = "Example: mcport completion get notes --input '{\"ref\":{\"type\":\"ref/prompt\",\"name\":\"summarize\"},\"argument\":{\"name\":\"text\",\"value\":\"hel\"}}'\nUse a prompt and argument from prompt ls. For a resource template use ref {\"type\":\"ref/resource\",\"uri\":\"<advertised-uri-template>\"} with its variable name. The MCP must support completions.\nRelated: mcport prompt ls --help, mcport prompt get --help, mcport resource templates --help"
    )]
    Get {
        /// Connection name or ID from connection ls.
        connection: String,
        /// Full MCP completion params object with ref and argument (JSON, @file or -), without a JSON-RPC envelope.
        #[arg(long, allow_hyphen_values = true)]
        input: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DocsTopic {
    /// Sign-in, connections, provider accounts, sharing and everyday CLI workflows.
    Usage,
    /// Architecture, local development, configuration, testing and release notes.
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
