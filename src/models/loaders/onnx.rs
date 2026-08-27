use anyhow::{bail, Context, Result};
use ndarray;

/// Token streaming callback (receives token id + decoded string).
type TokenCallback = Box<dyn FnMut(u32, &str) + Send>;

/// Return type for a single forward pass: logits + updated KV cache.
type ForwardOutput = (Vec<f32>, Vec<(DynValue, DynValue)>);
use ort::{
    ep,
    memory::MemoryInfo,
    session::{builder::GraphOptimizationLevel, Session, SessionOutputs},
    value::{DynValue, Value},
};
#[cfg(target_os = "macos")]
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
#[cfg(target_os = "macos")]
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    process::{Child, Command, Stdio},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

use super::onnx_config::{ExecutionProvider as ConfigExecutionProvider, ModelSize, OnnxLoadConfig};
#[cfg(target_os = "macos")]
use crate::config::{CoreMlComputeUnits, CoreMlConfig};
use crate::models::download::{DownloadProgress, ModelDownloader};
use crate::models::generator_new::TextGeneration;

/// ONNX model loader - downloads and loads models from HuggingFace
#[allow(dead_code)]
pub struct OnnxLoader {
    cache_dir: PathBuf,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CoreMlOrtOptions {
    compute_units: ep::coreml::ComputeUnits,
    profile_compute_plan: bool,
    enable_subgraphs: bool,
}

#[cfg(target_os = "macos")]
fn coreml_ort_options(config: CoreMlConfig) -> CoreMlOrtOptions {
    let compute_units = match config.compute_units {
        CoreMlComputeUnits::All => ep::coreml::ComputeUnits::All,
        CoreMlComputeUnits::CpuAndNeuralEngine => ep::coreml::ComputeUnits::CPUAndNeuralEngine,
        CoreMlComputeUnits::CpuAndGpu => ep::coreml::ComputeUnits::CPUAndGPU,
        CoreMlComputeUnits::CpuOnly => ep::coreml::ComputeUnits::CPUOnly,
    };

    CoreMlOrtOptions {
        compute_units,
        profile_compute_plan: config.profile_compute_plan,
        enable_subgraphs: config.enable_subgraphs,
    }
}

#[cfg(target_os = "macos")]
fn coreml_execution_provider(config: CoreMlConfig) -> ort::ep::ExecutionProviderDispatch {
    let options = coreml_ort_options(config);
    ep::CoreML::default()
        .with_compute_units(options.compute_units)
        .with_profile_compute_plan(options.profile_compute_plan)
        .with_subgraphs(options.enable_subgraphs)
        .build()
}

#[cfg(target_os = "macos")]
const COREML_PROFILE_PREDICATE: &str = r#"composedMessage BEGINSWITH "Operation:" OR composedMessage BEGINSWITH "profile function :" OR composedMessage BEGINSWITH "Error loading compute plan:" OR composedMessage BEGINSWITH "Error loading program from compute plan.""#;

#[cfg(target_os = "macos")]
const COREML_PROFILE_PREFIXES: [&str; 4] = [
    "Operation:",
    "profile function :",
    "Error loading compute plan:",
    "Error loading program from compute plan.",
];

#[cfg(target_os = "macos")]
const COREML_PROFILE_MAX_BYTES: usize = 16 * 1024 * 1024;

#[cfg(target_os = "macos")]
#[derive(Debug, PartialEq, Eq)]
struct CoreMlProfileCaptureReport {
    output_path: PathBuf,
    diagnostic_record_count: usize,
    placement_record_count: usize,
    truncated: bool,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CoreMlDiagnosticManifest {
    schema_version: u32,
    hardware_arch: String,
    hardware_model: Option<String>,
    hardware_model_unavailable_reason: Option<String>,
    model_name: String,
    model_repository: String,
    model_input_shapes: Option<Vec<String>>,
    model_input_shapes_unavailable_reason: Option<String>,
    ort_crate_version: String,
    onnx_runtime_version: String,
    coreml_framework_version: Option<String>,
    coreml_framework_version_unavailable_reason: Option<String>,
    requested_compute_units: String,
    requested_subgraphs: bool,
    requested_profile_compute_plan: bool,
    session_created: bool,
    session_creation_latency_ms: u128,
    process_memory_before_bytes: Option<u64>,
    process_memory_after_bytes: Option<u64>,
    process_memory_unavailable_reason: Option<String>,
    placement_records_observed: usize,
    fallback_observed: Option<bool>,
    fallback_unavailable_reason: Option<String>,
    capture_truncated: bool,
}

#[cfg(target_os = "macos")]
struct CoreMlProfileReadReport {
    diagnostic_record_count: usize,
    placement_record_count: usize,
    truncated: bool,
}

#[cfg(target_os = "macos")]
struct CoreMlProfileCapture {
    child: Child,
    output_path: PathBuf,
    reader: Option<JoinHandle<Result<CoreMlProfileReadReport>>>,
    finished: bool,
}

#[cfg(target_os = "macos")]
impl CoreMlProfileCapture {
    fn start(output_path: PathBuf) -> Result<Self> {
        let command = coreml_profile_log_command(std::process::id());
        Self::start_with_command(output_path, command)
    }

    fn start_with_command(output_path: PathBuf, mut command: Command) -> Result<Self> {
        let output = open_private_profile_output(&output_path)?;
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .context("Failed to start process-scoped CoreML compute-plan capture")?;
        let stdout = child
            .stdout
            .take()
            .context("CoreML compute-plan capture did not provide stdout")?;
        let reader = std::thread::spawn(move || drain_profile_stream(stdout, output));

        std::thread::sleep(Duration::from_millis(200));
        if let Some(status) = child
            .try_wait()
            .context("Failed to inspect CoreML compute-plan capture")?
        {
            let _ = reader.join();
            bail!(
                "CoreML compute-plan capture exited before session creation ({status}); no placement evidence was captured"
            );
        }

        Ok(Self {
            child,
            output_path,
            reader: Some(reader),
            finished: false,
        })
    }

    fn finish(mut self) -> Result<CoreMlProfileCaptureReport> {
        let already_exited = self
            .child
            .try_wait()
            .context("Failed to inspect CoreML compute-plan capture")?;
        if already_exited.is_none() {
            // Give unified logging a bounded opportunity to flush records that
            // were emitted immediately before session creation returned.
            std::thread::sleep(Duration::from_millis(500));
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.child.id() as i32),
                nix::sys::signal::Signal::SIGTERM,
            )
            .context("Failed to stop CoreML compute-plan capture")?;
            for _ in 0..20 {
                if self
                    .child
                    .try_wait()
                    .context("Failed to inspect CoreML compute-plan capture during shutdown")?
                    .is_some()
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if self
                .child
                .try_wait()
                .context("Failed to inspect CoreML compute-plan capture after shutdown")?
                .is_none()
            {
                self.child
                    .kill()
                    .context("Failed to force-stop CoreML compute-plan capture")?;
            }
        }
        let status = self
            .child
            .wait()
            .context("Failed to reap CoreML compute-plan capture")?;

        let exited_unexpectedly = already_exited.is_some() && !status.success();
        let read_report = self
            .reader
            .take()
            .context("CoreML compute-plan capture reader was missing")?
            .join()
            .map_err(|_| anyhow::anyhow!("CoreML compute-plan capture reader panicked"))??;
        self.finished = true;

        if exited_unexpectedly {
            bail!(
                "CoreML compute-plan capture exited unexpectedly ({status}); no placement evidence is available"
            );
        }

        Ok(CoreMlProfileCaptureReport {
            output_path: self.output_path.clone(),
            diagnostic_record_count: read_report.diagnostic_record_count,
            placement_record_count: read_report.placement_record_count,
            truncated: read_report.truncated,
        })
    }
}

#[cfg(target_os = "macos")]
fn coreml_profile_log_command(pid: u32) -> Command {
    let pid = pid.to_string();
    let mut command = Command::new("/usr/bin/log");
    command.args([
        "stream",
        "--process",
        pid.as_str(),
        "--style",
        "ndjson",
        "--level",
        "info",
        "--type",
        "log",
        "--timeout",
        "30m",
        "--predicate",
        COREML_PROFILE_PREDICATE,
    ]);
    command
}

#[cfg(target_os = "macos")]
impl Drop for CoreMlProfileCapture {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        );
        let mut exited = false;
        for _ in 0..4 {
            if self.child.try_wait().ok().flatten().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if !exited {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(target_os = "macos")]
fn drain_profile_stream(
    stdout: std::process::ChildStdout,
    mut output: File,
) -> Result<CoreMlProfileReadReport> {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let mut bytes_written = 0usize;
    let mut diagnostic_record_count = 0usize;
    let mut placement_record_count = 0usize;
    let mut truncated = false;

    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .context("Failed to read CoreML compute-plan capture stream")?
            == 0
        {
            break;
        }
        if COREML_PROFILE_PREFIXES
            .iter()
            .any(|prefix| line.contains(prefix))
        {
            diagnostic_record_count += 1;
        }
        if line.contains("Operation:") {
            placement_record_count += 1;
        }

        if bytes_written.saturating_add(line.len()) <= COREML_PROFILE_MAX_BYTES {
            output
                .write_all(line.as_bytes())
                .context("Failed to write CoreML compute-plan capture")?;
            bytes_written += line.len();
        } else {
            truncated = true;
        }
    }
    output
        .flush()
        .context("Failed to flush CoreML compute-plan capture")?;

    Ok(CoreMlProfileReadReport {
        diagnostic_record_count,
        placement_record_count,
        truncated,
    })
}

#[cfg(target_os = "macos")]
fn open_private_profile_output(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .context("CoreML compute-plan output has no parent directory")?;
    reject_symlinked_existing_ancestors(parent)?;
    let parent_existed = parent.exists();
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "Failed to create CoreML diagnostic directory {}",
            parent.display()
        )
    })?;
    let parent_metadata = fs::symlink_metadata(parent).with_context(|| {
        format!(
            "Failed to inspect CoreML diagnostic directory {}",
            parent.display()
        )
    })?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        bail!(
            "CoreML diagnostic directory {} must be a real directory",
            parent.display()
        );
    }
    if !parent_existed {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "Failed to protect CoreML diagnostic directory {}",
                parent.display()
            )
        })?;
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .with_context(|| {
            format!(
                "Failed to open private CoreML compute-plan output {}",
                path.display()
            )
        })?;
    let metadata = file.metadata().with_context(|| {
        format!(
            "Failed to inspect CoreML compute-plan output {}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        bail!(
            "CoreML compute-plan output {} must be a regular, unlinked file",
            path.display()
        );
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| {
            format!(
                "Failed to protect CoreML compute-plan output {}",
                path.display()
            )
        })?;
    file.set_len(0).with_context(|| {
        format!(
            "Failed to truncate CoreML compute-plan output {}",
            path.display()
        )
    })?;
    Ok(file)
}

#[cfg(target_os = "macos")]
fn reject_symlinked_existing_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        let metadata = match fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Failed to inspect CoreML diagnostic ancestor {}",
                        ancestor.display()
                    )
                });
            }
        };
        if metadata.file_type().is_symlink() {
            bail!(
                "CoreML diagnostic path contains symlinked ancestor {}",
                ancestor.display()
            );
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn process_memory_bytes() -> Option<u64> {
    let system = sysinfo::System::new_all();
    let pid = sysinfo::get_current_pid().ok()?;
    system.process(pid).map(sysinfo::Process::memory)
}

#[cfg(target_os = "macos")]
fn hardware_model() -> Option<String> {
    let output = Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.model"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(target_os = "macos")]
fn coreml_manifest_path(output_path: &Path) -> PathBuf {
    output_path.with_extension("manifest.json")
}

#[cfg(target_os = "macos")]
fn write_coreml_manifest(
    output_path: &Path,
    manifest: &CoreMlDiagnosticManifest,
) -> Result<PathBuf> {
    let manifest_path = coreml_manifest_path(output_path);
    let mut output = open_private_profile_output(&manifest_path)?;
    serde_json::to_writer_pretty(&mut output, manifest)
        .context("Failed to serialize private CoreML diagnostic manifest")?;
    output
        .write_all(b"\n")
        .context("Failed to terminate private CoreML diagnostic manifest")?;
    output
        .flush()
        .context("Failed to flush private CoreML diagnostic manifest")?;
    Ok(manifest_path)
}

#[cfg(target_os = "macos")]
fn coreml_provider_requested(config: &OnnxLoadConfig) -> bool {
    config
        .execution_providers
        .as_ref()
        .map_or(true, |providers| {
            providers.contains(&ConfigExecutionProvider::CoreML)
        })
}

#[cfg(target_os = "macos")]
fn commit_with_coreml_diagnostics<T, Start, Commit, Describe>(
    config: &OnnxLoadConfig,
    start_capture: Start,
    commit: Commit,
    describe_inputs: Describe,
) -> Result<(T, Option<(CoreMlProfileCaptureReport, PathBuf)>)>
where
    Start: FnOnce(PathBuf) -> Result<CoreMlProfileCapture>,
    Commit: FnOnce() -> std::result::Result<T, ort::Error>,
    Describe: FnOnce(&T) -> Vec<String>,
{
    let capture = if config.coreml.profile_compute_plan && coreml_provider_requested(config) {
        let output_path = config
            .coreml_profile_output
            .clone()
            .context("CoreML compute-plan profiling requires a private diagnostic output path")?;
        Some(start_capture(output_path)?)
    } else {
        None
    };
    let memory_before = process_memory_bytes();
    let started = Instant::now();
    let committed = commit();
    let input_shapes = committed.as_ref().ok().map(describe_inputs);
    let latency_ms = started.elapsed().as_millis();
    let memory_after = process_memory_bytes();
    let capture_report = capture.map(CoreMlProfileCapture::finish).transpose()?;

    let report_and_manifest = if let Some(report) = capture_report {
        let hardware_model = hardware_model();
        let manifest = CoreMlDiagnosticManifest {
            schema_version: 1,
            hardware_arch: std::env::consts::ARCH.to_string(),
            hardware_model: hardware_model.clone(),
            hardware_model_unavailable_reason: hardware_model
                .is_none()
                .then(|| "hw.model was unavailable from sysctl".to_string()),
            model_name: config.model_name.clone(),
            model_repository: config.repo_id.clone(),
            model_input_shapes: input_shapes,
            model_input_shapes_unavailable_reason: committed.is_err().then(|| {
                "Session creation failed before ONNX input metadata was available".to_string()
            }),
            ort_crate_version: "2.0.0-rc.11".to_string(),
            onnx_runtime_version: "1.23.2".to_string(),
            coreml_framework_version: None,
            coreml_framework_version_unavailable_reason: Some(
                "CoreML does not expose a framework version through the pinned ort API".to_string(),
            ),
            requested_compute_units: config.coreml.compute_units.name().to_string(),
            requested_subgraphs: config.coreml.enable_subgraphs,
            requested_profile_compute_plan: config.coreml.profile_compute_plan,
            session_created: committed.is_ok(),
            session_creation_latency_ms: latency_ms,
            process_memory_before_bytes: memory_before,
            process_memory_after_bytes: memory_after,
            process_memory_unavailable_reason: (memory_before.is_none() || memory_after.is_none())
                .then(|| "Process memory was unavailable from sysinfo".to_string()),
            placement_records_observed: report.placement_record_count,
            fallback_observed: None,
            fallback_unavailable_reason: Some(
                "CoreML compute-plan records do not provide a reliable fallback verdict"
                    .to_string(),
            ),
            capture_truncated: report.truncated,
        };
        let manifest_path = write_coreml_manifest(&report.output_path, &manifest)?;
        Some((report, manifest_path))
    } else {
        None
    };

    let value = committed.context("Failed to create ONNX session")?;
    Ok((value, report_and_manifest))
}

impl OnnxLoader {
    /// Create new ONNX loader with cache directory
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Create ONNX Runtime session with execution providers
    fn create_session(&self, model_path: &Path, config: &OnnxLoadConfig) -> Result<Session> {
        info!("Creating ONNX session from: {:?}", model_path);

        // Build execution provider list
        let mut builder = Session::builder()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .with_intra_threads(4)
            .map_err(|e| anyhow::anyhow!("{e}"))?; // Parallel ops within layer

        // Add execution providers based on config
        let providers = self.get_execution_providers(config);
        if !providers.is_empty() {
            builder = builder
                .with_execution_providers(providers)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        // CoreML emits compute-plan details through Apple unified logging
        // during session creation, so the scoped capture must span this call.
        #[cfg(target_os = "macos")]
        let (session, capture_result) = commit_with_coreml_diagnostics(
            config,
            CoreMlProfileCapture::start,
            || builder.commit_from_file(model_path),
            |session| {
                session
                    .inputs()
                    .iter()
                    .map(|input| format!("{}: {:?}", input.name(), input.dtype()))
                    .collect()
            },
        )?;

        #[cfg(not(target_os = "macos"))]
        let session = builder
            .commit_from_file(model_path)
            .context("Failed to create ONNX session")?;

        #[cfg(target_os = "macos")]
        if let Some((report, manifest_path)) = capture_result {
            if report.placement_record_count == 0 {
                warn!(
                    output = %report.output_path.display(),
                    manifest = %manifest_path.display(),
                    diagnostic_records = report.diagnostic_record_count,
                    truncated = report.truncated,
                    "CoreML compute-plan capture received no operation placement records; placement remains unobserved"
                );
            } else {
                info!(
                    output = %report.output_path.display(),
                    manifest = %manifest_path.display(),
                    placement_records = report.placement_record_count,
                    diagnostic_records = report.diagnostic_record_count,
                    truncated = report.truncated,
                    "CoreML compute-plan capture received operation placement records"
                );
            }
        }

        info!("ONNX session created successfully");

        Ok(session)
    }

    /// Get execution providers based on backend configuration
    fn get_execution_providers(
        &self,
        config: &OnnxLoadConfig,
    ) -> Vec<ort::ep::ExecutionProviderDispatch> {
        let mut providers = vec![];

        // Add execution providers based on config
        if let Some(exec_providers) = &config.execution_providers {
            for provider in exec_providers {
                match provider {
                    ConfigExecutionProvider::CoreML => {
                        #[cfg(target_os = "macos")]
                        {
                            info!(
                                requested_compute_units = config.coreml.compute_units.name(),
                                profile_compute_plan = config.coreml.profile_compute_plan,
                                enable_subgraphs = config.coreml.enable_subgraphs,
                                "Requesting CoreML execution provider; actual placement is runtime-selected"
                            );
                            providers.push(coreml_execution_provider(config.coreml));
                        }
                        #[cfg(not(target_os = "macos"))]
                        warn!("CoreML was requested on a non-macOS host; falling back to CPU");
                    }
                    ConfigExecutionProvider::CUDA => {
                        #[cfg(feature = "cuda")]
                        {
                            info!("Requesting CUDA execution provider");
                            providers.push(ep::CUDA::default().build());
                        }
                    }
                    ConfigExecutionProvider::CPU => {
                        info!("Requesting CPU execution provider");
                        providers.push(ep::CPU::default().build());
                    }
                    ConfigExecutionProvider::TensorRT => {
                        #[cfg(feature = "cuda")]
                        {
                            info!("Requesting TensorRT execution provider");
                            providers.push(ep::TensorRT::default().build());
                        }
                    }
                    ConfigExecutionProvider::DirectML => {
                        #[cfg(target_os = "windows")]
                        {
                            info!("Requesting DirectML execution provider");
                            providers.push(ep::DirectML::default().build());
                        }
                    }
                }
            }
        } else {
            // Default: Try platform-specific providers first, then CPU
            #[cfg(target_os = "macos")]
            {
                info!(
                    requested_compute_units = config.coreml.compute_units.name(),
                    profile_compute_plan = config.coreml.profile_compute_plan,
                    enable_subgraphs = config.coreml.enable_subgraphs,
                    "Auto-selecting CoreML provider; actual placement is runtime-selected"
                );
                providers.push(coreml_execution_provider(config.coreml));
            }

            #[cfg(feature = "cuda")]
            {
                info!("Auto-selecting: Trying CUDA");
                providers.push(ep::CUDA::default().build());
            }
        }

        // Always add CPU as fallback
        info!("Adding CPU as fallback provider");
        providers.push(ep::CPU::default().build());

        providers
    }

    /// Load ONNX model with progress tracking
    pub fn load_model_sync(&self, config: &OnnxLoadConfig) -> Result<LoadedOnnxModel> {
        info!("Loading ONNX model: {}", config.model_name);

        // Step 1: Download model files from HuggingFace
        let (model_dir, _progress_rx) = self.download_model_files(config)?;

        // Step 2: Find model.onnx file
        // onnx-community repos store models in onnx/ subdirectory
        let onnx_subdir_path = model_dir.join("onnx").join("model.onnx");
        let root_path = model_dir.join("model.onnx");

        let model_path = if onnx_subdir_path.exists() {
            info!("Found ONNX model at: {:?}", onnx_subdir_path);
            onnx_subdir_path
        } else if root_path.exists() {
            info!("Found ONNX model at: {:?}", root_path);
            root_path
        } else {
            bail!(
                "ONNX model file not found.\n\
                 Tried:\n\
                 - {:?}\n\
                 - {:?}",
                onnx_subdir_path,
                root_path
            );
        };

        // Step 3: Load tokenizer
        let tokenizer = self.load_tokenizer(&model_dir)?;

        // Step 4: Create ONNX Runtime session
        let session = self.create_session(&model_path, config)?;

        info!("Successfully loaded ONNX model: {}", config.model_name);

        Ok(LoadedOnnxModel {
            session,
            tokenizer,
            model_name: config.model_name.clone(),
            model_size: config.size,
            model_path,
        })
    }

    /// Download model files from HuggingFace Hub
    fn download_model_files(
        &self,
        config: &OnnxLoadConfig,
    ) -> Result<(PathBuf, mpsc::Receiver<DownloadProgress>)> {
        let repo = config.huggingface_repo();
        info!("Downloading from HuggingFace: {}", repo);

        let downloader = ModelDownloader::new()?;

        // Estimate size based on model size
        let estimated_size_gb = match config.size {
            ModelSize::Small => 0.5,
            ModelSize::Medium => 1.5,
            ModelSize::Large => 3.0,
            ModelSize::XLarge => 7.0,
        };

        // Download model files (model.onnx + model.onnx_data if exists)
        let (model_dir, progress_rx) = downloader
            .download_model(&repo, estimated_size_gb)
            .context("Failed to download ONNX model")?;

        Ok((model_dir, progress_rx))
    }

    /// Load tokenizer from model directory
    fn load_tokenizer(&self, model_dir: &Path) -> Result<Tokenizer> {
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !tokenizer_path.exists() {
            bail!("Tokenizer file not found: {:?}", tokenizer_path);
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
            anyhow::anyhow!("Failed to load tokenizer from {:?}: {}", tokenizer_path, e)
        })?;

        debug!("Tokenizer loaded successfully");
        Ok(tokenizer)
    }
}

/// Loaded ONNX model with tokenizer
pub struct LoadedOnnxModel {
    session: Session,
    tokenizer: Tokenizer,
    model_name: String,
    model_size: ModelSize,
    model_path: PathBuf,
}

impl LoadedOnnxModel {
    /// Get model name
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Get model size
    pub fn model_size(&self) -> ModelSize {
        self.model_size
    }

    /// Generate text from prompt
    ///
    /// NOTE: This is a placeholder for Phase 2.
    /// Full implementation in Phase 3 will handle:
    /// - ONNX Runtime session creation and inference
    /// - Streaming generation
    /// - Proper sampling (temperature, top_p, etc.)
    /// - Attention masks and position IDs
    /// - KV cache management
    /// - Stop tokens
    pub fn generate(&self, prompt: &str, _max_tokens: usize) -> Result<String> {
        info!("Generating response for prompt (placeholder)");

        // Step 1: Tokenize input (verify tokenizer works)
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("Failed to encode prompt: {}", e))?;

        let input_ids = encoding.get_ids();
        debug!("Input tokens: {} tokens", input_ids.len());

        // For Phase 2, return placeholder indicating ONNX structure is in place
        warn!("ONNX generation not yet fully implemented - returning placeholder");
        Ok(format!(
            "[ONNX placeholder - model: {}, tokenized {} tokens]",
            self.model_name,
            input_ids.len()
        ))
    }

    /// Get tokenizer reference
    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    /// Get model path
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    /// Autoregressive text generation with KV cache (Phase 5.1)
    fn generate_autoregressive(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
    ) -> Result<Vec<u32>> {
        self.generate_autoregressive_with_callback(input_ids, max_new_tokens, None)
    }

    /// Generate tokens autoregressively with optional streaming callback
    fn generate_autoregressive_with_callback(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        mut token_callback: Option<TokenCallback>,
    ) -> Result<Vec<u32>> {
        info!(
            "ONNX autoregressive generation: {} input tokens, max {} new tokens",
            input_ids.len(),
            max_new_tokens
        );

        let mut output_ids = input_ids.to_vec();
        let eos_token_id = self.get_eos_token_id();

        // Model architecture (from config.json)
        const NUM_LAYERS: usize = 28;
        const NUM_KV_HEADS: usize = 2;
        const HEAD_DIM: usize = 128; // hidden_size / num_attention_heads = 1536 / 12

        // Initialize empty KV cache for first step
        let mut past_key_values: Vec<(DynValue, DynValue)> = Vec::new();
        let mut past_seq_len = 0;

        // Generation loop
        for step in 0..max_new_tokens {
            debug!("Generation step {}/{}", step + 1, max_new_tokens);

            // 1. Prepare input tensor - only the new token(s) after first step
            let input_for_step = if step == 0 {
                &output_ids[..] // First step: all input tokens
            } else {
                &output_ids[output_ids.len() - 1..] // Subsequent: only last generated token
            };

            // 2. Run inference with KV cache using IoBinding
            let (logits, new_kv_cache) = self.run_with_kv_cache(
                input_for_step,
                &past_key_values,
                past_seq_len,
                NUM_LAYERS,
                NUM_KV_HEADS,
                HEAD_DIM,
            )?;

            // Update sequence length for next iteration
            past_seq_len += input_for_step.len();

            // Update KV cache for next iteration
            past_key_values = new_kv_cache;

            // 3. Sample next token with repetition penalty (pass previous output tokens)
            let previous_output = &output_ids[input_ids.len()..]; // Only new tokens, not input
            let next_token = Self::sample_token_with_params(
                &logits,
                previous_output,
                0.7,  // temperature: moderate randomness
                0.9,  // top_p: nucleus sampling
                1.15, // repetition_penalty: discourage repetition
            )?;
            debug!("Generated token: {}", next_token);

            // 4. Check for EOS
            if next_token == eos_token_id {
                info!("EOS token generated, stopping");
                break;
            }

            // 5. Append to output
            output_ids.push(next_token);

            // 6. Call streaming callback if provided
            if let Some(ref mut callback) = token_callback {
                // Decode just this token to text
                let token_text = self
                    .tokenizer
                    .decode(&[next_token], false)
                    .unwrap_or_else(|_| format!("[token_{}]", next_token));
                callback(next_token, &token_text);
            }
        }

        info!(
            "Generated {} new tokens",
            output_ids.len() - input_ids.len()
        );
        Ok(output_ids)
    }

    /// Run inference with KV cache using IoBinding for dynamic inputs
    fn run_with_kv_cache(
        &mut self,
        input_tokens: &[u32],
        past_kv: &[(DynValue, DynValue)],
        past_seq_len: usize,
        num_layers: usize,
        num_kv_heads: usize,
        head_dim: usize,
    ) -> Result<ForwardOutput> {
        // Prepare input_ids tensor
        let input_tensor = self.prepare_input(input_tokens)?;

        // Prepare position_ids tensor
        let position_ids = self.prepare_position_ids(input_tokens.len(), past_seq_len)?;

        // Prepare attention_mask tensor
        let attention_mask = self.prepare_attention_mask(input_tokens.len(), past_seq_len)?;

        // For first step, create empty KV cache tensors
        let kv_cache = if past_seq_len == 0 {
            // Empty cache: shape [1, num_kv_heads, 0, head_dim]
            let mut cache = Vec::new();
            for _ in 0..num_layers {
                let empty_key = ndarray::Array4::<f32>::zeros((1, num_kv_heads, 0, head_dim));
                let empty_value = ndarray::Array4::<f32>::zeros((1, num_kv_heads, 0, head_dim));

                let key_val = Value::from_array(empty_key)?.into_dyn();
                let value_val = Value::from_array(empty_value)?.into_dyn();

                cache.push((key_val, value_val));
            }
            cache
        } else {
            // Reuse existing cache from previous step (already owned Values)
            Vec::new() // Will bind past_kv directly below
        };

        // Create IoBinding for dynamic inputs
        let mut binding = self.session.create_binding()?;

        // Bind input_ids
        binding.bind_input("input_ids", &input_tensor)?;

        // Bind position_ids
        binding.bind_input("position_ids", &position_ids)?;

        // Bind attention_mask
        binding.bind_input("attention_mask", &attention_mask)?;

        // Bind past_key_values for each layer
        let cache_to_bind = if past_seq_len == 0 {
            &kv_cache
        } else {
            past_kv
        };
        for (layer_idx, (key, value)) in cache_to_bind.iter().enumerate() {
            let key_name = format!("past_key_values.{}.key", layer_idx);
            let value_name = format!("past_key_values.{}.value", layer_idx);

            binding.bind_input(&key_name, key)?;
            binding.bind_input(&value_name, value)?;
        }

        // Bind outputs to device memory (shape unknown, use bind_output_to_device)
        let mem_info = MemoryInfo::default(); // CPU memory
        binding.bind_output_to_device("logits", &mem_info)?;
        for layer_idx in 0..num_layers {
            let key_name = format!("present.{}.key", layer_idx);
            let value_name = format!("present.{}.value", layer_idx);

            binding.bind_output_to_device(&key_name, &mem_info)?;
            binding.bind_output_to_device(&value_name, &mem_info)?;
        }

        // Run inference (correct API: run_binding returns SessionOutputs)
        let mut outputs = self.session.run_binding(&binding)?;

        // Extract logits
        let logits = Self::extract_logits_static(&outputs, input_tokens.len())?;

        // Extract new KV cache by consuming outputs to get owned DynValues
        let mut new_cache = Vec::new();
        for layer_idx in 0..num_layers {
            let key_name = format!("present.{}.key", layer_idx);
            let value_name = format!("present.{}.value", layer_idx);

            // Get owned DynValue by removing from outputs
            let key_output = outputs
                .remove(&key_name)
                .ok_or_else(|| anyhow::anyhow!("Missing output: {}", key_name))?;
            let value_output = outputs
                .remove(&value_name)
                .ok_or_else(|| anyhow::anyhow!("Missing output: {}", value_name))?;

            new_cache.push((key_output, value_output));
        }

        Ok((logits, new_cache))
    }

    /// Prepare input tensor for ONNX Runtime
    fn prepare_input(&self, tokens: &[u32]) -> Result<DynValue> {
        debug!("Preparing input tensor: {} tokens", tokens.len());

        // Convert u32 tokens to i64 (ONNX typically expects int64)
        let input_data: Vec<i64> = tokens.iter().map(|&t| t as i64).collect();

        // Create tensor with shape [batch_size=1, seq_len]
        let array = ndarray::Array2::from_shape_vec((1, tokens.len()), input_data)
            .context("Failed to create ndarray for input")?;

        // Convert to ort::Value (ndarray feature enabled)
        // Use into_dyn() to erase the specific type to DynValueTypeMarker
        let value = Value::from_array(array).context("Failed to create ONNX Value from array")?;
        Ok(value.into_dyn())
    }

    /// Prepare position_ids tensor for ONNX Runtime
    fn prepare_position_ids(&self, seq_len: usize, past_seq_len: usize) -> Result<DynValue> {
        debug!(
            "Preparing position_ids: seq_len={}, past_seq_len={}",
            seq_len, past_seq_len
        );

        // Position IDs start from past_seq_len and go to past_seq_len + seq_len
        // For first step with seq_len=5: [0, 1, 2, 3, 4]
        // For second step with seq_len=1, past_seq_len=5: [5]
        let position_data: Vec<i64> = (past_seq_len..past_seq_len + seq_len)
            .map(|i| i as i64)
            .collect();

        // Create tensor with shape [batch_size=1, seq_len]
        let array = ndarray::Array2::from_shape_vec((1, seq_len), position_data)
            .context("Failed to create ndarray for position_ids")?;

        // Convert to ort::Value
        let value = Value::from_array(array)
            .context("Failed to create ONNX Value from position_ids array")?;
        Ok(value.into_dyn())
    }

    /// Prepare attention_mask tensor for ONNX Runtime
    fn prepare_attention_mask(&self, seq_len: usize, past_seq_len: usize) -> Result<DynValue> {
        debug!(
            "Preparing attention_mask: seq_len={}, past_seq_len={}",
            seq_len, past_seq_len
        );

        // Attention mask is all 1s for the total sequence length
        // Shape: [batch_size=1, total_seq_len]
        let total_seq_len = past_seq_len + seq_len;
        let mask_data: Vec<i64> = vec![1; total_seq_len];

        // Create tensor with shape [batch_size=1, total_seq_len]
        let array = ndarray::Array2::from_shape_vec((1, total_seq_len), mask_data)
            .context("Failed to create ndarray for attention_mask")?;

        // Convert to ort::Value
        let value = Value::from_array(array)
            .context("Failed to create ONNX Value from attention_mask array")?;
        Ok(value.into_dyn())
    }

    /// Extract logits from ONNX session output (static to avoid borrowing issues)
    fn extract_logits_static(outputs: &SessionOutputs, seq_len: usize) -> Result<Vec<f32>> {
        debug!("Extracting logits from output");

        // Get the first output by name (typically "logits" or similar)
        // Try common output names first
        let output_tensor = outputs
            .get("logits")
            .or_else(|| outputs.get("output"))
            .or_else(|| outputs.get("last_hidden_state"))
            .ok_or_else(|| anyhow::anyhow!("No output tensor found with expected names"))?;

        // Extract tensor data as f32
        // try_extract_tensor returns Result<(shape, data_slice)>
        let (shape, data) = output_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| anyhow::anyhow!("Failed to extract f32 tensor: {e}"))?;

        debug!("Output tensor shape: {:?}", shape);

        // Shape is typically [batch_size, seq_len, vocab_size]
        if shape.len() != 3 {
            bail!("Expected 3D output tensor, got shape: {:?}", shape);
        }

        let vocab_size = shape[2] as usize;
        let last_token_offset = (seq_len - 1) * vocab_size;

        // Extract the last token's logits
        let logits: Vec<f32> = data
            .iter()
            .skip(last_token_offset)
            .take(vocab_size)
            .copied()
            .collect();

        debug!("Extracted {} logits for last token", logits.len());
        Ok(logits)
    }

    /// Sample next token from logits (greedy sampling) - static to avoid borrowing issues
    #[allow(dead_code)]
    fn sample_token_static(logits: &[f32]) -> Result<u32> {
        Self::sample_token_with_params(logits, &[], 0.7, 0.9, 1.1)
    }

    /// Sample token with temperature, top-p, and repetition penalty
    fn sample_token_with_params(
        logits: &[f32],
        previous_tokens: &[u32],
        temperature: f32,
        top_p: f32,
        repetition_penalty: f32,
    ) -> Result<u32> {
        if logits.is_empty() {
            bail!("Cannot sample from empty logits");
        }

        let mut scores = logits.to_vec();

        // Apply repetition penalty
        if repetition_penalty != 1.0 && !previous_tokens.is_empty() {
            for &token_id in previous_tokens {
                if (token_id as usize) < scores.len() {
                    let score = scores[token_id as usize];
                    // If score > 0, divide by penalty; if score < 0, multiply by penalty
                    scores[token_id as usize] = if score > 0.0 {
                        score / repetition_penalty
                    } else {
                        score * repetition_penalty
                    };
                }
            }
        }

        // Apply temperature
        if temperature != 1.0 {
            for score in &mut scores {
                *score /= temperature;
            }
        }

        // Convert logits to probabilities using softmax
        let max_score = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let exp_scores: Vec<f32> = scores.iter().map(|&s| (s - max_score).exp()).collect();
        let sum_exp: f32 = exp_scores.iter().sum();
        let probs: Vec<f32> = exp_scores.iter().map(|&e| e / sum_exp).collect();

        // Create sorted indices by probability (descending)
        let mut indexed_probs: Vec<(usize, f32)> = probs.iter().cloned().enumerate().collect();
        indexed_probs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Top-p (nucleus) sampling
        let mut cumulative_prob = 0.0;
        let mut top_p_indices = Vec::new();

        for &(idx, prob) in &indexed_probs {
            cumulative_prob += prob;
            top_p_indices.push((idx, prob));
            if cumulative_prob >= top_p {
                break;
            }
        }

        // Renormalize probabilities for selected tokens
        let selected_prob_sum: f32 = top_p_indices.iter().map(|(_, p)| p).sum();
        if selected_prob_sum <= 0.0 {
            // Fallback to greedy if something went wrong
            if let Some(&(max_idx, _)) = indexed_probs.first() {
                return Ok(max_idx as u32);
            }
            bail!("No valid tokens to sample");
        }

        // Sample from the top-p distribution
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let mut rand_val: f32 = rng.gen::<f32>() * selected_prob_sum;

        for &(idx, prob) in &top_p_indices {
            rand_val -= prob;
            if rand_val <= 0.0 {
                debug!(
                    "Sampled token {} (prob: {:.4}, temp: {:.1}, top_p: {:.1})",
                    idx, prob, temperature, top_p
                );
                return Ok(idx as u32);
            }
        }

        // Fallback (should not reach here)
        if let Some(&(idx, _)) = top_p_indices.last() {
            Ok(idx as u32)
        } else {
            bail!("Failed to sample token")
        }
    }

    /// Get EOS token ID from tokenizer
    fn get_eos_token_id(&self) -> u32 {
        // Try to get from tokenizer's special tokens
        // For Qwen models, EOS is typically 151643
        // Fallback to common value if not available
        let vocab = self.tokenizer.get_vocab(true);

        vocab
            .get("<|endoftext|>")
            .or_else(|| vocab.get("<|im_end|>"))
            .or_else(|| vocab.get("</s>"))
            .copied()
            .unwrap_or(151643)
    }
}

// Implement TextGeneration trait
impl TextGeneration for LoadedOnnxModel {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        self.generate_autoregressive(input_ids, max_new_tokens)
    }

    fn generate_stream(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        token_callback: crate::models::TokenCallback,
    ) -> Result<Vec<u32>> {
        self.generate_autoregressive_with_callback(input_ids, max_new_tokens, Some(token_callback))
    }

    fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;
        Ok(encoding.get_ids().to_vec())
    }

    fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
        self.tokenizer
            .decode(tokens, true)
            .map_err(|e| anyhow::anyhow!("Decode failed: {}", e))
    }

    fn name(&self) -> &str {
        &self.model_name
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl std::fmt::Debug for LoadedOnnxModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedOnnxModel")
            .field("model_name", &self.model_name)
            .field("model_size", &self.model_size)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::loaders::onnx_config::ExecutionProvider;

    #[test]
    fn test_execution_providers_default() {
        let providers = ExecutionProvider::default_for_platform();
        assert!(!providers.is_empty());

        #[cfg(target_os = "macos")]
        {
            assert_eq!(providers[0], ExecutionProvider::CoreML);
            assert_eq!(providers[1], ExecutionProvider::CPU);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_coreml_compute_unit_policies_map_to_exact_ort_options() {
        use ep::coreml::ComputeUnits;

        for (policy, expected) in [
            (CoreMlComputeUnits::All, ComputeUnits::All),
            (
                CoreMlComputeUnits::CpuAndNeuralEngine,
                ComputeUnits::CPUAndNeuralEngine,
            ),
            (CoreMlComputeUnits::CpuAndGpu, ComputeUnits::CPUAndGPU),
            (CoreMlComputeUnits::CpuOnly, ComputeUnits::CPUOnly),
        ] {
            let options = coreml_ort_options(CoreMlConfig {
                compute_units: policy,
                ..CoreMlConfig::default()
            });
            assert_eq!(options.compute_units, expected);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_coreml_diagnostic_and_subgraph_flags_map_exactly() {
        for enabled in [false, true] {
            let options = coreml_ort_options(CoreMlConfig {
                profile_compute_plan: enabled,
                enable_subgraphs: !enabled,
                ..CoreMlConfig::default()
            });
            assert_eq!(options.profile_compute_plan, enabled);
            assert_eq!(options.enable_subgraphs, !enabled);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_create_session_boundary_wires_policy_capture_and_structured_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("diagnostics/compute-plan.ndjson");
        let unrelated_output = directory.path().join("general-finch.log");
        let ort_log_before = std::env::var_os("ORT_LOG");
        let rust_log_before = std::env::var_os("RUST_LOG");

        let mut config = OnnxLoadConfig::with_size(ModelSize::Small, directory.path().into());
        config.coreml = CoreMlConfig {
            compute_units: CoreMlComputeUnits::CpuAndNeuralEngine,
            profile_compute_plan: true,
            enable_subgraphs: true,
        };
        config.coreml_profile_output = Some(output.clone());
        let (_, report) = commit_with_coreml_diagnostics(
            &config,
            |path| {
                let mut source = Command::new("/bin/sh");
                source.args([
                    "-c",
                    "printf '%s\\n' '{\"composedMessage\":\"Operation: test_add, Device Usage: test_device, Estimated Cost: 0.5\"}'; exec sleep 5",
                ]);
                CoreMlProfileCapture::start_with_command(path, source)
            },
            || Ok(()),
            |_| vec!["input_ids: Tensor<Int64>[1, dynamic]".to_string()],
        )
        .unwrap();
        let (report, manifest_path) = report.unwrap();

        assert_eq!(report.diagnostic_record_count, 1);
        assert_eq!(report.placement_record_count, 1);
        assert!(!report.truncated);
        assert_eq!(report.output_path, output);
        let captured = fs::read_to_string(&output).unwrap();
        assert!(captured.contains("Operation: test_add"));
        assert!(!unrelated_output.exists());
        assert_eq!(std::env::var_os("ORT_LOG"), ort_log_before);
        assert_eq!(std::env::var_os("RUST_LOG"), rust_log_before);
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let manifest: CoreMlDiagnosticManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.requested_compute_units, "CPU + ANE");
        assert!(manifest.requested_subgraphs);
        assert!(manifest.requested_profile_compute_plan);
        assert!(manifest.session_created);
        assert_eq!(manifest.placement_records_observed, 1);
        assert_eq!(manifest.fallback_observed, None);
        assert_eq!(
            manifest.model_input_shapes,
            Some(vec!["input_ids: Tensor<Int64>[1, dynamic]".to_string()])
        );
        assert!(manifest.model_input_shapes_unavailable_reason.is_none());
        assert_eq!(
            fs::metadata(manifest_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_coreml_profile_predicate_is_pid_scoped_and_prefix_limited() {
        let command = coreml_profile_log_command(4242);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|args| args == ["--process", "4242"]));
        assert!(COREML_PROFILE_PREDICATE.contains("composedMessage BEGINSWITH"));
        for prefix in COREML_PROFILE_PREFIXES {
            assert!(COREML_PROFILE_PREDICATE.contains(prefix));
        }
        assert!(!COREML_PROFILE_PREDICATE.contains("CONTAINS"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_coreml_profile_output_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.ndjson");
        let output = directory.path().join("compute-plan.ndjson");
        fs::write(&target, "keep").unwrap();
        symlink(&target, &output).unwrap();

        let error = open_private_profile_output(&output).unwrap_err();
        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("CoreML compute-plan output")
                || diagnostic.to_ascii_lowercase().contains("symlink"),
            "unexpected symlink rejection diagnostic: {diagnostic}"
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "keep");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_coreml_profile_output_rejects_symlinked_ancestor() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("real");
        let linked = directory.path().join("linked");
        fs::create_dir(&real).unwrap();
        symlink(&real, &linked).unwrap();

        let error =
            open_private_profile_output(&linked.join("nested/compute-plan.ndjson")).unwrap_err();
        assert!(error.to_string().contains("symlinked ancestor"));
        assert!(!real.join("nested/compute-plan.ndjson").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the macOS unified logging service"]
    fn test_coreml_profile_capture_production_log_stream_smoke() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("compute-plan.ndjson");
        let mut config = OnnxLoadConfig::with_size(ModelSize::Small, directory.path().into());
        config.coreml.profile_compute_plan = true;
        config.coreml_profile_output = Some(output.clone());
        let (_, report) = commit_with_coreml_diagnostics(
            &config,
            CoreMlProfileCapture::start,
            || Ok(()),
            |_| vec!["synthetic smoke boundary".to_string()],
        )
        .unwrap();
        let (report, manifest_path) = report.unwrap();

        assert_eq!(report.output_path, output);
        assert!(report.diagnostic_record_count >= report.placement_record_count);
        assert!(!report.truncated);
        assert_eq!(
            fs::metadata(output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let manifest: CoreMlDiagnosticManifest =
            serde_json::from_slice(&fs::read(manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest.placement_records_observed,
            report.placement_record_count
        );
        assert_eq!(manifest.fallback_observed, None);
    }
}
