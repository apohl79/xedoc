//! Bounded worker for synchronous embedders.

use std::fs;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread;

use fastembed::Pooling;
use fastembed::TextEmbedding;
use fastembed::TokenizerFiles;
use fastembed::UserDefinedEmbeddingModel;

use crate::LocalArtifact;

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("embedding worker queue is full")]
    QueueFull,
    #[error("embedding worker stopped")]
    WorkerStopped,
    #[error("could not load checked local embedding files")]
    LocalArtifact(#[source] std::io::Error),
    #[error("could not initialize local embedding model")]
    Initialization(#[source] fastembed::Error),
    #[error("could not initialize bundled ONNX Runtime: {0}")]
    NativeRuntime(String),
    #[error("local embedding model did not return an embedding")]
    EmptyEmbedding,
}

pub trait TaskEmbedder: Send + Sync {
    fn embed(&self, prompt: &str) -> Result<Vec<f32>, EmbedError>;
}

pub trait SyncEmbedder: Send + 'static {
    fn embed_sync(&self, prompt: &str) -> Result<Vec<f32>, EmbedError>;
}

/// Synchronous FastEmbed adapter backed exclusively by a checked local artifact.
pub struct FastEmbedder {
    model: Mutex<TextEmbedding>,
    dimensions: usize,
}

impl FastEmbedder {
    /// Loads the bundled model and tokenizer files after artifact verification.
    ///
    /// # Errors
    ///
    /// Returns an error when checked local files cannot be read or FastEmbed
    /// cannot initialize a local ONNX session.
    pub fn from_local_artifact(artifact: &LocalArtifact) -> Result<Self, EmbedError> {
        initialize_native_runtime(artifact)?;
        let tokenizer_files = TokenizerFiles {
            tokenizer_file: read_artifact_file(artifact, "tokenizer.json")?,
            config_file: read_artifact_file(artifact, "config.json")?,
            special_tokens_map_file: read_artifact_file(artifact, "special_tokens_map.json")?,
            tokenizer_config_file: read_artifact_file(artifact, "tokenizer_config.json")?,
        };
        let model = UserDefinedEmbeddingModel::new(
            read_artifact_file(artifact, &artifact.descriptor.file)?,
            tokenizer_files,
        )
        .with_pooling(Pooling::Cls);
        let model = TextEmbedding::try_new_from_user_defined(model, Default::default())
            .map_err(EmbedError::Initialization)?;
        Ok(Self {
            model: Mutex::new(model),
            dimensions: artifact.descriptor.dimensions,
        })
    }
}

impl SyncEmbedder for FastEmbedder {
    fn embed_sync(&self, prompt: &str) -> Result<Vec<f32>, EmbedError> {
        let mut model = self.model.lock().map_err(|_| EmbedError::WorkerStopped)?;
        let embedding = model
            .embed([prompt], None)
            .map_err(EmbedError::Initialization)?
            .pop()
            .ok_or(EmbedError::EmptyEmbedding)?;
        (embedding.len() == self.dimensions)
            .then_some(embedding)
            .ok_or(EmbedError::EmptyEmbedding)
    }
}

fn initialize_native_runtime(artifact: &LocalArtifact) -> Result<(), EmbedError> {
    static NATIVE_RUNTIME: OnceLock<Result<(), String>> = OnceLock::new();
    let library_path = artifact
        .native_runtime_library()
        .map_err(|error| EmbedError::NativeRuntime(error.to_string()))?;
    NATIVE_RUNTIME
        .get_or_init(|| {
            ort::init_from(library_path)
                .map(|environment| {
                    environment.commit();
                })
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map(|_| ())
        .map_err(|error| EmbedError::NativeRuntime(error.clone()))
}

fn read_artifact_file(artifact: &LocalArtifact, name: &str) -> Result<Vec<u8>, EmbedError> {
    fs::read(artifact.root.join(name)).map_err(EmbedError::LocalArtifact)
}

struct Request {
    prompt: String,
    response: mpsc::SyncSender<Result<Vec<f32>, EmbedError>>,
}

/// A dedicated, bounded thread for a supplied synchronous embedder.
pub struct BoundedEmbedder {
    requests: mpsc::SyncSender<Request>,
}

impl BoundedEmbedder {
    pub fn new<E>(embedder: E, capacity: usize) -> Self
    where
        E: SyncEmbedder,
    {
        let (requests, receiver) = mpsc::sync_channel::<Request>(capacity.max(1));
        thread::spawn(move || {
            while let Ok(request) = receiver.recv() {
                let result = embedder.embed_sync(&request.prompt);
                let _ = request.response.send(result);
            }
        });
        Self { requests }
    }
}

impl TaskEmbedder for BoundedEmbedder {
    fn embed(&self, prompt: &str) -> Result<Vec<f32>, EmbedError> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.requests
            .try_send(Request {
                prompt: prompt.to_owned(),
                response,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => EmbedError::QueueFull,
                mpsc::TrySendError::Disconnected(_) => EmbedError::WorkerStopped,
            })?;
        receiver.recv().map_err(|_| EmbedError::WorkerStopped)?
    }
}
