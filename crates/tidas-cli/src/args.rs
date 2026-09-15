use std::io::{self, Write};
use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use tidas_contracts::{CommandNameV1, LogLevelV1, ProgressModeV1};

pub const DEFAULT_MEMORY_BUDGET_MIB: u64 = 512;
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;
pub const DEFAULT_IMPORT_MAX_ENTRY_MIB: u64 = 128;
pub const DEFAULT_IMPORT_MAX_ISSUE_KIB: u64 = 64;

#[derive(Debug, Parser)]
#[command(
    name = "tidas",
    version,
    disable_help_subcommand = true,
    about = "Cross-platform TIDAS conversion, import, export, validation, and release tooling",
    long_about = "The unified TIDAS executable. Domain behavior lives in reusable Rust crates; this binary only parses inputs, supplies bounded runtime controls, and renders stable human or JSON reports.",
    after_help = "Examples:\n  tidas version --format json\n  tidas --completion bash > tidas.bash\n\nPrecedence: command-line options override TIDAS_* environment variables, which override documented built-in defaults. No configuration file is loaded implicitly from the current directory.\n\nStdout contains only the requested report or completion script. Logs, progress, diagnostics, and file-write confirmations use stderr. Product commands dispatch only to Rust domain crates and never invoke Python."
)]
pub struct Cli {
    /// Reading-oriented human output or stable machine-readable JSON.
    #[arg(long, value_enum, default_value_t = OutputFormat::Human, global = true)]
    pub format: OutputFormat,

    /// Persist the complete report atomically instead of writing it to stdout.
    #[arg(long, value_name = "PATH", global = true)]
    pub report: Option<PathBuf>,

    /// Explicit configuration file; otherwise `TIDAS_CONFIG` is used when set.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Diagnostic verbosity; may also be set with `TIDAS_LOG`.
    #[arg(
        long,
        value_enum,
        env = "TIDAS_LOG",
        default_value_t = CliLogLevel::Warn,
        global = true
    )]
    pub log_level: CliLogLevel,

    /// Progress policy; may also be set with `TIDAS_PROGRESS`.
    #[arg(
        long,
        value_enum,
        env = "TIDAS_PROGRESS",
        default_value_t = CliProgressMode::Auto,
        global = true
    )]
    pub progress: CliProgressMode,

    /// Maximum accounted in-flight memory in MiB.
    #[arg(
        long,
        env = "TIDAS_MEMORY_BUDGET_MIB",
        default_value_t = DEFAULT_MEMORY_BUDGET_MIB,
        value_parser = parse_positive_u64,
        global = true
    )]
    pub memory_budget_mib: u64,

    /// Maximum number of items in each bounded work queue.
    #[arg(
        long,
        env = "TIDAS_QUEUE_CAPACITY",
        default_value_t = DEFAULT_QUEUE_CAPACITY,
        value_parser = parse_positive_usize,
        global = true
    )]
    pub queue_capacity: usize,

    /// Write a deterministic shell completion script to stdout and exit.
    #[arg(long, value_enum, value_name = "SHELL")]
    pub completion: Option<Shell>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    pub fn try_parse_checked() -> Result<Self, clap::Error> {
        let cli = Self::try_parse()?;
        cli.validate()?;
        Ok(cli)
    }

    fn validate(&self) -> Result<(), clap::Error> {
        match (self.completion, self.command.as_ref()) {
            (None, None) => Err(Self::command().error(
                ErrorKind::MissingSubcommand,
                "a product command is required; use `tidas --help` or request `--completion <shell>`",
            )),
            (Some(_), Some(_)) => Err(Self::command().error(
                ErrorKind::ArgumentConflict,
                "--completion generates a script and cannot be combined with a product command",
            )),
            (Some(_), None) if self.report.is_some() => Err(Self::command().error(
                ErrorKind::ArgumentConflict,
                "--completion writes to stdout and cannot be combined with --report",
            )),
            _ => {
                self.memory_budget_mib.checked_mul(1024 * 1024).ok_or_else(|| {
                    Self::command().error(
                        ErrorKind::InvalidValue,
                        "--memory-budget-mib is too large to represent in bytes",
                    )
                })?;
                match &self.command {
                    Some(Commands::Import(arguments)) => {
                        arguments.validate(&Self::command())?;
                    }
                    Some(Commands::Export(arguments)) => {
                        arguments.validate(&Self::command())?;
                    }
                    Some(Commands::Validate(arguments)) => {
                        arguments.validate(&Self::command())?;
                    }
                    _ => {}
                }
                Ok(())
            }
        }
    }

    #[must_use]
    pub const fn memory_budget_bytes(&self) -> u64 {
        self.memory_budget_mib * 1024 * 1024
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<CliLogLevel> for LogLevelV1 {
    fn from(value: CliLogLevel) -> Self {
        match value {
            CliLogLevel::Error => Self::Error,
            CliLogLevel::Warn => Self::Warn,
            CliLogLevel::Info => Self::Info,
            CliLogLevel::Debug => Self::Debug,
            CliLogLevel::Trace => Self::Trace,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CliProgressMode {
    Auto,
    Never,
    Always,
}

impl From<CliProgressMode> for ProgressModeV1 {
    fn from(value: CliProgressMode) -> Self {
        match value {
            CliProgressMode::Auto => Self::Auto,
            CliProgressMode::Never => Self::Never,
            CliProgressMode::Always => Self::Always,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum Commands {
    /// Convert between TIDAS JSON and eILCD XML.
    Convert(ConvertArgs),
    /// Import supported external LCA formats into TIDAS.
    Import(ImportArgs),
    /// Export database records and external documents as a package.
    Export(ExportArgs),
    /// Validate TIDAS JSON or eILCD/ILCD XML.
    Validate(ValidateArgs),
    /// Build and verify deterministic release packages.
    Release(ReleaseArgs),
    /// Inspect and validate packaged methodology rulesets.
    Ruleset(RulesetArgs),
    /// Print binary, contract, asset, XML engine, and runtime fingerprints.
    Version,
}

impl Commands {
    #[must_use]
    pub const fn name(&self) -> CommandNameV1 {
        match self {
            Self::Convert(_) => CommandNameV1::Convert,
            Self::Import(_) => CommandNameV1::Import,
            Self::Export(_) => CommandNameV1::Export,
            Self::Validate(_) => CommandNameV1::Validate,
            Self::Release(_) => CommandNameV1::Release,
            Self::Ruleset(_) => CommandNameV1::Ruleset,
            Self::Version => CommandNameV1::Version,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
#[command(
    long_about = "Convert a package between TIDAS JSON and eILCD XML, or use --to reference-unit with a tidas.flow-property-conversion-request.v1 JSON file (explicit - reads stdin). Reference-unit mode is report-only, supports Product/Waste Flow quantities and fixed decimal rounding, and rejects --output. The input tree is mirrored under OUTPUT/data, non-convertible files are preserved, and the locked schemas, stylesheets, or methodologies for the target format are materialized beside it. Publication is atomic: malformed input, cancellation, or runtime failure leaves an existing output unchanged.",
    after_help = "Examples:\n  tidas convert ./package --output ./eilcd-package --to ilcd\n  tidas convert ./eilcd-data --output ./tidas-package --to tidas --format json\n\nNext: validate the generated OUTPUT/data directory with `tidas validate` and the corresponding --input-format."
)]
pub struct ConvertArgs {
    /// Package directory, or reference-unit request JSON file (explicit - reads stdin).
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Target package directory to publish atomically.
    #[arg(long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    /// Target representation and locked asset family.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub to: ConversionTarget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ConversionTarget {
    Ilcd,
    Tidas,
    /// Convert a fixed product/waste Flow quantity to its reference unit; report only.
    ReferenceUnit,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
#[command(
    long_about = "Import one external LCA source into a deterministic, schema-validated TIDAS package and optionally an eILCD package. Supported inputs are EcoSpold 1, EcoSpold 2, SimaPro CSV, openLCA JSON-LD, openLCA process XLSX, and ILCD/eILCD. A .zolca database is intentionally rejected; export it to a supported exchange format first. Publication is atomic and all large outputs are written below OUTPUT.",
    after_help = "Examples:\n  tidas import ./database.zip --output ./imported\n  tidas import ./processes.csv --from-format simapro-csv --output ./imported --target both --write-mapping\n  tidas import ./database.jsonld --output ./imported --no-process-bundles --format json\n\nA .zolca database is intentionally rejected; export it to a supported exchange format first.\n\nOutputs:\n  OUTPUT/import-report.json and OUTPUT/issues.jsonl\n  OUTPUT/tidas when --target is tidas or both\n  OUTPUT/ilcd when --target is ilcd or both\n  OUTPUT/process-bundles by default\n  OUTPUT/mapping.csv.gz when --write-mapping is used\n\nNext: run `tidas validate OUTPUT/tidas --input-format tidas-json` or validate OUTPUT/ilcd with --input-format ilcd-xml."
)]
pub struct ImportArgs {
    /// Source file, directory, ZIP package, or XLSX workbook to import.
    #[arg(value_name = "INPUT")]
    pub input: PathBuf,

    /// Target directory to publish atomically.
    #[arg(long, value_name = "DIR")]
    pub output: PathBuf,

    /// Explicit source format; omit to use bounded signature detection.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub from_format: Option<ImportSourceFormat>,

    /// Generated package representation.
    #[arg(long, value_enum, default_value_t = ImportTargetArg::Tidas)]
    pub target: ImportTargetArg,

    /// Write the deterministic expert mapping artifact to OUTPUT/mapping.csv.gz.
    #[arg(long)]
    pub write_mapping: bool,

    /// Skip per-process dependency bundles, which are written by default.
    #[arg(long)]
    pub no_process_bundles: bool,

    /// Return data-issues (2) when the import completes with warnings.
    #[arg(long)]
    pub fail_on_warning: bool,

    /// Reject any individual source entry larger than this many MiB.
    #[arg(
        long,
        default_value_t = DEFAULT_IMPORT_MAX_ENTRY_MIB,
        value_parser = parse_positive_u64
    )]
    pub max_entry_mib: u64,
}

impl ImportArgs {
    fn validate(&self, command: &clap::Command) -> Result<(), clap::Error> {
        self.max_entry_mib.checked_mul(1024 * 1024).ok_or_else(|| {
            command.clone().error(
                ErrorKind::InvalidValue,
                "--max-entry-mib is too large to represent in bytes",
            )
        })?;
        Ok(())
    }

    #[must_use]
    pub const fn max_entry_bytes(&self) -> u64 {
        self.max_entry_mib * 1024 * 1024
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ImportSourceFormat {
    Ecospold1,
    Ecospold2,
    SimaproCsv,
    OpenlcaJsonld,
    OpenlcaProcessXlsx,
    Ilcd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ImportTargetArg {
    Tidas,
    Ilcd,
    Both,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
#[command(
    long_about = "Export the active records in a PostgreSQL TIDAS database into one deterministic TIDAS JSON or eILCD XML ZIP. Database rows are read through a repeatable-read, read-only snapshot and a bounded queue. Optional S3-compatible external documents are streamed into the package. Publication is atomic: failure or cancellation leaves an existing ZIP unchanged.",
    after_help = "Examples:\n  TIDAS_DATABASE_URL='postgresql://…' tidas export --output ./tidas.zip\n  TIDAS_DATABASE_URL='postgresql://…' TIDAS_S3_ACCESS_KEY_ID='…' TIDAS_S3_SECRET_ACCESS_KEY='…' tidas export --output ./tidas.zip --external-docs-bucket documents --s3-endpoint http://127.0.0.1:9000\n  TIDAS_DATABASE_URL='postgresql://…' tidas export --output ./eilcd.zip --target ilcd --skip-external-docs --format json\n\nCredentials are accepted only through TIDAS_DATABASE_URL, TIDAS_S3_ACCESS_KEY_ID, TIDAS_S3_SECRET_ACCESS_KEY, and optional TIDAS_S3_SESSION_TOKEN. Reports and diagnostics never include their values."
)]
pub struct ExportArgs {
    /// Deterministic ZIP path to publish atomically.
    #[arg(long, value_name = "ZIP")]
    pub output: PathBuf,

    /// Exported record representation.
    #[arg(long, value_enum, default_value_t = ExportTargetArg::Tidas)]
    pub target: ExportTargetArg,

    /// S3-compatible bucket containing external documents.
    #[arg(long, env = "TIDAS_S3_BUCKET", value_name = "BUCKET")]
    pub external_docs_bucket: Option<String>,

    /// S3-compatible service region.
    #[arg(long, env = "TIDAS_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,

    /// Custom S3-compatible endpoint, such as a local `MinIO` service.
    #[arg(long, env = "TIDAS_S3_ENDPOINT", value_name = "URL")]
    pub s3_endpoint: Option<String>,

    /// Optional object-key prefix to export.
    #[arg(long, env = "TIDAS_S3_PREFIX", value_name = "PREFIX")]
    pub s3_prefix: Option<String>,

    /// Intentionally omit all external documents.
    #[arg(long)]
    pub skip_external_docs: bool,

    /// Timeout for each object-storage list, get, or chunk operation.
    #[arg(
        long,
        default_value_t = 60,
        value_parser = parse_positive_u64,
        value_name = "SECONDS"
    )]
    pub network_timeout_seconds: u64,
}

impl ExportArgs {
    fn validate(&self, command: &clap::Command) -> Result<(), clap::Error> {
        if self.skip_external_docs
            && (self.external_docs_bucket.is_some()
                || self.s3_endpoint.is_some()
                || self.s3_prefix.is_some())
        {
            return Err(command.clone().error(
                ErrorKind::ArgumentConflict,
                "--skip-external-docs cannot be combined with S3 bucket, endpoint, or prefix options",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ExportTargetArg {
    Tidas,
    Ilcd,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
#[command(
    long_about = "Run native release-control operations over a finalized canonical TIDAS tree. The release layer never assigns UUIDs or versions. End-to-end builds validate TIDAS, resolve exact reference closure, derive and validate eILCD, prove normalized semantic round-trip, and atomically publish four byte-stable ZIPs.",
    after_help = "Examples:\n  tidas release build-packages --tidas-dir ./canonical --dataset-index ./canonical-dataset-index.json --output-dir ./release\n  tidas release validate-closure --input-dir ./canonical --dataset-index ./canonical-dataset-index.json --profile unit-process-full-closure.v1 --format json\n  tidas release semantic-roundtrip --tidas-dir ./canonical --ilcd-dir ./derived-ilcd\n\nAll actions use the global memory budget, bounded runtime controls, and cancellation token. Build publication is atomic and never invokes Python."
)]
pub struct ReleaseArgs {
    #[command(subcommand)]
    pub action: ReleaseCommand,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum ReleaseCommand {
    /// Validate, convert, prove closure/round-trip, and publish four ZIPs.
    BuildPackages(ReleaseBuildArgs),
    /// Convert canonical TIDAS JSON to a schema-ordered eILCD tree.
    ConvertIlcd(ReleaseConvertArgs),
    /// Compare normalized canonical JSON with derived eILCD XML.
    SemanticRoundtrip(ReleaseRoundtripArgs),
    /// Resolve one exact UUID/version transitive closure.
    ValidateClosure(ReleaseClosureArgs),
    /// Run native eILCD validation as a release gate.
    ValidateIlcd(ReleaseValidateArgs),
    /// Run native TIDAS validation as a release gate.
    ValidateTidas(ReleaseValidateArgs),
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct ReleaseBuildArgs {
    /// Finalized canonical TIDAS dataset tree.
    #[arg(long, value_name = "DIR")]
    pub tidas_dir: PathBuf,
    /// Canonical dataset identity/hash index.
    #[arg(long, value_name = "JSON")]
    pub dataset_index: PathBuf,
    /// Directory containing exactly four published ZIPs after success.
    #[arg(long, value_name = "DIR")]
    pub output_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct ReleaseConvertArgs {
    /// Finalized canonical TIDAS dataset tree.
    #[arg(long, value_name = "DIR")]
    pub input_dir: PathBuf,
    /// Derived eILCD package directory to publish atomically.
    #[arg(long, value_name = "DIR")]
    pub output_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct ReleaseRoundtripArgs {
    /// Canonical TIDAS dataset tree.
    #[arg(long, value_name = "DIR")]
    pub tidas_dir: PathBuf,
    /// Derived eILCD package tree.
    #[arg(long, value_name = "DIR")]
    pub ilcd_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct ReleaseClosureArgs {
    /// Finalized canonical TIDAS dataset tree.
    #[arg(long, value_name = "DIR")]
    pub input_dir: PathBuf,
    /// Canonical dataset identity/hash index.
    #[arg(long, value_name = "JSON")]
    pub dataset_index: PathBuf,
    /// Required release closure profile.
    #[arg(long, value_enum)]
    pub profile: ReleaseProfileArg,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ReleaseProfileArg {
    #[value(name = "unit-process-full-closure.v1")]
    UnitProcess,
    #[value(name = "standalone-lifecyclemodel-result-full-closure.v1")]
    StandaloneResult,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct ReleaseValidateArgs {
    /// TIDAS or eILCD package directory.
    #[arg(long, value_name = "DIR")]
    pub input_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct RulesetArgs {
    /// Return the ordered rules for one ruleset id; omit to inspect the catalog.
    #[arg(long, value_name = "RULESET_ID")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, clap::Args)]
pub struct ValidateArgs {
    /// Package directory containing canonical TIDAS category subdirectories.
    #[arg(value_name = "INPUT")]
    pub input: Option<PathBuf>,

    /// Input representation to validate.
    #[arg(long, value_enum, default_value_t = ValidationInputFormat::TidasJson)]
    pub input_format: ValidationInputFormat,

    /// Validation execution protocol.
    #[arg(long, value_enum, default_value_t = ValidationProtocol::Package)]
    pub protocol: ValidationProtocol,

    /// JSONL manifest required by document-validation-batch.v1.
    #[arg(long, value_name = "PATH")]
    pub input_manifest: Option<PathBuf>,

    /// Atomically write the complete deterministic issue stream as JSONL.
    #[arg(long, value_name = "PATH")]
    pub issues: Option<PathBuf>,

    /// Atomically write batch issue and final events as canonical JSONL.
    #[arg(long, value_name = "PATH")]
    pub events: Option<PathBuf>,

    /// Print the validation protocol and engine fingerprint handshake.
    #[arg(long)]
    pub describe: bool,

    /// Run only native TIDAS schema and semantic checks. This is diagnostic and does not prove eILCD convertibility.
    #[arg(long)]
    pub schema_only: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ValidationInputFormat {
    TidasJson,
    IlcdXml,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ValidationProtocol {
    Package,
    #[value(name = "document-validation-batch.v1")]
    DocumentValidationBatchV1,
}

impl ValidateArgs {
    fn validate(&self, command: &clap::Command) -> Result<(), clap::Error> {
        let invalid = |message| command.clone().error(ErrorKind::InvalidValue, message);
        if self.describe {
            if self.input.is_some()
                || self.input_manifest.is_some()
                || self.issues.is_some()
                || self.events.is_some()
                || self.schema_only
            {
                return Err(invalid(
                    "--describe cannot be combined with input or output paths",
                ));
            }
            return Ok(());
        }
        if self.input.is_none() {
            return Err(command.clone().error(
                ErrorKind::MissingRequiredArgument,
                "validate requires INPUT unless --describe is used",
            ));
        }
        match self.protocol {
            ValidationProtocol::Package => {
                if self.input_manifest.is_some() || self.events.is_some() {
                    return Err(invalid(
                        "--input-manifest and --events require --protocol document-validation-batch.v1",
                    ));
                }
                if self.schema_only && self.input_format != ValidationInputFormat::TidasJson {
                    return Err(invalid("--schema-only applies to TIDAS JSON input only"));
                }
            }
            ValidationProtocol::DocumentValidationBatchV1 => {
                if self.input_format != ValidationInputFormat::TidasJson {
                    return Err(invalid(
                        "document-validation-batch.v1 supports --input-format tidas-json only",
                    ));
                }
                if self.input_manifest.is_none() || self.events.is_none() {
                    return Err(command.clone().error(
                        ErrorKind::MissingRequiredArgument,
                        "document-validation-batch.v1 requires --input-manifest and --events",
                    ));
                }
                if self.issues.is_some() {
                    return Err(invalid(
                        "document-validation-batch.v1 uses --events instead of --issues",
                    ));
                }
                if self.schema_only {
                    return Err(invalid(
                        "--schema-only is not part of document-validation-batch.v1",
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn write_completion(shell: Shell, writer: &mut impl Write) -> io::Result<()> {
    let mut command = Cli::command();
    let mut bytes = Vec::new();
    clap_complete::generate(shell, &mut command, "tidas", &mut bytes);
    writer.write_all(&bytes)
}

fn parse_positive_u64(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|error| format!("expected a positive integer: {error}"))
        .and_then(|parsed| {
            if parsed == 0 {
                Err("value must be greater than zero".to_owned())
            } else {
                Ok(parsed)
            }
        })
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|error| format!("expected a positive integer: {error}"))
        .and_then(|parsed| {
            if parsed == 0 {
                Err("value must be greater than zero".to_owned())
            } else {
                Ok(parsed)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clap_contract_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn only_the_final_command_names_are_registered() {
        let command = Cli::command();
        let names: Vec<_> = command
            .get_subcommands()
            .map(clap::Command::get_name)
            .collect();
        assert_eq!(
            names,
            [
                "convert", "import", "export", "validate", "release", "ruleset", "version"
            ]
        );
    }

    #[test]
    fn every_supported_completion_is_repeatable() {
        for shell in [
            Shell::Bash,
            Shell::Elvish,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Zsh,
        ] {
            let mut first = Vec::new();
            let mut second = Vec::new();
            write_completion(shell, &mut first).unwrap();
            write_completion(shell, &mut second).unwrap();
            assert_eq!(first, second);
            assert!(!first.is_empty());
        }
    }
}
