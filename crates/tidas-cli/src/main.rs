mod args;
mod context;
mod measurement;
mod output;

use std::env;
use std::process::ExitCode;
use std::time::Duration;

use args::{
    Cli, Commands, ConversionTarget, ConvertArgs, ExportArgs, ExportTargetArg, ImportArgs,
    ImportSourceFormat, ImportTargetArg, ReleaseArgs, ReleaseCommand, ReleaseProfileArg,
    RulesetArgs, ValidateArgs, ValidationInputFormat, ValidationProtocol,
};
use clap::error::ErrorKind;
use context::ExecutionContext;
use tidas_assets::{asset_fingerprint, bundled_assets};
use tidas_contracts::{
    ArtifactRefV1, CommandNameV1, DiagnosticV1, ExitClass, INVOCATION_CONTEXT_SCHEMA_V1,
    OPERATION_REPORT_SCHEMA_V1, OperationReportV1,
};
use tidas_conversion::{
    ConversionDirection, ConversionError, ConversionProgressReporter, ConversionRequest,
    convert_directory,
};
use tidas_export::{ExportError, ExportFormat, ExportRequest, S3Config, SecretString, run_export};
use tidas_import::{
    ImportCoreError, ImportExecutionError, ImportRequest, ImportTarget, PackageWriteError,
    SourceFormat, run_import,
};
use tidas_release::{
    RELEASE_REPORT_SCHEMA_V1, ReleaseError, ReleaseProfile, ReleaseRequest, ReleaseRuntime,
    run_release,
};
use tidas_rulesets::{RulesetCatalog, RulesetError};
use tidas_runtime::RuntimeError;
use tidas_validation::{
    BatchValidationOutput, BatchValidationRequest, DOCUMENT_VALIDATION_PROFILE, ValidationError,
    ValidationProgressReporter, ValidationRequest, ValidationSummaryV1,
    describe_document_validation, run_document_validation_batch, validate_ilcd_package,
    validate_tidas_package,
};
use tidas_xml::engine_decision;

fn main() -> ExitCode {
    let cli = match Cli::try_parse_checked() {
        Ok(cli) => cli,
        Err(error) => return print_clap_error(&error),
    };

    if let Some(shell) = cli.completion {
        return match args::write_completion(shell, &mut std::io::stdout().lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("failed to write {shell} completion script: {error}");
                ExitCode::from(ExitClass::Io.code())
            }
        };
    }

    let execution = ExecutionContext::from_cli(&cli);
    let command = cli
        .command
        .clone()
        .expect("checked CLI always has a command unless completion was requested");
    let command_name = command.name();
    let report = match execution.install_cancellation_handler() {
        Ok(()) => dispatch(command, &execution),
        Err(error) => OperationReportV1::failed(
            command_name,
            ExitClass::Internal,
            "cancellation_handler_unavailable",
            format!("Failed to install the process cancellation handler: {error}"),
        ),
    }
    .with_invocation(execution.invocation.clone());

    let exit_class = report.exit_class;
    match output::render(&cli, &report) {
        Ok(()) => ExitCode::from(exit_class.code()),
        Err(error) => {
            eprintln!("tidas failed to render its report: {error}");
            ExitCode::from(ExitClass::Io.code())
        }
    }
}

fn print_clap_error(error: &clap::Error) -> ExitCode {
    let kind = error.kind();
    let code = if matches!(kind, ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        ExitClass::Success
    } else {
        ExitClass::Usage
    };
    if let Err(print_error) = error.print() {
        eprintln!("failed to print CLI guidance: {print_error}");
        return ExitCode::from(ExitClass::Io.code());
    }
    ExitCode::from(code.code())
}

fn dispatch(command: Commands, execution: &ExecutionContext) -> OperationReportV1 {
    if execution.cancellation.is_cancelled() {
        return OperationReportV1::cancelled(command.name());
    }

    match command {
        Commands::Convert(arguments) => conversion_report(&arguments, execution),
        Commands::Import(arguments) => import_report(&arguments, execution),
        Commands::Export(arguments) => export_report(&arguments, execution),
        Commands::Validate(arguments) => validation_report(&arguments, execution),
        Commands::Release(arguments) => release_report(&arguments, execution),
        Commands::Ruleset(arguments) => ruleset_report(&arguments),
        Commands::Version => version_report(execution),
    }
}

fn release_report(arguments: &ReleaseArgs, execution: &ExecutionContext) -> OperationReportV1 {
    let request = match &arguments.action {
        ReleaseCommand::BuildPackages(arguments) => ReleaseRequest::BuildPackages {
            tidas_dir: arguments.tidas_dir.clone(),
            dataset_index: arguments.dataset_index.clone(),
            output_dir: arguments.output_dir.clone(),
        },
        ReleaseCommand::ConvertIlcd(arguments) => ReleaseRequest::ConvertIlcd {
            input_dir: arguments.input_dir.clone(),
            output_dir: arguments.output_dir.clone(),
        },
        ReleaseCommand::SemanticRoundtrip(arguments) => ReleaseRequest::SemanticRoundtrip {
            tidas_dir: arguments.tidas_dir.clone(),
            ilcd_dir: arguments.ilcd_dir.clone(),
        },
        ReleaseCommand::ValidateClosure(arguments) => ReleaseRequest::ValidateClosure {
            input_dir: arguments.input_dir.clone(),
            dataset_index: arguments.dataset_index.clone(),
            profile: match arguments.profile {
                ReleaseProfileArg::UnitProcess => ReleaseProfile::UnitProcess,
                ReleaseProfileArg::StandaloneResult => ReleaseProfile::StandaloneResult,
            },
        },
        ReleaseCommand::ValidateIlcd(arguments) => ReleaseRequest::ValidateIlcd {
            input_dir: arguments.input_dir.clone(),
        },
        ReleaseCommand::ValidateTidas(arguments) => ReleaseRequest::ValidateTidas {
            input_dir: arguments.input_dir.clone(),
        },
    };
    let runtime = ReleaseRuntime {
        cancellation: execution.cancellation.clone(),
        memory_budget: execution.memory_budget.clone(),
        queue_capacity: execution.invocation.queue_capacity,
    };
    match run_release(&request, &runtime) {
        Ok(release) => completed_release_report(release),
        Err(error)
            if matches!(
                runtime_error_in_chain(&error),
                Some(RuntimeError::Cancelled)
            ) =>
        {
            OperationReportV1::cancelled(CommandNameV1::Release)
        }
        Err(error) => failed_release_report(&error),
    }
}

fn completed_release_report(release: tidas_release::ReleaseReportV1) -> OperationReportV1 {
    let has_issues = !release.ok;
    let artifacts = release
        .build
        .as_ref()
        .map(|build| build.packages.clone())
        .unwrap_or_default();
    let value = match serde_json::to_value(release) {
        Ok(value) => value,
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Release,
                ExitClass::Internal,
                "release_summary_serialization_failed",
                error.to_string(),
            );
        }
    };
    let mut report = if has_issues {
        OperationReportV1::completed_with_issues(
            CommandNameV1::Release,
            DiagnosticV1::new(
                "release_data_issues",
                "The native release action completed and found data issues.",
            ),
        )
    } else {
        OperationReportV1::succeeded(CommandNameV1::Release)
    };
    report.summary.insert("release".to_owned(), value);
    report
        .artifacts
        .extend(artifacts.into_iter().map(|package| ArtifactRefV1 {
            path: package.artifact.path,
            media_type: package.artifact.media_type,
            sha256: Some(package.artifact.sha256),
            bytes: Some(package.artifact.bytes),
        }));
    report
}

fn failed_release_report(error: &ReleaseError) -> OperationReportV1 {
    let (exit_class, code) = match error {
        ReleaseError::ZeroQueueCapacity | ReleaseError::OutputInsideInput(_) => {
            (ExitClass::Usage, "invalid_release_request")
        }
        ReleaseError::DatasetIndexInvalid(_)
        | ReleaseError::DatasetIndexSchemaUnsupported(_)
        | ReleaseError::DatasetIndexEmpty
        | ReleaseError::DuplicateDatasetIdentity(_)
        | ReleaseError::DuplicateDatasetPath(_)
        | ReleaseError::DatasetFileMissing(_)
        | ReleaseError::DatasetFileHashMismatch(_)
        | ReleaseError::ProfileRootsMissing(_)
        | ReleaseError::ReferenceVersionMissing(_)
        | ReleaseError::ReferenceClosureMissing(_)
        | ReleaseError::StandaloneMissingUnitClosure(_)
        | ReleaseError::DatasetJson { .. }
        | ReleaseError::ValidationIssues(_)
        | ReleaseError::SemanticRoundtripIssues { .. } => {
            (ExitClass::DataIssues, "release_input_invalid")
        }
        ReleaseError::InputNotDirectory(_)
        | ReleaseError::OutputNotDirectory(_)
        | ReleaseError::CommitRollback { .. }
        | ReleaseError::Zip(_)
        | ReleaseError::Walk(_)
        | ReleaseError::Io(_) => (ExitClass::Io, "release_io_failed"),
        ReleaseError::Runtime(RuntimeError::BudgetExceeded { .. }) => {
            (ExitClass::Internal, "memory_budget_exceeded")
        }
        ReleaseError::Runtime(_)
        | ReleaseError::DuplicateArchiveMember(_)
        | ReleaseError::OrderingSchemaMissing(_)
        | ReleaseError::OrderingSchemaReference(_)
        | ReleaseError::OrderingSchemaCycle(_)
        | ReleaseError::InvalidGeneratedPath
        | ReleaseError::SizeOverflow
        | ReleaseError::Validation(_)
        | ReleaseError::Asset(_)
        | ReleaseError::Json(_) => (ExitClass::Internal, "release_runtime_failed"),
        ReleaseError::Conversion(error) => classify_release_conversion_error(error),
        ReleaseError::UnsafePath(_)
        | ReleaseError::PathOutsideRoot(_)
        | ReleaseError::Symlink(_) => (ExitClass::DataIssues, "release_input_invalid"),
    };
    OperationReportV1::failed(CommandNameV1::Release, exit_class, code, error.to_string())
}

fn classify_release_conversion_error(error: &ConversionError) -> (ExitClass, &'static str) {
    match error {
        ConversionError::OutputInsideInput(_) | ConversionError::ZeroQueueCapacity => {
            (ExitClass::Usage, "invalid_release_request")
        }
        ConversionError::InputNotDirectory(_)
        | ConversionError::OutputNotDirectory(_)
        | ConversionError::InvalidOutput(_)
        | ConversionError::SourceChanged(_)
        | ConversionError::CommitRollback { .. }
        | ConversionError::Io(_)
        | ConversionError::Walk(_) => (ExitClass::Io, "release_io_failed"),
        ConversionError::Runtime(RuntimeError::BudgetExceeded { .. }) => {
            (ExitClass::Internal, "memory_budget_exceeded")
        }
        ConversionError::Runtime(_)
        | ConversionError::PathOutsideInput(_)
        | ConversionError::NonPortablePath(_)
        | ConversionError::OrderingSchemaMissing(_)
        | ConversionError::OrderingSchemaReference(_)
        | ConversionError::OrderingSchemaCycle(_)
        | ConversionError::SizeOverflow
        | ConversionError::Asset(_) => (ExitClass::Internal, "release_runtime_failed"),
        _ => (ExitClass::DataIssues, "release_input_invalid"),
    }
}

fn export_report(arguments: &ExportArgs, execution: &ExecutionContext) -> OperationReportV1 {
    let Some(database_url) = secret_environment("TIDAS_DATABASE_URL") else {
        return OperationReportV1::failed(
            CommandNameV1::Export,
            ExitClass::Usage,
            "missing_database_credentials",
            "TIDAS_DATABASE_URL is required for database export.",
        );
    };
    let mut request = ExportRequest::new(
        SecretString::new(database_url),
        arguments.output.clone(),
        match arguments.target {
            ExportTargetArg::Tidas => ExportFormat::Tidas,
            ExportTargetArg::Ilcd => ExportFormat::Ilcd,
        },
        execution.cancellation.clone(),
        execution.memory_budget.clone(),
        execution.invocation.queue_capacity,
    );
    request.skip_external_documents = arguments.skip_external_docs;
    request.network_timeout = Duration::from_secs(arguments.network_timeout_seconds);
    if let Some(bucket) = &arguments.external_docs_bucket {
        let Some(access_key_id) = secret_environment("TIDAS_S3_ACCESS_KEY_ID") else {
            return OperationReportV1::failed(
                CommandNameV1::Export,
                ExitClass::Usage,
                "missing_export_credentials",
                "TIDAS_S3_ACCESS_KEY_ID is required when --external-docs-bucket is used.",
            );
        };
        let Some(secret_access_key) = secret_environment("TIDAS_S3_SECRET_ACCESS_KEY") else {
            return OperationReportV1::failed(
                CommandNameV1::Export,
                ExitClass::Usage,
                "missing_export_credentials",
                "TIDAS_S3_SECRET_ACCESS_KEY is required when --external-docs-bucket is used.",
            );
        };
        request.external_documents = Some(S3Config {
            bucket: bucket.clone(),
            region: arguments.s3_region.clone(),
            endpoint: arguments.s3_endpoint.clone(),
            prefix: arguments.s3_prefix.clone(),
            access_key_id: SecretString::new(access_key_id),
            secret_access_key: SecretString::new(secret_access_key),
            session_token: secret_environment("TIDAS_S3_SESSION_TOKEN").map(SecretString::new),
        });
    }
    match run_export(&request) {
        Ok(summary) => {
            let mut report = OperationReportV1::succeeded(CommandNameV1::Export);
            if let Ok(value) = serde_json::to_value(&summary) {
                report.summary.insert("export".to_owned(), value);
            } else {
                return OperationReportV1::failed(
                    CommandNameV1::Export,
                    ExitClass::Internal,
                    "export_summary_serialization_failed",
                    "The native export summary could not be serialized.",
                );
            }
            report.artifacts.push(ArtifactRefV1 {
                path: arguments.output.to_string_lossy().into_owned(),
                media_type: "application/zip".to_owned(),
                sha256: Some(summary.archive_sha256.clone()),
                bytes: Some(summary.archive_bytes),
            });
            if summary.external_documents_skipped {
                report.diagnostics.push(DiagnosticV1::new(
                    "external_documents_skipped",
                    "The database package completed without external documents.",
                ));
            }
            for warning in &summary.warnings {
                report
                    .diagnostics
                    .push(DiagnosticV1::new("export_warning", warning));
            }
            report.next_actions.push(format!(
                "Extract {} and validate the resulting package with `tidas validate`.",
                arguments.output.display()
            ));
            report
        }
        Err(error)
            if matches!(
                runtime_error_in_chain(&error),
                Some(RuntimeError::Cancelled)
            ) =>
        {
            OperationReportV1::cancelled(CommandNameV1::Export)
        }
        Err(error) => failed_export_report(&error),
    }
}

fn secret_environment(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn failed_export_report(error: &ExportError) -> OperationReportV1 {
    let (exit_class, code) = match error {
        ExportError::ZeroQueueCapacity => (ExitClass::Usage, "invalid_export_request"),
        ExportError::UnsafePath(_) | ExportError::Json(_) | ExportError::Conversion(_) => {
            (ExitClass::DataIssues, "export_source_invalid")
        }
        ExportError::DatabaseConnect(_)
        | ExportError::DatabaseTlsRoots
        | ExportError::Database(_)
        | ExportError::StorageConfiguration(_)
        | ExportError::Storage(_)
        | ExportError::StorageTimeout
        | ExportError::Zip(_)
        | ExportError::OutputNotRegularFile(_)
        | ExportError::Io(_)
        | ExportError::CommitRollback { .. } => (ExitClass::Io, "export_io_failed"),
        ExportError::DatabaseTask(_) | ExportError::RuntimeCreate(_) | ExportError::Runtime(_) => {
            (ExitClass::Internal, "export_runtime_failed")
        }
    };
    OperationReportV1::failed(CommandNameV1::Export, exit_class, code, error.to_string())
}

fn import_report(arguments: &ImportArgs, execution: &ExecutionContext) -> OperationReportV1 {
    let target = match arguments.target {
        ImportTargetArg::Tidas => ImportTarget::Tidas,
        ImportTargetArg::Ilcd => ImportTarget::Ilcd,
        ImportTargetArg::Both => ImportTarget::Both,
    };
    let requested_format = arguments.from_format.map(|format| match format {
        ImportSourceFormat::Ecospold1 => SourceFormat::Ecospold1,
        ImportSourceFormat::Ecospold2 => SourceFormat::Ecospold2,
        ImportSourceFormat::SimaproCsv => SourceFormat::SimaproCsv,
        ImportSourceFormat::OpenlcaJsonld => SourceFormat::OpenlcaJsonld,
        ImportSourceFormat::OpenlcaProcessXlsx => SourceFormat::OpenlcaProcessXlsx,
        ImportSourceFormat::Ilcd => SourceFormat::Ilcd,
    });
    let request = ImportRequest {
        source: arguments.input.clone(),
        requested_format,
        output_dir: arguments.output.clone(),
        target,
        write_mapping: arguments.write_mapping,
        write_process_bundles: !arguments.no_process_bundles,
        cancellation: execution.cancellation.clone(),
        memory_budget: execution.memory_budget.clone(),
        queue_capacity: execution.invocation.queue_capacity,
        max_entry_bytes: arguments.max_entry_bytes(),
        max_issue_bytes: usize::try_from(args::DEFAULT_IMPORT_MAX_ISSUE_KIB * 1024)
            .expect("the built-in import issue limit fits usize"),
    };
    match run_import(&request) {
        Ok(summary) => completed_import_report(arguments, &summary),
        Err(error)
            if matches!(
                runtime_error_in_chain(&error),
                Some(RuntimeError::Cancelled)
            ) =>
        {
            OperationReportV1::cancelled(CommandNameV1::Import)
        }
        Err(error) => failed_import_report(&error),
    }
}

fn completed_import_report(
    arguments: &ImportArgs,
    summary: &tidas_import::ImportExecutionReportV1,
) -> OperationReportV1 {
    let has_errors = summary.error_count > 0;
    let has_failing_warnings = arguments.fail_on_warning && summary.warning_count > 0;
    let mut report = if has_errors || has_failing_warnings {
        let message = if has_errors {
            format!(
                "Import completed with {} error issue(s); inspect {}/issues.jsonl.",
                summary.error_count,
                arguments.output.display()
            )
        } else {
            format!(
                "Import completed with {} warning(s) and --fail-on-warning was requested; inspect {}/issues.jsonl.",
                summary.warning_count,
                arguments.output.display()
            )
        };
        OperationReportV1::completed_with_issues(
            CommandNameV1::Import,
            DiagnosticV1::new("import_issues", message),
        )
    } else {
        OperationReportV1::succeeded(CommandNameV1::Import)
    };
    let summary_value = match serde_json::to_value(summary) {
        Ok(value) => value,
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Import,
                ExitClass::Internal,
                "import_summary_serialization_failed",
                error.to_string(),
            );
        }
    };
    report.summary.insert("import".to_owned(), summary_value);
    add_import_artifacts_and_actions(&mut report, arguments, summary);
    report
}

fn add_import_artifacts_and_actions(
    report: &mut OperationReportV1,
    arguments: &ImportArgs,
    summary: &tidas_import::ImportExecutionReportV1,
) {
    report.artifacts.push(ArtifactRefV1 {
        path: arguments.output.to_string_lossy().into_owned(),
        media_type: "application/vnd.tidas.import-directory".to_owned(),
        sha256: None,
        bytes: None,
    });
    report.artifacts.push(ArtifactRefV1 {
        path: arguments
            .output
            .join("import-report.json")
            .to_string_lossy()
            .into_owned(),
        media_type: "application/json".to_owned(),
        sha256: None,
        bytes: None,
    });
    report.artifacts.push(ArtifactRefV1 {
        path: arguments
            .output
            .join("issues.jsonl")
            .to_string_lossy()
            .into_owned(),
        media_type: "application/x-ndjson".to_owned(),
        sha256: None,
        bytes: None,
    });
    if matches!(
        arguments.target,
        ImportTargetArg::Tidas | ImportTargetArg::Both
    ) {
        report.artifacts.push(ArtifactRefV1 {
            path: arguments
                .output
                .join("tidas")
                .to_string_lossy()
                .into_owned(),
            media_type: "application/vnd.tidas.package-directory".to_owned(),
            sha256: Some(summary.tidas_package.output_tree_sha256.clone()),
            bytes: Some(summary.tidas_package.output_bytes),
        });
        report.next_actions.push(format!(
            "tidas validate {} --input-format tidas-json",
            arguments.output.join("tidas").display()
        ));
    }
    if matches!(
        arguments.target,
        ImportTargetArg::Ilcd | ImportTargetArg::Both
    ) {
        if let Some(conversion) = &summary.ilcd_conversion {
            report.artifacts.push(ArtifactRefV1 {
                path: arguments.output.join("ilcd").to_string_lossy().into_owned(),
                media_type: "application/vnd.ilcd.package-directory".to_owned(),
                sha256: Some(conversion.output_tree_sha256.clone()),
                bytes: Some(conversion.output_bytes),
            });
        }
        report.next_actions.push(format!(
            "tidas validate {} --input-format ilcd-xml",
            arguments.output.join("ilcd").display()
        ));
    }
    if let Some(mapping) = &summary.mapping {
        report.artifacts.push(ArtifactRefV1 {
            path: arguments
                .output
                .join("mapping.csv.gz")
                .to_string_lossy()
                .into_owned(),
            media_type: "application/gzip".to_owned(),
            sha256: Some(mapping.output_sha256.clone()),
            bytes: Some(mapping.output_bytes),
        });
    }
    if summary.process_bundles.is_some() {
        report.artifacts.push(ArtifactRefV1 {
            path: arguments
                .output
                .join("process-bundles")
                .to_string_lossy()
                .into_owned(),
            media_type: "application/vnd.tidas.process-bundles-directory".to_owned(),
            sha256: None,
            bytes: None,
        });
    }
}

fn failed_import_report(error: &ImportExecutionError) -> OperationReportV1 {
    if matches!(
        runtime_error_in_chain(error),
        Some(RuntimeError::BudgetExceeded { .. })
    ) {
        return OperationReportV1::failed(
            CommandNameV1::Import,
            ExitClass::Internal,
            "memory_budget_exceeded",
            error.to_string(),
        );
    }
    let (exit_class, code) = match error {
        ImportExecutionError::ZeroQueueCapacity | ImportExecutionError::OutputNestedInSource(_) => {
            (ExitClass::Usage, "invalid_import_request")
        }
        ImportExecutionError::Core(
            ImportCoreError::ZolcaUnsupported
            | ImportCoreError::UnknownFormat
            | ImportCoreError::AdapterUnavailable(_)
            | ImportCoreError::Detection(_)
            | ImportCoreError::Adapter(_),
        )
        | ImportExecutionError::SourceIssues { .. } => {
            (ExitClass::DataIssues, "import_source_invalid")
        }
        ImportExecutionError::Package(
            PackageWriteError::ReservedIdentifier { .. } | PackageWriteError::ProcessNoExchanges(_),
        ) => (ExitClass::DataIssues, "import_source_invalid"),
        ImportExecutionError::Package(PackageWriteError::FlowPreflight(_)) => {
            (ExitClass::DataIssues, "import_preflight_failed")
        }
        ImportExecutionError::Io(_)
        | ImportExecutionError::CommitRollback { .. }
        | ImportExecutionError::Core(ImportCoreError::Store(_))
        | ImportExecutionError::Mapping(_)
        | ImportExecutionError::Bundles(_) => (ExitClass::Io, "import_io_failed"),
        ImportExecutionError::Runtime(_)
        | ImportExecutionError::Core(
            ImportCoreError::Runtime(_)
            | ImportCoreError::Issue(_)
            | ImportCoreError::ZeroIssueLimit,
        )
        | ImportExecutionError::GeneratedPackageInvalid { .. }
        | ImportExecutionError::Json(_)
        | ImportExecutionError::Package(_)
        | ImportExecutionError::Conversion(_)
        | ImportExecutionError::Validation(_) => (ExitClass::Internal, "import_runtime_failed"),
    };
    OperationReportV1::failed(CommandNameV1::Import, exit_class, code, error.to_string())
}

fn runtime_error_in_chain<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a RuntimeError> {
    let mut current = Some(error);
    while let Some(candidate) = current {
        if let Some(runtime) = candidate.downcast_ref::<RuntimeError>() {
            return Some(runtime);
        }
        current = candidate.source();
    }
    None
}

fn conversion_report(arguments: &ConvertArgs, execution: &ExecutionContext) -> OperationReportV1 {
    if arguments.to == ConversionTarget::ReferenceUnit {
        return measurement::report(arguments, execution);
    }
    let Some(output) = arguments.output.as_ref() else {
        return OperationReportV1::failed(
            CommandNameV1::Convert,
            ExitClass::Usage,
            "missing_output",
            "--output is required for package conversion",
        );
    };
    let direction = match arguments.to {
        ConversionTarget::Ilcd => ConversionDirection::TidasToIlcd,
        ConversionTarget::Tidas => ConversionDirection::IlcdToTidas,
        ConversionTarget::ReferenceUnit => unreachable!("handled above"),
    };
    let request = ConversionRequest {
        input_dir: arguments.input.clone(),
        output_dir: output.clone(),
        direction,
        cancellation: execution.cancellation.clone(),
        memory_budget: execution.memory_budget.clone(),
        queue_capacity: execution.invocation.queue_capacity,
        progress: execution
            .invocation
            .progress_enabled
            .then(conversion_progress_reporter),
    };
    match convert_directory(&request) {
        Ok(summary) => {
            let mut report = OperationReportV1::succeeded(CommandNameV1::Convert);
            report.summary.insert(
                "conversion".to_owned(),
                serde_json::to_value(&summary).expect("conversion report contract is serializable"),
            );
            report.artifacts.push(ArtifactRefV1 {
                path: output.to_string_lossy().into_owned(),
                media_type: "application/vnd.tidas.package-directory".to_owned(),
                sha256: Some(summary.output_tree_sha256),
                bytes: Some(summary.output_bytes),
            });
            let input_format = match arguments.to {
                ConversionTarget::Ilcd => "ilcd-xml",
                ConversionTarget::Tidas => "tidas-json",
                ConversionTarget::ReferenceUnit => unreachable!("handled above"),
            };
            report.next_actions.push(format!(
                "tidas validate {} --input-format {input_format}",
                output.join("data").display()
            ));
            report
        }
        Err(ConversionError::Runtime(RuntimeError::Cancelled)) => {
            OperationReportV1::cancelled(CommandNameV1::Convert)
        }
        Err(error) => failed_conversion_report(&error),
    }
}

fn conversion_progress_reporter() -> ConversionProgressReporter {
    ConversionProgressReporter::new(|progress| {
        eprintln!(
            "tidas progress: convert phase={} direction={} files={} converted={} copied={} assets={}",
            progress.phase,
            progress.direction.as_str(),
            progress.files_processed,
            progress.converted_file_count,
            progress.copied_file_count,
            progress.asset_file_count,
        );
    })
}

fn failed_conversion_report(error: &ConversionError) -> OperationReportV1 {
    let (exit_class, code) = match error {
        ConversionError::InputNotDirectory(_)
        | ConversionError::OutputNotDirectory(_)
        | ConversionError::InvalidOutput(_)
        | ConversionError::SourceChanged(_)
        | ConversionError::CommitRollback { .. }
        | ConversionError::Io(_)
        | ConversionError::Walk(_) => (ExitClass::Io, "conversion_io_failed"),
        ConversionError::OutputInsideInput(_) | ConversionError::ZeroQueueCapacity => {
            (ExitClass::Usage, "invalid_conversion_request")
        }
        ConversionError::Runtime(RuntimeError::BudgetExceeded { .. }) => {
            (ExitClass::Internal, "memory_budget_exceeded")
        }
        ConversionError::Runtime(_) => (ExitClass::Internal, "conversion_runtime_failed"),
        ConversionError::PathOutsideInput(_)
        | ConversionError::NonPortablePath(_)
        | ConversionError::OrderingSchemaMissing(_)
        | ConversionError::OrderingSchemaReference(_)
        | ConversionError::OrderingSchemaCycle(_)
        | ConversionError::SizeOverflow
        | ConversionError::Asset(_) => (ExitClass::Internal, "conversion_setup_failed"),
        _ => (ExitClass::DataIssues, "conversion_input_invalid"),
    };
    OperationReportV1::failed(CommandNameV1::Convert, exit_class, code, error.to_string())
}

fn ruleset_report(arguments: &RulesetArgs) -> OperationReportV1 {
    let catalog = match RulesetCatalog::load() {
        Ok(catalog) => catalog,
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Ruleset,
                ExitClass::Internal,
                "ruleset_catalog_invalid",
                error.to_string(),
            );
        }
    };
    let mut report = OperationReportV1::succeeded(CommandNameV1::Ruleset);
    report.summary.insert(
        "ruleset_description".to_owned(),
        serde_json::to_value(catalog.description())
            .expect("ruleset description contract is serializable"),
    );
    report.summary.insert(
        "methodology_validation".to_owned(),
        serde_json::to_value(catalog.methodology_report())
            .expect("methodology validation contract is serializable"),
    );
    if let Some(id) = &arguments.id {
        match catalog.rules_for(id) {
            Ok(rules) => {
                report
                    .summary
                    .insert("ruleset_id".to_owned(), serde_json::json!(id));
                report.summary.insert(
                    "rules".to_owned(),
                    serde_json::to_value(rules).expect("packaged rules are serializable"),
                );
            }
            Err(RulesetError::UnknownRuleset(_)) => {
                return OperationReportV1::failed(
                    CommandNameV1::Ruleset,
                    ExitClass::Usage,
                    "unknown_ruleset",
                    error_message_for_unknown_ruleset(
                        id,
                        catalog.description().ruleset_ids.as_slice(),
                    ),
                );
            }
            Err(error) => {
                return OperationReportV1::failed(
                    CommandNameV1::Ruleset,
                    ExitClass::Internal,
                    "ruleset_resolution_failed",
                    error.to_string(),
                );
            }
        }
    } else {
        report
            .summary
            .insert("catalog".to_owned(), catalog.metadata().clone());
    }
    report
}

fn error_message_for_unknown_ruleset(id: &str, available: &[String]) -> String {
    format!(
        "Unknown ruleset id '{id}'. Available ids: {}",
        available.join(", ")
    )
}

fn validation_report(arguments: &ValidateArgs, execution: &ExecutionContext) -> OperationReportV1 {
    if arguments.describe {
        return validation_describe_report();
    }
    let input = arguments
        .input
        .clone()
        .expect("checked validate arguments require an input");
    let request = ValidationRequest {
        input_dir: input,
        issue_spool: arguments.issues.clone(),
        cancellation: execution.cancellation.clone(),
        memory_budget: execution.memory_budget.clone(),
        queue_capacity: execution.invocation.queue_capacity,
        progress: execution
            .invocation
            .progress_enabled
            .then(validation_progress_reporter),
    };
    if arguments.protocol == ValidationProtocol::DocumentValidationBatchV1 {
        return batch_validation_report(arguments, request);
    }
    if arguments.input_format == ValidationInputFormat::TidasJson {
        return full_tidas_validation_report(arguments, &request, execution);
    }
    let result = match arguments.input_format {
        ValidationInputFormat::TidasJson => unreachable!("TIDAS validation is handled above"),
        ValidationInputFormat::IlcdXml => validate_ilcd_package(&request),
    };
    match result {
        Ok(output) => completed_validation_report(output.summary, output.issue_spool_path),
        Err(ValidationError::Runtime(RuntimeError::Cancelled)) => {
            OperationReportV1::cancelled(CommandNameV1::Validate)
        }
        Err(error) => failed_validation_report(&error),
    }
}

fn full_tidas_validation_report(
    arguments: &ValidateArgs,
    request: &ValidationRequest,
    execution: &ExecutionContext,
) -> OperationReportV1 {
    let native = match validate_tidas_package(request) {
        Ok(output) => output,
        Err(ValidationError::Runtime(RuntimeError::Cancelled)) => {
            return OperationReportV1::cancelled(CommandNameV1::Validate);
        }
        Err(error) => return failed_validation_report(&error),
    };
    if !native.summary.ok || arguments.schema_only {
        let mut report = completed_validation_report(native.summary, native.issue_spool_path);
        if arguments.schema_only {
            report.diagnostics.push(DiagnosticV1::new(
                "schema_only_validation",
                "Native TIDAS schema and semantic checks passed; eILCD projection, target XSD, and recovery were not checked.",
            ));
            report.next_actions.push(
                "Run the same command without --schema-only for complete TIDAS-to-eILCD compatibility validation."
                    .to_owned(),
            );
        }
        return report;
    }

    let compatibility = match run_eilcd_compatibility(arguments, request, execution) {
        Ok(compatibility) => compatibility,
        Err(report) => return *report,
    };
    complete_tidas_validation_report(native, compatibility)
}

struct CompatibilityValidation {
    conversion: tidas_conversion::ConversionReportV1,
    projection: tidas_validation::ValidationOutput,
    roundtrip: Option<tidas_release::SemanticRoundtripReportV1>,
    issue_spool_path: Option<std::path::PathBuf>,
}

fn run_eilcd_compatibility(
    arguments: &ValidateArgs,
    request: &ValidationRequest,
    execution: &ExecutionContext,
) -> Result<CompatibilityValidation, Box<OperationReportV1>> {
    let workspace = match tempfile::tempdir() {
        Ok(workspace) => workspace,
        Err(error) => {
            return Err(Box::new(OperationReportV1::failed(
                CommandNameV1::Validate,
                ExitClass::Io,
                "validation_workspace_failed",
                error.to_string(),
            )));
        }
    };
    let ilcd = workspace.path().join("ilcd");
    let conversion = match convert_directory(&ConversionRequest {
        input_dir: request.input_dir.clone(),
        output_dir: ilcd.clone(),
        direction: ConversionDirection::TidasToIlcd,
        cancellation: execution.cancellation.clone(),
        memory_budget: execution.memory_budget.clone(),
        queue_capacity: execution.invocation.queue_capacity,
        progress: execution
            .invocation
            .progress_enabled
            .then(conversion_progress_reporter),
    }) {
        Ok(report) => report,
        Err(ConversionError::Runtime(RuntimeError::Cancelled)) => {
            return Err(Box::new(OperationReportV1::cancelled(
                CommandNameV1::Validate,
            )));
        }
        Err(error) => {
            return Err(Box::new(OperationReportV1::failed(
                CommandNameV1::Validate,
                ExitClass::DataIssues,
                "eilcd_projection_failed",
                error.to_string(),
            )));
        }
    };

    let projection_issues = arguments.issues.as_ref().map(|path| {
        let mut name = path.as_os_str().to_owned();
        name.push(".eilcd.jsonl");
        std::path::PathBuf::from(name)
    });
    let projection_validation = match validate_ilcd_package(&ValidationRequest {
        input_dir: ilcd.join("data"),
        issue_spool: projection_issues.clone(),
        cancellation: execution.cancellation.clone(),
        memory_budget: execution.memory_budget.clone(),
        queue_capacity: execution.invocation.queue_capacity,
        progress: execution
            .invocation
            .progress_enabled
            .then(validation_progress_reporter),
    }) {
        Ok(output) => output,
        Err(ValidationError::Runtime(RuntimeError::Cancelled)) => {
            return Err(Box::new(OperationReportV1::cancelled(
                CommandNameV1::Validate,
            )));
        }
        Err(error) => return Err(Box::new(failed_validation_report(&error))),
    };

    let roundtrip = if projection_validation.summary.ok {
        Some(run_semantic_roundtrip_compatibility(
            request, ilcd, execution,
        )?)
    } else {
        None
    };

    Ok(CompatibilityValidation {
        conversion,
        projection: projection_validation,
        roundtrip,
        issue_spool_path: projection_issues,
    })
}

fn run_semantic_roundtrip_compatibility(
    request: &ValidationRequest,
    ilcd_dir: std::path::PathBuf,
    execution: &ExecutionContext,
) -> Result<tidas_release::SemanticRoundtripReportV1, Box<OperationReportV1>> {
    match run_release(
        &ReleaseRequest::SemanticRoundtrip {
            tidas_dir: request.input_dir.clone(),
            ilcd_dir,
        },
        &ReleaseRuntime {
            cancellation: execution.cancellation.clone(),
            memory_budget: execution.memory_budget.clone(),
            queue_capacity: execution.invocation.queue_capacity,
        },
    ) {
        Ok(report) => report.roundtrip.ok_or_else(|| {
            Box::new(OperationReportV1::failed(
                CommandNameV1::Validate,
                ExitClass::Internal,
                "eilcd_recovery_report_missing",
                "The semantic round-trip action completed without a round-trip report.",
            ))
        }),
        Err(ReleaseError::Runtime(RuntimeError::Cancelled)) => Err(Box::new(
            OperationReportV1::cancelled(CommandNameV1::Validate),
        )),
        Err(error) => Err(Box::new(OperationReportV1::failed(
            CommandNameV1::Validate,
            ExitClass::DataIssues,
            "eilcd_recovery_failed",
            error.to_string(),
        ))),
    }
}

fn complete_tidas_validation_report(
    native: tidas_validation::ValidationOutput,
    compatibility: CompatibilityValidation,
) -> OperationReportV1 {
    let roundtrip_ok = compatibility
        .roundtrip
        .as_ref()
        .is_none_or(|report| report.ok);
    let projection_validation = compatibility.projection;
    let complete_ok = projection_validation.summary.ok && roundtrip_ok;
    let mut report = if complete_ok {
        OperationReportV1::succeeded(CommandNameV1::Validate)
    } else {
        OperationReportV1::completed_with_issues(
            CommandNameV1::Validate,
            DiagnosticV1::new(
                "eilcd_compatibility_issues",
                "Native TIDAS checks passed, but eILCD projection, target XSD validation, or recovery found issues.",
            ),
        )
    };
    report.summary.insert(
        "validation".to_owned(),
        serde_json::to_value(&native.summary).expect("validation summary is serializable"),
    );
    report.summary.insert(
        "eilcd_projection_validation".to_owned(),
        serde_json::to_value(&projection_validation.summary)
            .expect("validation summary is serializable"),
    );
    report.summary.insert(
        "eilcd_projection".to_owned(),
        serde_json::to_value(compatibility.conversion).expect("conversion report is serializable"),
    );
    if let Some(roundtrip) = compatibility.roundtrip {
        report.summary.insert(
            "semantic_roundtrip".to_owned(),
            serde_json::to_value(roundtrip).expect("roundtrip report is serializable"),
        );
    }
    if let (Some(path), Some(spool)) =
        (native.issue_spool_path, native.summary.issue_spool.as_ref())
    {
        report.artifacts.push(ArtifactRefV1 {
            path: path.to_string_lossy().into_owned(),
            media_type: "application/x-ndjson".to_owned(),
            sha256: Some(spool.sha256.clone()),
            bytes: Some(spool.bytes),
        });
    }
    if let (Some(path), Some(spool)) = (
        compatibility.issue_spool_path,
        projection_validation.summary.issue_spool.as_ref(),
    ) {
        report.artifacts.push(ArtifactRefV1 {
            path: path.to_string_lossy().into_owned(),
            media_type: "application/x-ndjson".to_owned(),
            sha256: Some(spool.sha256.clone()),
            bytes: Some(spool.bytes),
        });
    }
    report
}

fn validation_progress_reporter() -> ValidationProgressReporter {
    ValidationProgressReporter::new(|progress| {
        let total = progress
            .documents_total
            .map_or_else(|| "?".to_owned(), |value| value.to_string());
        let category = progress.category.as_deref().unwrap_or("-");
        eprintln!(
            "tidas progress: validate phase={} format={} category={} documents={}/{} issues={}",
            progress.phase,
            progress.input_format,
            category,
            progress.documents_processed,
            total,
            progress.issues_found,
        );
    })
}

fn validation_describe_report() -> OperationReportV1 {
    match asset_fingerprint() {
        Ok(fingerprint) => {
            let mut report = OperationReportV1::succeeded(CommandNameV1::Validate);
            report.summary.insert(
                "validation_describe".to_owned(),
                serde_json::to_value(match describe_document_validation(fingerprint) {
                    Ok(description) => description,
                    Err(error) => {
                        return OperationReportV1::failed(
                            CommandNameV1::Validate,
                            ExitClass::Internal,
                            "validation_describe_failed",
                            error.to_string(),
                        );
                    }
                })
                .expect("validation describe contract is serializable"),
            );
            report
        }
        Err(error) => OperationReportV1::failed(
            CommandNameV1::Validate,
            ExitClass::Internal,
            "validation_describe_failed",
            error.to_string(),
        ),
    }
}

fn batch_validation_report(
    arguments: &ValidateArgs,
    validation: ValidationRequest,
) -> OperationReportV1 {
    let request = BatchValidationRequest {
        validation,
        input_manifest: arguments
            .input_manifest
            .clone()
            .expect("checked batch arguments require a manifest"),
        event_spool: arguments.events.clone(),
        profile: DOCUMENT_VALIDATION_PROFILE.to_owned(),
    };
    match run_document_validation_batch(&request) {
        Ok(output) => completed_batch_validation_report(output),
        Err(ValidationError::Runtime(RuntimeError::Cancelled)) => {
            OperationReportV1::cancelled(CommandNameV1::Validate)
        }
        Err(error) => failed_validation_report(&error),
    }
}

fn completed_batch_validation_report(output: BatchValidationOutput) -> OperationReportV1 {
    let mut report = OperationReportV1::succeeded(CommandNameV1::Validate);
    report.summary.insert(
        "validation_batch_final".to_owned(),
        serde_json::to_value(output.final_event)
            .expect("validation final event contract is serializable"),
    );
    if let (Some(path), Some(spool)) = (output.event_spool_path, output.event_spool) {
        report.artifacts.push(ArtifactRefV1 {
            path: path.to_string_lossy().into_owned(),
            media_type: "application/x-ndjson".to_owned(),
            sha256: Some(spool.sha256),
            bytes: Some(spool.bytes),
        });
    }
    report
}

fn completed_validation_report(
    summary: ValidationSummaryV1,
    issue_spool_path: Option<std::path::PathBuf>,
) -> OperationReportV1 {
    let has_issues = !summary.ok;
    let issue_spool = summary.issue_spool.clone();
    let summary_value = match serde_json::to_value(summary) {
        Ok(value) => value,
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Validate,
                ExitClass::Internal,
                "validation_summary_serialization_failed",
                error.to_string(),
            );
        }
    };
    let mut report = if has_issues {
        OperationReportV1::completed_with_issues(
            CommandNameV1::Validate,
            DiagnosticV1::new(
                "validation_issues",
                "Validation completed and found data issues.",
            ),
        )
    } else {
        OperationReportV1::succeeded(CommandNameV1::Validate)
    };
    report
        .summary
        .insert("validation".to_owned(), summary_value);
    if let (Some(path), Some(spool)) = (issue_spool_path, issue_spool) {
        report.artifacts.push(ArtifactRefV1 {
            path: path.to_string_lossy().into_owned(),
            media_type: "application/x-ndjson".to_owned(),
            sha256: Some(spool.sha256),
            bytes: Some(spool.bytes),
        });
    }
    report
}

fn failed_validation_report(error: &ValidationError) -> OperationReportV1 {
    let (exit_class, code) = match &error {
        ValidationError::InputNotDirectory(_)
        | ValidationError::SpoolParentMissing(_)
        | ValidationError::PersistSpool { .. }
        | ValidationError::Io(_) => (ExitClass::Io, "validation_io_failed"),
        ValidationError::BatchProtocol(_) => (ExitClass::DataIssues, "batch_protocol_failed"),
        ValidationError::Runtime(RuntimeError::BudgetExceeded { .. }) => {
            (ExitClass::Internal, "memory_budget_exceeded")
        }
        ValidationError::Runtime(_) => (ExitClass::Internal, "validation_runtime_failed"),
        ValidationError::ZeroQueueCapacity => (ExitClass::Usage, "invalid_queue_capacity"),
        ValidationError::PathOutsideInput(_)
        | ValidationError::SizeOverflow
        | ValidationError::UnexpectedXsdAsset(_)
        | ValidationError::Walk(_)
        | ValidationError::InvalidXml(_)
        | ValidationError::Json(_)
        | ValidationError::Asset(_)
        | ValidationError::Schema(_)
        | ValidationError::Semantic(_)
        | ValidationError::Ruleset(_)
        | ValidationError::Xml(_) => (ExitClass::Internal, "validation_setup_failed"),
    };
    OperationReportV1::failed(CommandNameV1::Validate, exit_class, code, error.to_string())
}

fn version_report(execution: &ExecutionContext) -> OperationReportV1 {
    let mut report = OperationReportV1::succeeded(CommandNameV1::Version);
    report.summary.insert(
        "binary_version".to_owned(),
        serde_json::json!(env!("CARGO_PKG_VERSION")),
    );
    report.summary.insert(
        "operation_report_schema".to_owned(),
        serde_json::json!(OPERATION_REPORT_SCHEMA_V1),
    );
    report.summary.insert(
        "invocation_context_schema".to_owned(),
        serde_json::json!(INVOCATION_CONTEXT_SCHEMA_V1),
    );
    report.summary.insert(
        "release_report_schema".to_owned(),
        serde_json::json!(RELEASE_REPORT_SCHEMA_V1),
    );
    report.summary.insert(
        "flow_property_conversion_schema".to_owned(),
        serde_json::json!(tidas_measurement::REPORT_SCHEMA),
    );
    report.summary.insert(
        "flow_property_conversion_request_schema".to_owned(),
        serde_json::json!(tidas_measurement::REQUEST_SCHEMA),
    );
    report
        .summary
        .insert("command_count".to_owned(), serde_json::json!(7));
    report.summary.insert(
        "memory_budget_bytes".to_owned(),
        serde_json::json!(execution.memory_budget.limit()),
    );
    report.summary.insert(
        "asset_count".to_owned(),
        serde_json::json!(bundled_assets().len()),
    );
    match asset_fingerprint() {
        Ok(fingerprint) => {
            report.summary.insert(
                "asset_fingerprint".to_owned(),
                serde_json::json!(fingerprint),
            );
        }
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Version,
                ExitClass::Internal,
                "asset_fingerprint_failed",
                error.to_string(),
            );
        }
    }
    match serde_json::to_value(engine_decision()) {
        Ok(decision) => {
            report.summary.insert("xml_engine".to_owned(), decision);
        }
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Version,
                ExitClass::Internal,
                "xml_engine_contract_failed",
                error.to_string(),
            );
        }
    }
    match RulesetCatalog::load() {
        Ok(catalog) => {
            report.summary.insert(
                "ruleset_catalog".to_owned(),
                serde_json::to_value(catalog.description())
                    .expect("ruleset description contract is serializable"),
            );
        }
        Err(error) => {
            return OperationReportV1::failed(
                CommandNameV1::Version,
                ExitClass::Internal,
                "ruleset_fingerprint_failed",
                error.to_string(),
            );
        }
    }
    report.next_actions.push(
        "Run `tidas --help` to inspect the stable command tree; functional slices are tracked under tidas-tools#117."
            .to_owned(),
    );
    report
}
