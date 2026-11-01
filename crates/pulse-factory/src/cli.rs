//! The command line (ADR-0004): `arm`, `status`, `halt`, `hook` and `controller`.
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
pub struct ControllerArgs {
    /// Milliseconds between two ticks.
    #[arg(long, default_value_t = 2000)]
    pub tick_ms: u64,
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
        Verb::Hook => crate::hook::run(),
        Verb::Controller(a) => crate::controller::run(&a),
    }
}
