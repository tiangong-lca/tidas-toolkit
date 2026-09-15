use std::io::Read;

use tidas_contracts::{CommandNameV1, ExitClass, OperationReportV1};
use tidas_measurement::convert_measurement;

use crate::args::ConvertArgs;
use crate::context::ExecutionContext;

const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn report(arguments: &ConvertArgs, execution: &ExecutionContext) -> OperationReportV1 {
    if arguments.output.is_some() {
        return failed(
            ExitClass::Usage,
            "unexpected_output",
            "reference-unit conversion is report-only; use global --report for atomic report output",
        );
    }
    let input: Box<dyn Read> = if arguments.input.as_os_str() == "-" {
        Box::new(std::io::stdin())
    } else {
        match std::fs::symlink_metadata(&arguments.input) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return failed(
                    ExitClass::DataIssues,
                    "symlink_input",
                    "request input must not be a symlink",
                );
            }
            Err(error) => return failed(ExitClass::Io, "measurement_input_io", error.to_string()),
            _ => {}
        }
        match std::fs::File::open(&arguments.input) {
            Ok(file) => Box::new(file),
            Err(error) => return failed(ExitClass::Io, "measurement_input_io", error.to_string()),
        }
    };
    read_and_convert(input, execution)
}

fn read_and_convert(mut input: Box<dyn Read>, execution: &ExecutionContext) -> OperationReportV1 {
    let mut bytes = Vec::new();
    let mut reservations = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        if execution.cancellation.is_cancelled() {
            return OperationReportV1::cancelled(CommandNameV1::Convert);
        }
        let count = match input.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => return failed(ExitClass::Io, "measurement_input_io", error.to_string()),
        };
        if bytes.len() + count > MAX_REQUEST_BYTES {
            return failed(
                ExitClass::DataIssues,
                "measurement_request_too_large",
                "request exceeds 16 MiB",
            );
        }
        match execution.memory_budget.reserve(count as u64 * 16) {
            Ok(reservation) => reservations.push(reservation),
            Err(error) => {
                return failed(
                    ExitClass::Internal,
                    "measurement_memory_budget",
                    error.to_string(),
                );
            }
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => {
            return failed(
                ExitClass::DataIssues,
                "invalid_measurement_json",
                error.to_string(),
            );
        }
    };
    match convert_measurement(&value) {
        Ok(conversion) => {
            let mut report = OperationReportV1::succeeded(CommandNameV1::Convert);
            report.summary.insert(
                "flow_property_conversion".to_owned(),
                serde_json::to_value(conversion).expect("measurement report serializes"),
            );
            report
        }
        Err(error) => failed(ExitClass::DataIssues, error.code, error.message),
    }
}

fn failed(class: ExitClass, code: &str, message: impl Into<String>) -> OperationReportV1 {
    OperationReportV1::failed(CommandNameV1::Convert, class, code, message.into())
}
