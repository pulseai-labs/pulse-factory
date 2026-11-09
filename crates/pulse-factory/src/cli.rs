//! The command line (ADR-0004): `arm`, `status`, `halt`, `requests`, `show`, `claim`, `answer`,
//! `clear`, `ack`, `project`, `session`, `chat-runtime`, `device`, `api`, `hook` and `controller`.
//!
//! No verb waits or watches. Exit codes: 0 ok, 1 a refusal or an error, 2 `arm` with neither
//! `--dry-run` nor `--yes` (or a command line that clap refuses).

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "pulse-factory",
    version,
    about = "Arm a run of seats and watch item PRs merge, with no operator present."
)]
pub struct Cli {
    #[command(subcommand)]
    pub verb: Verb,
}

#[derive(Subcommand, Debug)]
pub enum Verb {
    /// Check a plan, show what it will spend, and start a run.
    Arm(ArmArgs),
    /// Show a run: its units, sessions, cost and notes.
    Status(StatusArgs),
    /// Stop new launches and merges. Running seats finish their stage.
    Halt(HaltArgs),
    /// List the open requests: what waits for an answer.
    Requests(RequestsArgs),
    /// Show one request: every field, its body, its answer and the verbs that answer it.
    Show(ShowArgs),
    /// Take the claim: this session becomes the one that may answer.
    Claim(ClaimArgs),
    /// Answer a request with a ruling. A parked unit resumes at the stage it parked in.
    Answer(AnswerArgs),
    /// Resume a parked unit, at the stage it parked in or at `--to`.
    Clear(ClearArgs),
    /// Ack a merge request: the spine PR merges with the head SHA the request names.
    Ack(AckArgs),
    /// Add, list and remove the projects on this host. A project is an ID, a name and one root.
    Project(ProjectArgs),
    /// Chat with a project's Orchestrator: start, send, answer, set, interrupt, end, list, read.
    Session(SessionArgs),
    /// Install the managed Claude Code copy that the chat runs.
    ChatRuntime(ChatRuntimeArgs),
    /// Pair, list and revoke the devices that call the network API. A device acts as you.
    Device(DeviceArgs),
    /// Show whether the network API is off, waiting for its address or listening.
    Api(ApiArgs),
    /// Judge one tool call of a seat (the extension runs this; stdin holds the call).
    Hook,
    /// Run the controller in the foreground.
    Controller(ControllerArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ArmArgs {
    /// The spine ID. It fills `param.spine`.
    pub spine: String,
    /// A workflow name under the share directory, or a path (a value with a `/`).
    #[arg(long)]
    pub workflow: String,
    /// The ledger file of the run.
    #[arg(long)]
    pub ledger: PathBuf,
    /// A workflow parameter, `key=value`. Repeat for each.
    #[arg(long = "param", value_name = "K=V")]
    pub params: Vec<String>,
    /// A directory that holds plugins. Repeat for each.
    #[arg(long = "plugin-root", value_name = "DIR")]
    pub plugin_roots: Vec<PathBuf>,
    /// Run the seats as paper scripts against a bare repo in this directory. No model, no spend.
    #[arg(long, value_name = "DIR")]
    pub paper: Option<PathBuf>,
    /// Replace this earlier run. It must have no running session.
    #[arg(long, value_name = "RUN")]
    pub replace: Option<String>,
    /// Show the summary and check the plan. Start nothing, record nothing.
    #[arg(long)]
    pub dry_run: bool,
    /// Start the run.
    #[arg(long)]
    pub yes: bool,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct StatusArgs {
    /// The run. The default is the latest.
    pub run: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct HaltArgs {
    /// The run. The default is the latest.
    pub run: Option<String>,
    /// Why the run halts.
    #[arg(long)]
    pub reason: String,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct RequestsArgs {
    /// Only this run's requests. The default is every run.
    #[arg(long, value_name = "RUN")]
    pub run: Option<String>,
    /// Also the answered and withdrawn requests.
    #[arg(long)]
    pub all: bool,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ShowArgs {
    /// The request, such as `R-3`.
    pub id: String,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ClaimArgs {
    /// A label for this session. The default is `<user>@<host>:<process>`.
    #[arg(long)]
    pub holder: Option<String>,
    /// The run to record the claim in. The default is the latest run that has not ended.
    #[arg(long, value_name = "RUN")]
    pub run: Option<String>,
    /// Print JSON. The token is never printed: it goes to the claim token file.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AnswerArgs {
    /// The request, such as `R-3`.
    pub id: String,
    /// The revision you read with `show`. Another revision is refused.
    #[arg(long, value_name = "N")]
    pub rev: u32,
    /// Your ruling. It is stored and shown as data and never run.
    #[arg(long)]
    pub text: String,
    /// The file that holds the claim token. The default is `<state dir>/claim-token`.
    #[arg(long, value_name = "FILE")]
    pub claim_token_file: Option<PathBuf>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ClearArgs {
    /// The run.
    pub run: String,
    /// The item (the parked unit).
    pub item: String,
    /// The stage to resume at. The default is the stage the unit parked in.
    #[arg(long, value_name = "STAGE")]
    pub to: Option<String>,
    /// The file that holds the claim token. The default is `<state dir>/claim-token`.
    #[arg(long, value_name = "FILE")]
    pub claim_token_file: Option<PathBuf>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct AckArgs {
    /// The `merge_ack` request, such as `R-3`.
    pub id: String,
    /// The revision you read with `show`. Another revision is refused.
    #[arg(long, value_name = "N")]
    pub rev: u32,
    /// The file that holds the claim token. The default is `<state dir>/claim-token`.
    #[arg(long, value_name = "FILE")]
    pub claim_token_file: Option<PathBuf>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ProjectArgs {
    #[command(subcommand)]
    pub verb: ProjectVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ProjectVerb {
    /// Register a directory as a project.
    Add(ProjectAddArgs),
    /// List the projects.
    List(ProjectListArgs),
    /// Take a project off the list.
    Remove(ProjectRemoveArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ProjectAddArgs {
    /// The root: an existing directory, a git repo or a plain folder. A relative path is taken
    /// from the current directory.
    pub root: PathBuf,
    /// The name: 1 to 64 characters. The default is the last component of the root.
    #[arg(long)]
    pub name: Option<String>,
    /// The command ID. A retry sends the same one, and the command is admitted once. The default
    /// is a random one for each call.
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ProjectListArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ProjectRemoveArgs {
    /// The project, such as `P-1`.
    pub id: String,
    /// The command ID. A retry sends the same one, and the command is admitted once. The default
    /// is a random one for each call.
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionArgs {
    #[command(subcommand)]
    pub verb: SessionVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum SessionVerb {
    /// Start the Orchestrator of a project in its main worktree.
    Start(SessionStartArgs),
    /// Send a message to a chat. It does not wait for the reply: read it with `session items`.
    Send(SessionSendArgs),
    /// Answer an approval or a question of a chat.
    Answer(SessionAnswerArgs),
    /// End the running turn of a chat. The process stays.
    Interrupt(SessionInterruptArgs),
    /// Change the model, effort, permission mode or a runtime option of this chat only.
    Set(SessionSetArgs),
    /// End a chat and stop its process.
    End(SessionEndArgs),
    /// Redact one item of a chat's display log (CLI only).
    Redact(SessionRedactArgs),
    /// List the chats, with their state, cost and process.
    List(SessionListArgs),
    /// Read the stored items of a chat.
    Items(SessionItemsArgs),
}

#[derive(Args, Debug, Clone)]
pub struct SessionStartArgs {
    /// The project, such as `P-1`.
    #[arg(long, value_name = "P-n")]
    pub project: String,
    /// The agent. The default is the built-in Orchestrator.
    #[arg(long, default_value = "orchestrator")]
    pub agent: String,
    /// The command ID. A retry sends the same one, and the command is admitted once.
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionSendArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    /// The message: 1 byte to 256 KiB.
    pub text: String,
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionAnswerArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    /// The `seq` of the request item (read it with `session items`).
    pub seq: u64,
    /// Allow the tool call.
    #[arg(long, conflicts_with_all = ["deny", "answer"])]
    pub allow: bool,
    /// Deny the tool call, with a message of at most 1000 characters and no line break.
    #[arg(long, value_name = "MESSAGE", conflicts_with = "answer")]
    pub deny: Option<String>,
    /// Answer a question: `<question>=<label>`. Repeat for each question.
    #[arg(long, value_name = "QUESTION=LABEL")]
    pub answer: Vec<String>,
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionInterruptArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionSetArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    /// The permission mode.
    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,
    /// A runtime option: `<id>=<value>`. Repeat for each.
    #[arg(long, value_name = "ID=VALUE")]
    pub option: Vec<String>,
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionEndArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionRedactArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    /// The `seq` of the item.
    pub seq: u64,
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionListArgs {
    /// Only this project's chats.
    #[arg(long, value_name = "P-n")]
    pub project: Option<String>,
    /// Also the ended chats.
    #[arg(long)]
    pub all: bool,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SessionItemsArgs {
    /// The chat, such as `S-1`.
    pub id: String,
    /// Only the items after this `seq`.
    #[arg(long, default_value_t = 0)]
    pub after: u64,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ChatRuntimeArgs {
    #[command(subcommand)]
    pub verb: ChatRuntimeVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ChatRuntimeVerb {
    /// Fetch the managed Claude Code copy, check it against the pin, and place it.
    Install(ChatRuntimeInstallArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ChatRuntimeInstallArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct DeviceArgs {
    #[command(subcommand)]
    pub verb: DeviceVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum DeviceVerb {
    /// Print one pairing code. It works once, for 10 minutes, and a new code replaces it.
    Pair(DevicePairArgs),
    /// List the devices: live ones first, then the revoked ones.
    List(DeviceListArgs),
    /// Revoke a device. Its next call is refused. The device pairs again with a new code.
    Revoke(DeviceRevokeArgs),
}

#[derive(Args, Debug, Clone)]
pub struct DevicePairArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct DeviceListArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct DeviceRevokeArgs {
    /// The device, such as `D-1`.
    pub id: String,
    /// The command ID. A retry sends the same one, and the command is admitted once. The default
    /// is a random one for each call.
    #[arg(long, value_name = "ID")]
    pub command_id: Option<String>,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ApiArgs {
    #[command(subcommand)]
    pub verb: ApiVerb,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ApiVerb {
    /// Show `off`, `waiting for <addr>`, `listening on <addr>` or `failed`, and the key's hash.
    Status(ApiStatusArgs),
}

#[derive(Args, Debug, Clone)]
pub struct ApiStatusArgs {
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Clone)]
pub struct ControllerArgs {
    /// Milliseconds between two ticks.
    #[arg(long, default_value_t = 2000)]
    pub tick_ms: u64,
    /// Serve the network API on this address, such as `100.90.203.21:7443`. The default is the
    /// environment variable `PF_API_LISTEN`. With neither, the API is off and no key is made.
    #[arg(long, value_name = "ADDR")]
    pub api_listen: Option<String>,
}

/// Run the command line and return the exit code.
pub fn run(args: Vec<OsString>) -> i32 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    match cli.verb {
        Verb::Arm(a) => crate::arm::run(&a),
        Verb::Status(a) => crate::controller::client::status(&a),
        Verb::Halt(a) => crate::controller::client::halt(&a),
        Verb::Requests(a) => crate::controller::client::requests(&a),
        Verb::Show(a) => crate::controller::client::show(&a),
        Verb::Claim(a) => crate::controller::client::claim(&a),
        Verb::Answer(a) => crate::controller::client::answer(&a),
        Verb::Clear(a) => crate::controller::client::clear(&a),
        Verb::Ack(a) => crate::controller::client::ack(&a),
        Verb::Project(a) => match a.verb {
            ProjectVerb::Add(a) => crate::controller::client::project_add(&a),
            ProjectVerb::List(a) => crate::controller::client::project_list(&a),
            ProjectVerb::Remove(a) => crate::controller::client::project_remove(&a),
        },
        Verb::Session(a) => match a.verb {
            SessionVerb::Start(a) => crate::controller::client::session_start(&a),
            SessionVerb::Send(a) => crate::controller::client::session_send(&a),
            SessionVerb::Answer(a) => crate::controller::client::session_answer(&a),
            SessionVerb::Interrupt(a) => crate::controller::client::session_interrupt(&a),
            SessionVerb::Set(a) => crate::controller::client::session_set(&a),
            SessionVerb::End(a) => crate::controller::client::session_end(&a),
            SessionVerb::Redact(a) => crate::controller::client::session_redact(&a),
            SessionVerb::List(a) => crate::controller::client::session_list(&a),
            SessionVerb::Items(a) => crate::controller::client::session_items(&a),
        },
        Verb::ChatRuntime(a) => match a.verb {
            ChatRuntimeVerb::Install(a) => crate::controller::client::chat_runtime_install(&a),
        },
        Verb::Device(a) => match a.verb {
            DeviceVerb::Pair(a) => crate::controller::client::device_pair(&a),
            DeviceVerb::List(a) => crate::controller::client::device_list(&a),
            DeviceVerb::Revoke(a) => crate::controller::client::device_revoke(&a),
        },
        Verb::Api(a) => match a.verb {
            ApiVerb::Status(a) => crate::controller::client::api_status(&a),
        },
        Verb::Hook => crate::hook::run(),
        Verb::Controller(a) => crate::controller::run(&a),
    }
}
