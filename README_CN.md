---
title: tidas-tools README CN
docType: guide
scope: repo
status: active
authoritative: false
owner: tidas-tools
language: zh-CN
whenToUse:
  - when you need Chinese user-facing CLI examples or basic development commands
whenToUpdate:
  - when Chinese CLI examples, development commands, or release notes change
checkPaths:
  - README_CN.md
  - AGENTS.md
  - .docpact/config.yaml
  - docs/agents/**
  - Cargo.toml
  - crates/**
  - contracts/**
  - assets/**
  - packaging/**
  - migration/**
  - .github/workflows/**
  - scripts/install.*
  - scripts/publish-crates.sh
  - scripts/test-release-request.sh
  - scripts/validate-release-request.sh
  - scripts/sync-rust-package-assets.sh
lastReviewedAt: 2026-09-13
lastReviewedCommit: e237a6cf1d4c55ba490719a8fcee0f659d1f840c
lastReviewedNote: "Reviewed for toolkit #189: canonical dispatch, installers and package metadata use tidas-toolkit/tidas-sdks. The v1 notice reader preserves the historical namespace through 0.3.0 without changing provenance authority, asset locks, release requests or CLI behavior."
related:
  - AGENTS.md
  - .docpact/config.yaml
  - docs/agents/repo-validation.md
  - docs/agents/repo-architecture.md
  - README.md
---

# TianGong TIDAS Tools 使用说明

[![crates.io](https://img.shields.io/crates/v/tidas.svg)][crates.io]
[![GitHub Release](https://img.shields.io/github/v/release/tiangong-lca/tidas-toolkit)][releases]

[crates.io]: https://crates.io/crates/tidas
[releases]: https://github.com/tiangong-lca/tidas-toolkit/releases

[English](https://github.com/tiangong-lca/tidas-toolkit/blob/main/README.md) | [中文](https://github.com/tiangong-lca/tidas-toolkit/blob/main/README_CN.md)

本仓库通过唯一的跨平台 Rust 可执行文件 `tidas` 提供 TIDAS 转换、导入、导出、
校验、发布和 ruleset 能力。

## 统一 Rust CLI

Cargo workspace 提供稳定机器与 invocation 契约、有界运行时
基础设施、可执行资产完整性锁、XML/XSD/XSLT 跨平台边界、最终统一 CLI 适配层，
以及原生 TIDAS/ILCD 校验、引用提取、batch 证据、ruleset 检查、双向
TIDAS/eILCD 转换、外部格式导入、数据库导出、确定性 release control 与可复现
原生分发：

```bash
cargo build --workspace
cargo build --release -p tidas --bin tidas
cargo run -p tidas --bin tidas -- --help
cargo run -p tidas --bin tidas -- --format json version
cargo run -p tidas --bin tidas -- convert <TIDAS数据包目录> \
  --output <eILCD数据包目录> --to ilcd --format json
cargo run -p tidas --bin tidas -- convert <eILCD数据目录> \
  --output <TIDAS数据包目录> --to tidas --format json
cargo run -p tidas --bin tidas -- import <源文件或目录> \
  --output <导入输出目录> --target both --write-mapping --format json
cargo run -p tidas --bin tidas -- export \
  --output <数据包.zip> --skip-external-docs --format json
cargo run -p tidas --bin tidas -- validate <数据包目录> \
  --issues <issues.jsonl> --format json
cargo run -p tidas --bin tidas -- validate <ILCD目录> \
  --input-format ilcd-xml --issues <issues.jsonl> --format json
cargo run -p tidas --bin tidas -- release build-packages \
  --tidas-dir <canonical-TIDAS目录> \
  --dataset-index <canonical-dataset-index.json> \
  --output-dir <release目录> --format json
cargo run -p tidas --bin tidas -- ruleset --format json
cargo run -p tidas --bin tidas -- --completion bash > tidas.bash
cargo run -p tidas-assets --bin tidas-asset-lock -- check
cargo run -p tidas-dist -- version
```

从源码构建 release executable 时，使用带 package 限定的命令
`cargo build --release -p tidas --bin tidas`；产物位于 `target/release/tidas`
（Windows 为 `target/release/tidas.exe`）。

命令树固定为 `convert`、`import`、`export`、`validate`、`release`、
`ruleset` 和 `version`。七个命令均由 Rust 实现，且不依赖第二运行时。

原生导入支持 EcoSpold 1/2、SimaPro CSV、openLCA JSON-LD、openLCA process
XLSX 与 ILCD 文件、目录或 ZIP 包。默认通过有界签名检测源格式，也可使用
`--from-format` 处理歧义输入。命令始终在内部写出并校验 TIDAS，可通过
`--target ilcd|both` 发布 ILCD，默认写出每个 process 的依赖 bundle，并通过
`--write-mapping` 启用确定性的 `mapping.csv.gz`。`.zolca` 会被明确拒绝。
解析、exchange 与 issue 报告均使用可取消、有内存预算且落盘的有界流；失败时不会
发布部分输出。

原生转换把输入镜像到 `OUTPUT/data`，保留数据包元数据，写入经过完整性锁验证的
目标 schemas/stylesheets/methodologies，并原子发布整个输出目录。含顶层扩展元数据
的 TIDAS 文档使用确定性的 `.tidas-envelope.json` sidecar，使 eILCD XML 保持单根，
反向转换时再无损恢复原包络。遍历拒绝符号链接和 XML 1.0 非法字符；相同输入的成功
运行会返回相同的输出树 SHA-256。

原生导出在单个 repeatable-read、只读 PostgreSQL 快照中读取 active records，
通过有界队列流式处理，归一化 TIDAS 数据包版本，可选流式下载 S3-compatible
附加文档，并原子发布一个确定性 ZIP。数据库使用 `TIDAS_DATABASE_URL`；对象存储
凭据只接受 `TIDAS_S3_ACCESS_KEY_ID`、`TIDAS_S3_SECRET_ACCESS_KEY` 与可选的
`TIDAS_S3_SESSION_TOKEN`，其值不会进入报告或诊断。

原生校验只解析内嵌且经过完整性锁验证的 schemas。通过 `--issues` 可把全部问题
按确定顺序原子写入 JSONL；operation report 只保留有界计数与 spool hash，不在
内存中累计问题数组。ILCD XML 使用离线复用的 XSD context 和相同有界报告契约；
`document-validation-batch.v1` 提供 manifest 预检、漂移防护 issue 事件和确定性
final evidence hash。
校验进度按有界频率只写入 stderr；非交互运行可使用 `--progress always`。

全局运行参数遵循“命令行 > `TIDAS_*` 环境变量 > 内置默认值”的优先级，不会隐式
读取当前目录中的配置文件。stdout 只包含一次 human/JSON 报告或 completion 脚本；
日志、进度、诊断与报告落盘确认写入 stderr。使用 `--report <PATH>` 可在不占用
stdout 的情况下持久化报告。默认计入内存预算为 512 MiB，有界队列容量为 256。
规范契约见 [docs/agents/cli-contract.md](docs/agents/cli-contract.md)。

## 原生分发

预构建的 GitHub 归档是面向最终用户的主要渠道；其中已包含原生 XML 依赖，运行时
不需要 Rust 或开发工具链。
原生 release workflow 从同一个精确的 `tidas` binary 构建并验证 Linux
x86_64/ARM64、macOS Apple Silicon 与 Windows x86_64 制品。每个平台均
重复构建归档并逐字节比较，验证 SHA-256，执行打包后的 `version`、help、JSON
`version` 与 `ruleset` 探针，并生成 SPDX SBOM 和 GitHub OIDC
provenance/SBOM attestation。固定版本且静态链接的 libxml2/libxslt 使运行时
不依赖 Homebrew、vcpkg、Java、Node.js 或开发工具链。

同一个 `v<version>` release 会先把 `tidas` 源码包与全部可复用领域 crates
发布到 crates.io，再创建不可变的 GitHub Release。已经安装 Rust 1.98.1+ 以及平台
libxml2/libxslt 开发依赖的开发者，也可从源码安装唯一的统一 executable：

```bash
cargo install tidas --version 0.3.0 --locked
```

全部公开 workspace crates 使用完全相同的精确版本，避免 Cargo 混用不兼容的领域
release；`tidas-dist` 始终是仓库内部 release tooling，不会发布。Pull request 会在
无凭据条件下执行完整 multi-package `cargo package` 验证与 crates.io dry-run；
只有 tag context 下的 release workflow 能读取 `CARGO_REGISTRY_TOKEN`。

原生版本通过评审并合并一个只追加的
`.github/releases/v<version>.json` 来授权；该请求把版本绑定到完整 commit SHA。
Pull request 阶段只有只读、无 secret 的校验。合并 job 创建或验证精确 lightweight
tag，再显式从该 tag dispatch 原生 release workflow，使 artifact provenance
绑定到实际发布的源码 commit，而不是请求 PR 的 merge commit。

原生版本发布后，可安装明确且不可变的版本：

```bash
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://raw.githubusercontent.com/tiangong-lca/tidas-toolkit/main/scripts/install.sh
sh install.sh --version 0.2.0 --prefix "$HOME/.local"
```

```powershell
.\scripts\install.ps1 -Version 0.2.0
```

每个 GitHub Release 同时携带由相同归档哈希生成的 Homebrew formula 与 Winget
manifests。创建外部 tap 或提交 Winget community 需要单独批准，且这些路径绝不
重新构建 executable。Homebrew formula 在 macOS 上选择 Apple Silicon 归档，
POSIX 安装器会在任何下载之前拒绝 macOS Intel。macOS Intel 与 Windows ARM64
不受支持。

## 开发

需要 Rust 1.98.1 或更新版本以及平台对应的 libxml2/libxslt 开发包。运行：

```bash
scripts/audit-rust-only.sh
cargo run --locked -p tidas-assets --bin tidas-asset-lock -- check
cargo run --locked -p tidas-assets --bin tidas-asset-lock -- spec-check \
  --archive <QUALIFIED_CANDIDATE.tgz>
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
```

有意修改 schema 或可执行资产后，使用 `cargo run -p tidas-assets --bin tidas-asset-lock -- write` 依次更新两类锁，评审全部差异后重新运行门禁。领域与大包验证见 [验证指南](docs/agents/repo-validation.md)。

36 个公共 schema、2 份共享 methodology 文档以及配套 `schema.lock.json` 是 `tiangong-lca/tidas-spec` 发布合格公共规范的受控副本，由 `crates/tidas-assets/src/spec_pin.rs` 中的归档与 manifest SHA-256 精确固定到唯一不可变候选；候选 manifest 与导入溯源记录保存在 `assets/spec/`。不要手工修改公共 schema 或 methodology：使用 `tidas-asset-lock spec-import --archive <QUALIFIED_CANDIDATE>` 重新生成，并用 `tidas-asset-lock spec-check` 验证；CI 会用取自精确源提交的候选执行该检查。工具自有的 methodology 输入（`runtime_rulesets.json`、`runtime_rulesets.schema.json` 与 elementary taxonomy extension）、eILCD schema/style、验证索引仍归本仓库所有，不会被该导入替换。

Rust-only cutover 之前的实现只作为 Git 历史以及 `migration/final-python-line.json` 声明的不可变 tag 保留，不再是安装、执行、CI 或发布路径。

## 参与贡献

实施前先创建 GitHub Issue，并遵循 [AGENTS.md](AGENTS.md)、本仓库 Docpact route 与 workspace delivery workflow。PR 需记录聚焦验证，以及必要的下游 SDK 或 root workspace integration。
