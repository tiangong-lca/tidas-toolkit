param(
    [string]$OutputDir = "dist/windows-public-proof"
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$version = "0.3.3"
$tag = "v$version"
$sourceSha = "4f1be37fa3a9c9e939321e0d40776a63a2e005c8"
$repository = "tiangong-lca/tidas-toolkit"
$archiveName = "tidas-$tag-x86_64-pc-windows-msvc.zip"
$expectedArchiveSha = "57faa60519777a2b179bcfe4cc57325b548e696e9ba3c7b1f39893350db81550"
$fixturePath = Join-Path $PSScriptRoot "../crates/tidas-conversion/tests/fixtures/process-types-v1/synthetic-process.json"
$fixtureSha = "637d88ecff0749f1724c1d2db99ab5a020113bc41ab18bc7c5f1d2d19afc00d3"
$expectedIssueCount = 400
$output = [System.IO.Path]::GetFullPath($OutputDir)
$headers = @{
    "User-Agent" = "Toolkit-public-Windows-archive-verifier"
    "Accept" = "application/vnd.github+json"
}

function Assert-True([bool]$condition, [string]$message) {
    if (-not $condition) { throw $message }
}

function Get-Sha256([byte[]]$bytes) {
    return [System.Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($bytes)
    ).ToLowerInvariant()
}

function Write-Utf8([string]$path, [string]$content) {
    [System.IO.File]::WriteAllText(
        $path, $content, [System.Text.UTF8Encoding]::new($false)
    )
}

function Invoke-Tidas([string]$binary, [string[]]$arguments, [string]$label) {
    $stderrPath = Join-Path $output "$label.stderr.txt"
    $stdoutLines = @(& $binary @arguments 2> $stderrPath)
    $exitCode = $LASTEXITCODE
    $raw = $stdoutLines -join [Environment]::NewLine
    Write-Utf8 (Join-Path $output "$label.report.json") ($raw + [Environment]::NewLine)
    Assert-True (-not [string]::IsNullOrWhiteSpace($raw)) "$label returned no JSON report"
    $report = ConvertFrom-Json -InputObject $raw -AsHashtable
    return @{ exit_code = $exitCode; report = $report }
}

New-Item -ItemType Directory -Force -Path $output | Out-Null
$tagRef = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/git/ref/tags/$tag" -Headers $headers
Assert-True ($tagRef.object.type -eq "commit") "release tag is not a lightweight commit ref"
Assert-True ($tagRef.object.sha -eq $sourceSha) "release tag does not bind the qualified source"
$release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repository/releases/tags/$tag" -Headers $headers
Assert-True ($release.tag_name -eq $tag) "unexpected public release tag"
$archiveAsset = @($release.assets | Where-Object { $_.name -eq $archiveName })
$sidecarAsset = @($release.assets | Where-Object { $_.name -eq "$archiveName.sha256" })
Assert-True ($archiveAsset.Count -eq 1 -and $sidecarAsset.Count -eq 1) "public Windows archive/sidecar missing or ambiguous"
$archivePath = Join-Path $output $archiveName
$sidecarPath = Join-Path $output "$archiveName.sha256"
Invoke-WebRequest -Uri $archiveAsset[0].browser_download_url -OutFile $archivePath -Headers $headers
Invoke-WebRequest -Uri $sidecarAsset[0].browser_download_url -OutFile $sidecarPath -Headers $headers
$archiveBytes = [System.IO.File]::ReadAllBytes($archivePath)
$archiveSha = Get-Sha256 $archiveBytes
$sidecarBytes = [System.IO.File]::ReadAllBytes($sidecarPath)
$sidecarSha = Get-Sha256 $sidecarBytes
Assert-True ($archiveAsset[0].digest -eq "sha256:$archiveSha") "archive differs from GitHub asset digest"
Assert-True ($archiveSha -eq $expectedArchiveSha) "archive differs from independently recorded immutable v0.3.3 digest"
Assert-True ($sidecarAsset[0].digest -eq "sha256:$sidecarSha") "sidecar differs from GitHub asset digest"
Assert-True ($archiveAsset[0].size -eq $archiveBytes.Length) "archive size differs from GitHub asset"
$sidecarLines = [System.IO.File]::ReadAllLines($sidecarPath)
Assert-True ($sidecarLines.Count -eq 1) "archive sidecar line count differs"
$parts = $sidecarLines[0].Trim() -split "\s+", 2
Assert-True ($parts.Count -eq 2 -and $parts[0] -eq $archiveSha -and $parts[1].TrimStart("*") -eq $archiveName) "archive sidecar checksum/name mismatch"

$extractDir = Join-Path $output "extracted"
Expand-Archive -LiteralPath $archivePath -DestinationPath $extractDir -Force
$binary = Join-Path $extractDir "tidas-$tag-x86_64-pc-windows-msvc/bin/tidas.exe"
Assert-True (Test-Path -LiteralPath $binary -PathType Leaf) "verified archive lacks tidas.exe"
$binaryVersion = (& $binary --version) -join [Environment]::NewLine
Assert-True ($LASTEXITCODE -eq 0 -and $binaryVersion -match "0\.3\.3") "archive executable has wrong version"
$actualFixtureSha = Get-Sha256 ([System.IO.File]::ReadAllBytes($fixturePath))
Assert-True ($actualFixtureSha -eq $fixtureSha) "Toolkit-owned synthetic fixture bytes drifted"

$inputDir = Join-Path $output "input"
$processDir = Join-Path $inputDir "processes"
New-Item -ItemType Directory -Force -Path $processDir | Out-Null
$fixture = Get-Content -LiteralPath $fixturePath -Raw | ConvertFrom-Json -AsHashtable
$prototype = $fixture.processDataSet.exchanges.exchange[0]
$rows = @(
    for ($index = 0; $index -lt $expectedIssueCount; $index++) {
        $row = ConvertFrom-Json -InputObject (ConvertTo-Json -InputObject $prototype -Depth 40 -Compress) -AsHashtable
        $row["@dataSetInternalID"] = [string]$index
        $row["meanAmount"] = @{ invalid = $index }
        $row
    }
)
$fixture.processDataSet.exchanges.exchange = $rows
$inputPath = Join-Path $processDir "synthetic.json"
Write-Utf8 $inputPath ((ConvertTo-Json -InputObject $fixture -Depth 60 -Compress) + [Environment]::NewLine)
$inputSha = Get-Sha256 ([System.IO.File]::ReadAllBytes($inputPath))

$shortDir = Join-Path $output "short"
New-Item -ItemType Directory -Force -Path $shortDir | Out-Null
$deepDir = $output
foreach ($part in @(
    "项目 workspace", ".foundry", "workspaces", ("task-" + ("a" * 64)),
    "outputs", "assessment", ("b" * 64), ("run-" + ("c" * 40)),
    "process", (".tidas-validate-stage-" + ("d" * 36))
)) {
    $deepDir = Join-Path $deepDir $part
}
Assert-True ($deepDir.Length -gt 324) "deep caller path is too short to cover Windows long-path regression"
$extendedDeepDir = "\\?\" + $deepDir
[System.IO.Directory]::CreateDirectory($extendedDeepDir) | Out-Null
$shortIssues = Join-Path $shortDir "validation-events.jsonl"
$deepIssues = Join-Path $deepDir "validation-events.jsonl"
$argsBase = @("validate", $inputDir, "--input-format", "tidas-json", "--schema-only", "--issues")
$argsEnd = @("--format", "json", "--progress", "never")
$short = Invoke-Tidas $binary ($argsBase + @($shortIssues) + $argsEnd) "short"
$deep = Invoke-Tidas $binary ($argsBase + @($deepIssues) + $argsEnd) "deep"
foreach ($run in @($short, $deep)) {
    Assert-True ($run.exit_code -eq 2) "short/deep native exit code is not data-issues"
    Assert-True ($run.report.exit_class -eq "data-issues" -and $run.report.status -eq "completed-with-issues") "short/deep native class/status differs"
    Assert-True ($run.report.summary.validation.error_count -eq $expectedIssueCount) "short/deep Process issue count differs"
    Assert-True ($run.report.summary.validation.issue_spool.event_count -eq $expectedIssueCount) "short/deep spool event count differs"
}
$shortBytes = [System.IO.File]::ReadAllBytes($shortIssues)
$deepBytes = [System.IO.File]::ReadAllBytes("\\?\" + $deepIssues)
$shortSha = Get-Sha256 $shortBytes
$deepSha = Get-Sha256 $deepBytes
Assert-True ($shortSha -eq $deepSha -and $shortBytes.Length -eq $deepBytes.Length) "short/deep issue spool hashes/lengths differ"
Assert-True ([System.Convert]::ToBase64String($shortBytes) -ceq [System.Convert]::ToBase64String($deepBytes)) "short/deep issue spool bytes differ"
[System.IO.File]::WriteAllBytes((Join-Path $output "deep-validation-events.jsonl"), $deepBytes)
Assert-True ($short.report.summary.validation.issue_spool.sha256 -eq $shortSha) "short report spool hash differs"
Assert-True ($deep.report.summary.validation.issue_spool.sha256 -eq $deepSha) "deep report spool hash differs"
Assert-True ($deep.report.artifacts[0].path -eq $deepIssues) "deep report lost caller-supplied path"
$events = @([System.IO.File]::ReadAllLines($shortIssues) | ForEach-Object { ConvertFrom-Json -InputObject $_ -AsHashtable })
Assert-True ($events.Count -eq $expectedIssueCount) "synthetic Process event line count differs"
for ($index = 0; $index -lt $expectedIssueCount; $index++) {
    Assert-True ($events[$index].issue_ordinal -eq $index) "issue ordinal differs at index $index"
    Assert-True ($events[$index].issue.location -eq "processDataSet/exchanges/exchange/$index/meanAmount") "Process exchange location differs at index $index"
    Assert-True ($events[$index].issue.issue_code -eq "schema_error") "Process row was not reported as schema_error at index $index"
}

$manifestPath = Join-Path $output "manifest.jsonl"
$manifest = [ordered]@{
    document_key = "process:synthetic:01.00.000"
    category = "processes"
    relative_path = "processes/synthetic.json"
    content_sha256 = $inputSha
}
Write-Utf8 $manifestPath ((ConvertTo-Json -InputObject $manifest -Compress) + [Environment]::NewLine)
$shortEvents = Join-Path $shortDir "batch-events.jsonl"
$deepEvents = Join-Path $deepDir "batch-events.jsonl"
$batchArgsBase = @("validate", $inputDir, "--protocol", "document-validation-batch.v1", "--input-manifest", $manifestPath, "--events")
$shortBatch = Invoke-Tidas $binary ($batchArgsBase + @($shortEvents) + $argsEnd) "short-batch"
$deepBatch = Invoke-Tidas $binary ($batchArgsBase + @($deepEvents) + $argsEnd) "deep-batch"
foreach ($run in @($shortBatch, $deepBatch)) {
    Assert-True ($run.exit_code -eq 0 -and $run.report.exit_class -eq "success" -and $run.report.status -eq "succeeded") "short/deep batch native class/status differs"
    Assert-True ($run.report.summary.validation_batch_final.summary.issue_count -eq $expectedIssueCount) "short/deep batch issue count differs"
}
$shortBatchBytes = [System.IO.File]::ReadAllBytes($shortEvents)
$deepBatchBytes = [System.IO.File]::ReadAllBytes("\\?\" + $deepEvents)
Assert-True ([System.Convert]::ToBase64String($shortBatchBytes) -ceq [System.Convert]::ToBase64String($deepBatchBytes)) "short/deep batch event bytes differ"
[System.IO.File]::WriteAllBytes((Join-Path $output "deep-batch-events.jsonl"), $deepBatchBytes)
$batchSha = Get-Sha256 $shortBatchBytes
Assert-True ($shortBatch.report.summary.validation_batch_final.logical_issue_stream_sha256 -eq $deepBatch.report.summary.validation_batch_final.logical_issue_stream_sha256) "short/deep logical issue hashes differ"
Assert-True ($deepBatch.report.artifacts[0].path -eq $deepEvents) "deep batch report lost caller-supplied path"
$batchLines = @([System.IO.File]::ReadAllLines($shortEvents) | ForEach-Object { ConvertFrom-Json -InputObject $_ -AsHashtable })
Assert-True ($batchLines.Count -eq ($expectedIssueCount + 1)) "batch issue/final event count differs"
Assert-True ($batchLines[0].type -eq "issue" -and $batchLines[-1].type -eq "final" -and $batchLines[-1].completed) "batch final event is missing"

$blockedParent = Join-Path $output "blocked-parent"
Write-Utf8 $blockedParent ("this is a file, not an issue-spool directory" + [Environment]::NewLine)
$blockedIssues = Join-Path $blockedParent "events.jsonl"
$io = Invoke-Tidas $binary ($argsBase + @($blockedIssues) + $argsEnd) "io-failure"
Assert-True ($io.exit_code -eq 74 -and $io.report.exit_class -eq "io" -and $io.report.status -eq "failed") "real issue-spool I/O failure was misclassified"
$ioDiagnostic = @($io.report.diagnostics | Where-Object { $_.code -eq "validation_io_failed" })
Assert-True ($ioDiagnostic.Count -eq 1) "native validation_io_failed diagnostic was lost"
Assert-True ($ioDiagnostic[0].message.Contains($blockedParent)) "I/O diagnostic lost original destination path"
Assert-True (-not (Test-Path -LiteralPath $blockedIssues)) "failed I/O published an issue spool"

$proof = [ordered]@{
    schema = "tidas.published-windows-archive-proof.v1"
    version = $version
    tag = $tag
    source_sha = $sourceSha
    archive = $archiveName
    archive_sha256 = $archiveSha
    archive_bytes = $archiveBytes.Length
    sidecar_sha256 = $sidecarSha
    installed_binary_version = $binaryVersion
    synthetic_fixture_sha256 = $fixtureSha
    generated_process_sha256 = $inputSha
    process_exchange_count = $expectedIssueCount
    deep_path_utf16_units = $deepDir.Length
    short_exit_code = $short.exit_code
    deep_exit_code = $deep.exit_code
    short_exit_class = $short.report.exit_class
    deep_exit_class = $deep.report.exit_class
    issue_spool_sha256 = $shortSha
    issue_spool_bytes = $shortBytes.Length
    issue_spool_identical = $true
    first_issue_ordinal = $events[0].issue_ordinal
    last_issue_ordinal = $events[-1].issue_ordinal
    batch_event_count = $batchLines.Count
    batch_event_sha256 = $batchSha
    batch_event_identical = $true
    batch_logical_issue_sha256 = $shortBatch.report.summary.validation_batch_final.logical_issue_stream_sha256
    io_exit_code = $io.exit_code
    io_exit_class = $io.report.exit_class
    io_diagnostic_code = $ioDiagnostic[0].code
    io_diagnostic_message = $ioDiagnostic[0].message
    technical_fixture_only = $true
}
$proofPath = Join-Path $output "proof.json"
Write-Utf8 $proofPath ((ConvertTo-Json -InputObject $proof -Depth 20) + [Environment]::NewLine)
Write-Output "Verified published Windows archive $archiveName at $archiveSha; $expectedIssueCount indexed rows, short/deep spool bytes equal, real I/O exit 74 preserved."
