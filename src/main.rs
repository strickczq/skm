//! skm — declaratively manage agent skills. Entry point and CLI dispatch.

#[cfg(windows)]
compile_error!(
    "skm supports only macOS and Linux (including WSL). \
     Native Windows is not supported; please run skm inside WSL."
);

mod cmd;
mod error;
mod model;
mod pipeline;
mod source;
mod sys;
mod ui;

use clap::{Parser, Subcommand, ValueEnum};

use error::ExitCode;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ColorWhen {
    Auto,
    Always,
    Never,
}

impl From<ColorWhen> for anstream::ColorChoice {
    fn from(c: ColorWhen) -> Self {
        match c {
            ColorWhen::Auto => Self::Auto,
            ColorWhen::Always => Self::Always,
            ColorWhen::Never => Self::Never,
        }
    }
}

#[derive(Parser)]
#[command(name = "skm", version, about = "Declaratively manage agent skills")]
struct Cli {
    /// Operate on the global manifest (~/.config/skm/skm.toml).
    #[arg(short = 'g', long, global = true)]
    global: bool,

    /// Colorize output: auto (default, detects TTY + honors NO_COLOR), always, never.
    #[arg(
        long = "color",
        value_enum,
        default_value_t = ColorWhen::Auto,
        global = true,
        value_name = "WHEN"
    )]
    color: ColorWhen,

    #[command(subcommand)]
    command: Commands,
}

/// Shown under `skm add --help`. The spec is auto-detected; these cover the
/// common shapes plus the explicit `git|tar|zip|local` escape hatch.
const ADD_EXAMPLES: &str = "\
Examples:
  skm add anthropics/skills --subdir docx GitHub owner/repo + subdirectory
  skm add anthropics/skills@v1.2          pin owner/repo with an inline @ref
  skm add https://github.com/foo/bar      full git URL (scp-style git@… too)
  skm add ./vendor/reviewer               local directory, relative to skm.toml
  skm add foo.tar.gz --sha256 <hex>       archive, byte-verified
  skm add git <url>                       force a kind when detection is ambiguous";

#[derive(Subcommand)]
enum Commands {
    /// Create a commented skm.toml template.
    Init {
        /// Overwrite an existing manifest.
        #[arg(long)]
        force: bool,
    },
    /// Resolve the manifest into skm.lock (does not deploy).
    Lock {
        /// Re-resolve mutable refs. Bare flag upgrades all; pass names to limit.
        #[arg(long, num_args = 0.., value_name = "NAME")]
        upgrade: Option<Vec<String>>,
    },
    /// Converge installed skills to the lock (the core command).
    Sync {
        /// Content fully determined by the lock; do not re-resolve.
        #[arg(long)]
        frozen: bool,
        /// Assert lock ≡ manifest, then deploy (CI gate). Does not write lock.
        #[arg(long)]
        locked: bool,
        /// Compute and print the change plan without modifying disk.
        #[arg(long)]
        dry_run: bool,
        /// Use only local cache; never touch the network.
        #[arg(long)]
        offline: bool,
        /// Force prune of managed extras (legal with --frozen).
        #[arg(long)]
        prune: bool,
        /// Disable prune.
        #[arg(long)]
        no_prune: bool,
        /// Skip the prune confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Add a skill: edit manifest → lock → sync.
    #[command(visible_alias = "a", after_help = ADD_EXAMPLES)]
    Add {
        /// Spec (smart) or explicit form, e.g. `git <url>`, `local <path>`.
        #[arg(required = true, num_args = 1..)]
        spec: Vec<String>,
        /// Subdirectory inside the source.
        #[arg(long)]
        subdir: Option<String>,
        /// Git ref (branch / tag / commit).
        #[arg(long)]
        r#ref: Option<String>,
        /// Install directory name (inferred if omitted).
        #[arg(long)]
        name: Option<String>,
        /// Expected sha256 of a zip/tar archive.
        #[arg(long)]
        sha256: Option<String>,
        /// Agents (repeatable or comma-separated).
        #[arg(long, value_delimiter = ',')]
        agent: Vec<String>,
        /// Only edit manifest + lock; do not deploy.
        #[arg(long)]
        no_sync: bool,
        /// Allow creating a manifest in home/root, or overwrite.
        #[arg(long)]
        force: bool,
    },
    /// Re-resolve mutable refs and deploy (`lock --upgrade` + `sync`).
    #[command(visible_aliases = ["up", "upgrade"])]
    Update {
        /// Skills to upgrade (default: all). Immutable pins are no-ops.
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Compute the change plan without modifying disk.
        #[arg(long)]
        dry_run: bool,
        /// Use only local cache; never touch the network.
        #[arg(long)]
        offline: bool,
        /// Disable prune.
        #[arg(long)]
        no_prune: bool,
        /// Skip the prune confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Remove skills from the manifest (and prune on the next sync).
    #[command(visible_alias = "rm")]
    Remove {
        /// Skill names to remove.
        #[arg(required = true)]
        names: Vec<String>,
        /// Only edit the manifest; defer disk/lock changes to the next sync.
        #[arg(long)]
        no_sync: bool,
    },
    /// Show the state of every (skill, agent).
    #[command(visible_alias = "st")]
    Status,
    /// Diagnose the environment.
    Doctor,
    /// Inspect or clean the global source-artifact cache.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
}

#[derive(Subcommand)]
enum CacheAction {
    /// Print the cache directory path.
    Dir,
    /// Remove all cached source artifacts.
    Clean,
}

fn main() {
    let cli = Cli::parse();
    ui::init(cli.color.into());
    let result = dispatch(cli);
    match result {
        Ok(code) => std::process::exit(code.code()),
        Err(e) => {
            ui::error!("{}", ui::strip_prefix(&e.message));
            std::process::exit(e.exit.code());
        }
    }
}

fn dispatch(cli: Cli) -> error::Result<ExitCode> {
    let global = cli.global;
    match cli.command {
        Commands::Init { force } => cmd::init::run(global, force),
        Commands::Lock { upgrade } => cmd::lock::run(global, upgrade),
        Commands::Sync {
            frozen,
            locked,
            dry_run,
            offline,
            prune,
            no_prune,
            yes,
        } => cmd::sync::run(cmd::sync::SyncArgs {
            global,
            frozen,
            locked,
            dry_run,
            offline,
            prune,
            no_prune,
            yes,
            upgrade: crate::pipeline::resolve::Upgrade::None,
        }),
        Commands::Update {
            names,
            dry_run,
            offline,
            no_prune,
            yes,
        } => cmd::update::run(cmd::update::UpdateArgs {
            global,
            names,
            dry_run,
            offline,
            no_prune,
            yes,
        }),
        Commands::Add {
            spec,
            subdir,
            r#ref,
            name,
            sha256,
            agent,
            no_sync,
            force,
        } => cmd::add::run(cmd::add::AddArgs {
            global,
            spec,
            subdir,
            ref_: r#ref,
            name,
            sha256,
            agent,
            no_sync,
            force,
        }),
        Commands::Remove { names, no_sync } => cmd::remove::run(global, names, no_sync),
        Commands::Status => cmd::status::run(global),
        Commands::Doctor => cmd::doctor::run(global),
        Commands::Cache { action } => match action {
            CacheAction::Dir => cmd::cache::dir(),
            CacheAction::Clean => cmd::cache::clean(),
        },
    }
}
