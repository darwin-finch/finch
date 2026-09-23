//! Managed Hugging Face artifacts for daemon-local GGUF chat models.

use anyhow::{bail, Context, Result};
use futures::StreamExt;
use hf_hub::{api::tokio::ApiBuilder, Cache, Repo, RepoType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::bootstrap::{DownloadProgressSnapshot, GeneratorState};
use super::unified_loader::{ModelFamily, ModelSize};

const HUGGING_FACE_ENDPOINT: &str = "https://huggingface.co";

/// Quantizations offered for Finch-managed GGUF artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GgufQuantization {
    /// Four-bit K-quant with medium mixed precision.
    #[default]
    Q4KM,
    /// Five-bit K-quant with medium mixed precision.
    Q5KM,
    /// Eight-bit round-to-nearest -- near-lossless. Not offered in the chat
    /// model picker (too large relative to the quality gain on multi-billion
    /// parameter models), but the right choice for the ~33M-parameter memory
    /// embedding model, where quantization error is proportionally larger
    /// and the absolute size cost of full fidelity is trivial (~37MB).
    Q8_0,
}

impl GgufQuantization {
    /// Stable GGUF quantization label.
    pub fn name(self) -> &'static str {
        match self {
            Self::Q4KM => "Q4_K_M",
            Self::Q5KM => "Q5_K_M",
            Self::Q8_0 => "Q8_0",
        }
    }
}

/// Immutable identity for one Finch-managed GGUF artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedGgufArtifact {
    /// Hugging Face model repository, for example `Qwen/Qwen2.5-3B-Instruct-GGUF`.
    pub repository: String,
    /// Immutable Hugging Face commit revision.
    pub revision: String,
    /// Exact repository-relative GGUF filename.
    pub filename: String,
    /// User-selected quantization.
    pub quantization: GgufQuantization,
    /// Expected artifact size in bytes.
    pub expected_size: u64,
    /// Expected lowercase SHA-256 digest.
    pub sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct VerificationMarker {
    sha256: String,
    size: u64,
    modified_nanos: u128,
}

impl ManagedGgufArtifact {
    pub(crate) fn validate(&self) -> Result<()> {
        let repository_parts = self.repository.split('/').collect::<Vec<_>>();
        if repository_parts.len() != 2
            || repository_parts
                .iter()
                .any(|part| !safe_hub_component(part))
        {
            bail!("managed GGUF repository must be an owner/name pair");
        }
        if self.revision.len() != 40 || !self.revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("managed GGUF revision must be an immutable 40-character commit");
        }
        if Path::new(&self.filename)
            .file_name()
            .and_then(|name| name.to_str())
            != Some(self.filename.as_str())
            || !self.filename.to_ascii_lowercase().ends_with(".gguf")
        {
            bail!("managed GGUF filename must be one repository-root .gguf file");
        }
        if self.expected_size == 0 {
            bail!("managed GGUF expected size must be non-zero");
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("managed GGUF SHA-256 must contain 64 hexadecimal characters");
        }
        Ok(())
    }
}

fn safe_hub_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Return the exact managed artifact for a supported family, size, and quantization.
pub fn managed_gguf_artifact(
    family: ModelFamily,
    size: ModelSize,
    quantization: GgufQuantization,
) -> Option<ManagedGgufArtifact> {
    let (repository, revision, q4, q5) = match (family, size) {
        (ModelFamily::Qwen2, ModelSize::Small) => (
            "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
            "91cad51170dc346986eccefdc2dd33a9da36ead9",
            (
                "qwen2.5-1.5b-instruct-q4_k_m.gguf",
                1_117_320_736,
                "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
            ),
            (
                "qwen2.5-1.5b-instruct-q5_k_m.gguf",
                1_285_494_304,
                "b46661073c18e5b56a41fa320975f866a00def1ff08feef4718e013258896f8c",
            ),
        ),
        (ModelFamily::Qwen2, ModelSize::Medium) => (
            "Qwen/Qwen2.5-3B-Instruct-GGUF",
            "7dabda4d13d513e3e842b20f0d435c732f172cbe",
            (
                "qwen2.5-3b-instruct-q4_k_m.gguf",
                2_104_932_768,
                "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d",
            ),
            (
                "qwen2.5-3b-instruct-q5_k_m.gguf",
                2_438_740_384,
                "2c63dde5f2c9ab1fd64d47dee2d34dade6ba9ff62442d1d20b5342310c982081",
            ),
        ),
        (ModelFamily::Gemma2, ModelSize::Small) => (
            "bartowski/gemma-2-2b-it-GGUF",
            "855f67caed130e1befc571b52bd181be2e858883",
            (
                "gemma-2-2b-it-Q4_K_M.gguf",
                1_708_582_752,
                "e0aee85060f168f0f2d8473d7ea41ce2f3230c1bc1374847505ea599288a7787",
            ),
            (
                "gemma-2-2b-it-Q5_K_M.gguf",
                1_923_278_688,
                "be65d5966f7efd40d9d8c9d5f6667582d861de52df20133991e2e9cb491150da",
            ),
        ),
        (ModelFamily::Gemma2, ModelSize::Medium) => (
            "bartowski/gemma-2-9b-it-GGUF",
            "d731033f3dc4018261fd39896e50984d398b4ac5",
            (
                "gemma-2-9b-it-Q4_K_M.gguf",
                5_761_057_728,
                "13b2a7b4115bbd0900162edcebe476da1ba1fc24e718e8b40d32f6e300f56dfe",
            ),
            (
                "gemma-2-9b-it-Q5_K_M.gguf",
                6_647_366_592,
                "a4b0b55ce809a09baaefb789b0046ac77ecd502aba8aeb2ed63cc237d9f40ce7",
            ),
        ),
        _ => return None,
    };
    let (filename, expected_size, sha256) = match quantization {
        GgufQuantization::Q4KM => q4,
        GgufQuantization::Q5KM => q5,
        // Not offered for chat models -- reserved for the fixed memory
        // embedding artifact (src/models/neural_embedding.rs), which is
        // constructed directly, not through this family/size picker.
        GgufQuantization::Q8_0 => return None,
    };
    Some(ManagedGgufArtifact {
        repository: repository.to_string(),
        revision: revision.to_string(),
        filename: filename.to_string(),
        quantization,
        expected_size,
        sha256: sha256.to_string(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DownloadDisposition {
    CacheHit,
    Downloaded,
}

/// Where `artifact` would land in the managed GGUF cache, without touching
/// the filesystem or the network -- the same `cache_dir/repo--revision/
/// filename` layout `ManagedGgufDownloader::ensure` commits a verified
/// download to. For a cheap "is this already usable" check that must not
/// probe the network or re-verify a checksum on every call (the ONNX
/// embedding engine this replaced had the same constraint on its own
/// cache lookup).
pub(crate) fn managed_gguf_cache_path(artifact: &ManagedGgufArtifact) -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()
        .context("Could not determine the user cache directory")?
        .join("finch")
        .join("gguf");
    Ok(cache_dir
        .join(artifact.repository.replace('/', "--"))
        .join(&artifact.revision)
        .join(&artifact.filename))
}

pub(crate) struct ManagedGgufDownloader {
    cache_dir: PathBuf,
    endpoint: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl ManagedGgufDownloader {
    pub(crate) fn from_environment(configured_token: Option<String>) -> Result<Self> {
        let hub_cache = Cache::from_env();
        let cache_dir = dirs::cache_dir()
            .context("Could not determine the user cache directory")?
            .join("finch")
            .join("gguf");
        Self::new(
            cache_dir,
            HUGGING_FACE_ENDPOINT.to_string(),
            configured_token.or_else(|| hub_cache.token()),
        )
    }

    fn new(cache_dir: PathBuf, endpoint: String, token: Option<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("finch/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .context("build managed GGUF HTTP client")?;
        Ok(Self {
            cache_dir,
            endpoint,
            token,
            client,
        })
    }

    pub(crate) async fn ensure(
        &self,
        artifact: &ManagedGgufArtifact,
        model_name: &str,
        state: Arc<RwLock<GeneratorState>>,
        cancellation: &CancellationToken,
    ) -> Result<(PathBuf, DownloadDisposition)> {
        artifact.validate()?;
        let artifact_dir = self
            .cache_dir
            .join(artifact.repository.replace('/', "--"))
            .join(&artifact.revision);
        tokio::fs::create_dir_all(&artifact_dir)
            .await
            .with_context(|| format!("create managed GGUF cache {}", artifact_dir.display()))?;
        let final_path = artifact_dir.join(&artifact.filename);
        let part_path = artifact_dir.join(format!("{}.part", artifact.filename));
        let marker_path = artifact_dir.join(format!("{}.verified", artifact.filename));
        let lock_path = artifact_dir.join(format!("{}.lock", artifact.filename));

        let lock = tokio::task::spawn_blocking(move || -> Result<std::fs::File> {
            use fs2::FileExt;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&lock_path)
                .with_context(|| format!("open managed GGUF lock {}", lock_path.display()))?;
            file.lock_exclusive()
                .with_context(|| format!("lock managed GGUF cache {}", lock_path.display()))?;
            Ok(file)
        })
        .await
        .context("managed GGUF lock task panicked")??;

        if verified_cache_hit(&final_path, &marker_path, artifact).await? {
            drop(lock);
            return Ok((final_path, DownloadDisposition::CacheHit));
        }
        remove_if_present(&final_path).await?;
        remove_if_present(&marker_path).await?;

        let mut resumed = tokio::fs::metadata(&part_path)
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if resumed > artifact.expected_size {
            remove_if_present(&part_path).await?;
            resumed = 0;
        }
        let remaining = artifact.expected_size.saturating_sub(resumed);
        let available = fs2::available_space(&artifact_dir)
            .with_context(|| format!("inspect free space for {}", artifact_dir.display()))?;
        if available < remaining {
            bail!(
                "not enough disk space for {}: need {} more bytes, only {} bytes available",
                artifact.filename,
                remaining,
                available
            );
        }

        set_download_state(
            &state,
            model_name,
            artifact,
            resumed,
            artifact.expected_size,
        )
        .await;
        let mut downloaded = resumed;
        if downloaded < artifact.expected_size {
            let url = self.artifact_url(artifact)?;
            let mut request = self.client.get(url);
            if resumed > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={resumed}-"));
            }
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
            let response = tokio::select! {
                _ = cancellation.cancelled() => bail!("managed GGUF download cancelled"),
                response = tokio::time::timeout(Duration::from_secs(60), request.send()) => {
                    response
                        .context("timed out waiting for Hugging Face GGUF response")?
                        .context("request managed GGUF artifact")?
                },
            };
            let status = response.status();
            if resumed > 0 && status == reqwest::StatusCode::OK {
                remove_if_present(&part_path).await?;
                resumed = 0;
                downloaded = 0;
                set_download_state(&state, model_name, artifact, 0, artifact.expected_size).await;
            } else if resumed > 0 && status != reqwest::StatusCode::PARTIAL_CONTENT {
                bail!("Hugging Face refused the resumable GGUF request with HTTP {status}");
            } else if resumed == 0 && !status.is_success() {
                bail!("Hugging Face GGUF request failed with HTTP {status}");
            }

            let mut output = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&part_path)
                .await
                .with_context(|| format!("open partial GGUF {}", part_path.display()))?;
            let mut stream = response.bytes_stream();
            let mut last_progress_update = tokio::time::Instant::now();
            loop {
                let next = tokio::select! {
                    _ = cancellation.cancelled() => bail!("managed GGUF download cancelled"),
                    next = tokio::time::timeout(Duration::from_secs(60), stream.next()) => {
                        next.context("managed GGUF transfer stalled for 60 seconds")?
                    },
                };
                let Some(chunk) = next else { break };
                let chunk = chunk.context("read managed GGUF response")?;
                output
                    .write_all(&chunk)
                    .await
                    .with_context(|| format!("write partial GGUF {}", part_path.display()))?;
                downloaded = downloaded.saturating_add(chunk.len() as u64);
                if downloaded > artifact.expected_size {
                    bail!("downloaded GGUF exceeded its pinned expected size");
                }
                if downloaded == artifact.expected_size
                    || last_progress_update.elapsed() >= Duration::from_millis(100)
                {
                    set_download_state(
                        &state,
                        model_name,
                        artifact,
                        downloaded,
                        artifact.expected_size,
                    )
                    .await;
                    last_progress_update = tokio::time::Instant::now();
                }
            }
            output
                .flush()
                .await
                .with_context(|| format!("flush partial GGUF {}", part_path.display()))?;
            output
                .sync_all()
                .await
                .with_context(|| format!("sync partial GGUF {}", part_path.display()))?;
        }
        if downloaded != artifact.expected_size {
            bail!(
                "downloaded GGUF size mismatch: expected {} bytes, received {}",
                artifact.expected_size,
                downloaded
            );
        }

        let digest = sha256_file(&part_path, cancellation).await?;
        if !digest.eq_ignore_ascii_case(&artifact.sha256) {
            remove_if_present(&part_path).await?;
            bail!(
                "downloaded GGUF checksum mismatch for {}; the corrupt partial file was removed",
                artifact.filename
            );
        }
        tokio::fs::rename(&part_path, &final_path)
            .await
            .with_context(|| format!("commit managed GGUF {}", final_path.display()))?;
        write_verification_marker(&marker_path, &final_path, artifact)
            .await
            .with_context(|| format!("record GGUF verification {}", marker_path.display()))?;
        drop(lock);
        Ok((final_path, DownloadDisposition::Downloaded))
    }

    fn artifact_url(&self, artifact: &ManagedGgufArtifact) -> Result<String> {
        let api = ApiBuilder::from_cache(Cache::new(self.cache_dir.clone()))
            .with_endpoint(self.endpoint.clone())
            .with_progress(false)
            .build()
            .context("build Hugging Face repository resolver")?;
        let repo = api.repo(Repo::with_revision(
            artifact.repository.clone(),
            RepoType::Model,
            artifact.revision.clone(),
        ));
        Ok(repo.url(&artifact.filename))
    }
}

async fn set_download_state(
    state: &Arc<RwLock<GeneratorState>>,
    model_name: &str,
    artifact: &ManagedGgufArtifact,
    downloaded_bytes: u64,
    total_bytes: u64,
) {
    *state.write().await = GeneratorState::Downloading {
        model_name: model_name.to_string(),
        progress: DownloadProgressSnapshot {
            file_name: artifact.filename.clone(),
            downloaded_bytes,
            total_bytes,
        },
    };
}

async fn verified_cache_hit(
    path: &Path,
    marker: &Path,
    artifact: &ManagedGgufArtifact,
) -> Result<bool> {
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return Ok(false);
    };
    if metadata.len() != artifact.expected_size {
        return Ok(false);
    }
    if let Ok(contents) = tokio::fs::read_to_string(marker).await {
        if let Ok(verification) = serde_json::from_str::<VerificationMarker>(&contents) {
            if verification.sha256.eq_ignore_ascii_case(&artifact.sha256)
                && verification.size == metadata.len()
                && verification.modified_nanos == modified_nanos(&metadata)?
            {
                return Ok(true);
            }
        }
    }
    let cancellation = CancellationToken::new();
    let digest = sha256_file(path, &cancellation).await?;
    if !digest.eq_ignore_ascii_case(&artifact.sha256) {
        return Ok(false);
    }
    write_verification_marker(marker, path, artifact).await?;
    Ok(true)
}

async fn write_verification_marker(
    marker: &Path,
    artifact_path: &Path,
    artifact: &ManagedGgufArtifact,
) -> Result<()> {
    let metadata = tokio::fs::metadata(artifact_path)
        .await
        .with_context(|| format!("inspect verified GGUF {}", artifact_path.display()))?;
    let verification = VerificationMarker {
        sha256: artifact.sha256.to_ascii_lowercase(),
        size: metadata.len(),
        modified_nanos: modified_nanos(&metadata)?,
    };
    let encoded = serde_json::to_vec(&verification).context("encode GGUF verification marker")?;
    tokio::fs::write(marker, encoded)
        .await
        .with_context(|| format!("record GGUF verification {}", marker.display()))
}

fn modified_nanos(metadata: &std::fs::Metadata) -> Result<u128> {
    Ok(metadata
        .modified()
        .context("read GGUF modification time")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("GGUF modification time predates Unix epoch")?
        .as_nanos())
}

async fn sha256_file(path: &Path, cancellation: &CancellationToken) -> Result<String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("open GGUF for checksum {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = tokio::select! {
            _ = cancellation.cancelled() => bail!("managed GGUF verification cancelled"),
            read = file.read(&mut buffer) => read.with_context(|| format!("read GGUF for checksum {}", path.display()))?,
        };
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

async fn remove_if_present(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{header, HeaderMap, Response, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    struct HubFixture {
        bytes: Arc<Vec<u8>>,
        requests: Arc<AtomicUsize>,
        ranges: Arc<Mutex<Vec<Option<String>>>>,
        authorizations: Arc<Mutex<Vec<Option<String>>>>,
        chunk_delay: Duration,
    }

    async fn hub_file(State(fixture): State<HubFixture>, headers: HeaderMap) -> Response<Body> {
        fixture.requests.fetch_add(1, Ordering::SeqCst);
        let range = headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        fixture.ranges.lock().unwrap().push(range.clone());
        fixture.authorizations.lock().unwrap().push(
            headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
        );
        let start = range
            .as_deref()
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.strip_suffix('-'))
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if start > fixture.bytes.len() {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .body(Body::empty())
                .unwrap();
        }
        let chunks = fixture.bytes[start..]
            .chunks(1024)
            .map(|chunk| chunk.to_vec())
            .collect::<Vec<_>>();
        let delay = fixture.chunk_delay;
        let body = Body::from_stream(futures::stream::unfold(
            (chunks.into_iter(), delay),
            |(mut chunks, delay)| async move {
                let chunk = chunks.next()?;
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                Some((Ok::<_, std::io::Error>(chunk), (chunks, delay)))
            },
        ));
        let mut response = Response::builder()
            .status(if start == 0 {
                StatusCode::OK
            } else {
                StatusCode::PARTIAL_CONTENT
            })
            .header(header::CONTENT_LENGTH, fixture.bytes.len() - start);
        if start > 0 {
            response = response.header(
                header::CONTENT_RANGE,
                format!(
                    "bytes {start}-{}/{}",
                    fixture.bytes.len() - 1,
                    fixture.bytes.len()
                ),
            );
        }
        response.body(body).unwrap()
    }

    async fn fake_hub(bytes: Vec<u8>, chunk_delay: Duration) -> (String, HubFixture) {
        let fixture = HubFixture {
            bytes: Arc::new(bytes),
            requests: Arc::new(AtomicUsize::new(0)),
            ranges: Arc::new(Mutex::new(Vec::new())),
            authorizations: Arc::new(Mutex::new(Vec::new())),
            chunk_delay,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .fallback(get(hub_file))
            .with_state(fixture.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), fixture)
    }

    fn test_artifact(bytes: &[u8]) -> ManagedGgufArtifact {
        ManagedGgufArtifact {
            repository: "test/model".to_string(),
            revision: "a".repeat(40),
            filename: "model-q4_k_m.gguf".to_string(),
            quantization: GgufQuantization::Q4KM,
            expected_size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        }
    }

    fn artifact_dir(cache: &Path, artifact: &ManagedGgufArtifact) -> PathBuf {
        cache
            .join(artifact.repository.replace('/', "--"))
            .join(&artifact.revision)
    }

    #[test]
    fn managed_catalog_is_explicit_and_rejects_unsupported_combinations() {
        let artifact = managed_gguf_artifact(
            ModelFamily::Qwen2,
            ModelSize::Medium,
            GgufQuantization::Q4KM,
        )
        .expect("Qwen 3B Q4_K_M must be managed");
        artifact.validate().expect("catalog artifact must validate");
        assert_eq!(artifact.repository, "Qwen/Qwen2.5-3B-Instruct-GGUF");
        assert_eq!(artifact.revision.len(), 40);
        assert_eq!(artifact.sha256.len(), 64);
        assert!(managed_gguf_artifact(
            ModelFamily::Llama3,
            ModelSize::Large,
            GgufQuantization::Q4KM,
        )
        .is_none());
    }

    #[test]
    fn edited_artifact_cannot_escape_the_cache() {
        let mut artifact =
            managed_gguf_artifact(ModelFamily::Qwen2, ModelSize::Small, GgufQuantization::Q4KM)
                .unwrap();
        artifact.filename = "../../outside.gguf".to_string();
        assert!(artifact.validate().is_err());
        artifact.filename = "model.gguf".to_string();
        artifact.repository = "../outside".to_string();
        assert!(artifact.validate().is_err());
    }

    #[tokio::test]
    async fn first_download_reports_bytes_and_offline_cache_hit_does_not_request_again() {
        let bytes = b"a small deterministic GGUF fixture".repeat(256);
        let artifact = test_artifact(&bytes);
        let (endpoint, fixture) = fake_hub(bytes.clone(), Duration::ZERO).await;
        let cache = tempfile::tempdir().unwrap();
        let downloader = ManagedGgufDownloader::new(
            cache.path().into(),
            endpoint,
            Some("configured-hf-token".into()),
        )
        .unwrap();
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let cancellation = CancellationToken::new();

        let (path, disposition) = downloader
            .ensure(&artifact, "Test model", Arc::clone(&state), &cancellation)
            .await
            .unwrap();
        assert_eq!(disposition, DownloadDisposition::Downloaded);
        assert_eq!(tokio::fs::read(&path).await.unwrap(), bytes);
        assert!(matches!(
            &*state.read().await,
            GeneratorState::Downloading { progress, .. }
                if progress.downloaded_bytes == artifact.expected_size
                    && progress.total_bytes == artifact.expected_size
        ));
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.authorizations.lock().unwrap().as_slice(),
            [Some("Bearer configured-hf-token".to_string())]
        );

        *state.write().await = GeneratorState::Initializing;
        let (_, disposition) = downloader
            .ensure(&artifact, "Test model", Arc::clone(&state), &cancellation)
            .await
            .unwrap();
        assert_eq!(disposition, DownloadDisposition::CacheHit);
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 1);
        assert!(matches!(&*state.read().await, GeneratorState::Initializing));
    }

    #[tokio::test]
    async fn interrupted_partial_download_resumes_with_a_range_request() {
        let bytes = b"resume this GGUF payload".repeat(512);
        let artifact = test_artifact(&bytes);
        let (endpoint, fixture) = fake_hub(bytes.clone(), Duration::ZERO).await;
        let cache = tempfile::tempdir().unwrap();
        let dir = artifact_dir(cache.path(), &artifact);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let resume_at = bytes.len() / 3;
        tokio::fs::write(
            dir.join(format!("{}.part", artifact.filename)),
            &bytes[..resume_at],
        )
        .await
        .unwrap();
        let downloader = ManagedGgufDownloader::new(cache.path().into(), endpoint, None).unwrap();
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let (path, disposition) = downloader
            .ensure(&artifact, "Test model", state, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(disposition, DownloadDisposition::Downloaded);
        assert_eq!(tokio::fs::read(path).await.unwrap(), bytes);
        assert_eq!(
            fixture.ranges.lock().unwrap().as_slice(),
            [Some(format!("bytes={resume_at}-"))]
        );
    }

    #[tokio::test]
    async fn complete_partial_is_verified_and_committed_without_an_eof_range_request() {
        let bytes = b"complete partial from interrupted commit".repeat(256);
        let artifact = test_artifact(&bytes);
        let (endpoint, fixture) = fake_hub(bytes.clone(), Duration::ZERO).await;
        let cache = tempfile::tempdir().unwrap();
        let dir = artifact_dir(cache.path(), &artifact);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(dir.join(format!("{}.part", artifact.filename)), &bytes)
            .await
            .unwrap();
        let downloader = ManagedGgufDownloader::new(cache.path().into(), endpoint, None).unwrap();

        let (path, disposition) = downloader
            .ensure(
                &artifact,
                "Test model",
                Arc::new(RwLock::new(GeneratorState::Initializing)),
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(disposition, DownloadDisposition::Downloaded);
        assert_eq!(tokio::fs::read(path).await.unwrap(), bytes);
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn stale_verification_marker_cannot_hide_same_size_cache_corruption() {
        let bytes = b"cache integrity fixture".repeat(256);
        let artifact = test_artifact(&bytes);
        let (endpoint, fixture) = fake_hub(bytes.clone(), Duration::ZERO).await;
        let cache = tempfile::tempdir().unwrap();
        let downloader = ManagedGgufDownloader::new(cache.path().into(), endpoint, None).unwrap();
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let (path, _) = downloader
            .ensure(
                &artifact,
                "Test model",
                Arc::clone(&state),
                &CancellationToken::new(),
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(2)).await;
        tokio::fs::write(&path, vec![0_u8; bytes.len()])
            .await
            .unwrap();
        let (_, disposition) = downloader
            .ensure(&artifact, "Test model", state, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(disposition, DownloadDisposition::Downloaded);
        assert_eq!(tokio::fs::read(path).await.unwrap(), bytes);
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn checksum_rejection_removes_corrupt_partial_and_allows_clean_retry() {
        let bytes = b"checksum fixture".repeat(256);
        let mut wrong = test_artifact(&bytes);
        wrong.sha256 = "0".repeat(64);
        let (endpoint, fixture) = fake_hub(bytes.clone(), Duration::ZERO).await;
        let cache = tempfile::tempdir().unwrap();
        let downloader = ManagedGgufDownloader::new(cache.path().into(), endpoint, None).unwrap();
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let error = downloader
            .ensure(
                &wrong,
                "Test model",
                Arc::clone(&state),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        let part = artifact_dir(cache.path(), &wrong).join(format!("{}.part", wrong.filename));
        assert!(
            !part.exists(),
            "a checksum-rejected partial must be removed"
        );

        let correct = test_artifact(&bytes);
        downloader
            .ensure(&correct, "Test model", state, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(fixture.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancellation_keeps_a_resumable_partial_and_never_commits_a_final_file() {
        let bytes = vec![7_u8; 128 * 1024];
        let artifact = test_artifact(&bytes);
        let (endpoint, _) = fake_hub(bytes, Duration::from_millis(5)).await;
        let cache = tempfile::tempdir().unwrap();
        let downloader =
            Arc::new(ManagedGgufDownloader::new(cache.path().into(), endpoint, None).unwrap());
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let cancellation = CancellationToken::new();
        let task = {
            let downloader = Arc::clone(&downloader);
            let state = Arc::clone(&state);
            let artifact = artifact.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                downloader
                    .ensure(&artifact, "Test model", state, &cancellation)
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    &*state.read().await,
                    GeneratorState::Downloading { progress, .. } if progress.downloaded_bytes > 0
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fake Hub must deliver at least one chunk");
        cancellation.cancel();
        let error = task.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        let dir = artifact_dir(cache.path(), &artifact);
        assert!(dir.join(format!("{}.part", artifact.filename)).exists());
        assert!(!dir.join(&artifact.filename).exists());
    }

    #[tokio::test]
    #[ignore = "downloads the pinned 1.5B Qwen GGUF from Hugging Face"]
    async fn real_hugging_face_pinned_artifact_smoke() {
        let artifact =
            managed_gguf_artifact(ModelFamily::Qwen2, ModelSize::Small, GgufQuantization::Q4KM)
                .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let downloader = ManagedGgufDownloader::new(
            cache.path().into(),
            HUGGING_FACE_ENDPOINT.to_string(),
            Cache::from_env().token(),
        )
        .unwrap();
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));

        let (path, _) = downloader
            .ensure(&artifact, "Qwen 2.5 1.5B", state, &CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            tokio::fs::metadata(path).await.unwrap().len(),
            artifact.expected_size
        );
    }
}
