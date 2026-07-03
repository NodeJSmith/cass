use std::path::{Path, PathBuf};
use std::sync::Mutex;

use frankensearch::{ModelCategory, SearchError, SearchResult, SyncEmbed};
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

const EMBEDDER_ID: &str = "jina-v2-small-512";
const MODEL_NAME: &str = "jina-embeddings-v2-small-en";
const EXPECTED_DIMENSION: usize = 512;
const ONNX_FILE: &str = "model_q4.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";

pub struct JinaEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    dimension: usize,
    id: String,
}

impl JinaEmbedder {
    pub fn embedder_id_static() -> &'static str {
        EMBEDDER_ID
    }

    pub fn model_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("models").join(MODEL_NAME)
    }

    pub fn load(data_dir: &Path) -> SearchResult<Self> {
        let model_dir = Self::model_dir(data_dir);
        let model_path = model_dir.join(ONNX_FILE);
        let tokenizer_path = model_dir.join(TOKENIZER_FILE);

        if !model_path.is_file() {
            return Err(SearchError::EmbedderUnavailable {
                model: MODEL_NAME.to_string(),
                reason: format!(
                    "ONNX model not found at {}. Download model_q4.onnx and tokenizer.json from huggingface.co/xenova/jina-embeddings-v2-small-en",
                    model_path.display()
                ),
            });
        }
        if !tokenizer_path.is_file() {
            return Err(SearchError::EmbedderUnavailable {
                model: MODEL_NAME.to_string(),
                reason: format!(
                    "tokenizer not found at {}",
                    tokenizer_path.display()
                ),
            });
        }

        let session = Session::builder()
            .map_err(|e| embedding_failed(&format!("session builder: {e}")))?
            .with_intra_threads(4)
            .map_err(|e| embedding_failed(&format!("intra threads: {e}")))?
            .commit_from_file(&model_path)
            .map_err(|e| embedding_failed(&format!("load model: {e}")))?;

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| embedding_failed(&format!("load tokenizer: {e}")))?;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            dimension: EXPECTED_DIMENSION,
            id: EMBEDDER_ID.to_string(),
        })
    }

    fn embed_one(&self, text: &str) -> SearchResult<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| embedding_failed(&format!("tokenize: {e}")))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&m| m as i64)
            .collect();
        let token_type_ids: Vec<i64> = encoding
            .get_type_ids()
            .iter()
            .map(|&t| t as i64)
            .collect();

        let seq_len = input_ids.len();

        let ids_tensor = Tensor::from_array((vec![1i64, seq_len as i64], input_ids))
            .map_err(|e| embedding_failed(&format!("input_ids tensor: {e}")))?;
        let mask_tensor =
            Tensor::from_array((vec![1i64, seq_len as i64], attention_mask.clone()))
                .map_err(|e| embedding_failed(&format!("attention_mask tensor: {e}")))?;
        let type_tensor = Tensor::from_array((vec![1i64, seq_len as i64], token_type_ids))
            .map_err(|e| embedding_failed(&format!("token_type_ids tensor: {e}")))?;

        let mut session = self
            .session
            .lock()
            .map_err(|e| embedding_failed(&format!("session lock: {e}")))?;

        let outputs = session
            .run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
                "token_type_ids" => type_tensor,
            ])
            .map_err(|e| embedding_failed(&format!("inference: {e}")))?;

        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| embedding_failed(&format!("extract tensor: {e}")))?;

        // Output shape: [1, seq_len, hidden_dim]
        if shape.len() != 3 || shape[0] != 1 {
            return Err(embedding_failed(&format!(
                "unexpected output shape: expected [1, seq_len, hidden_dim], got {shape:?}"
            )));
        }
        let hidden_dim = shape[2] as usize;
        if hidden_dim != self.dimension {
            return Err(embedding_failed(&format!(
                "dimension mismatch: model output {hidden_dim}, expected {}",
                self.dimension
            )));
        }

        let embedding = mean_pool_and_normalize(data, seq_len, hidden_dim, &attention_mask);
        Ok(embedding)
    }
}

impl SyncEmbed for JinaEmbedder {
    fn embed_sync(&self, text: &str) -> SearchResult<Vec<f32>> {
        if text.is_empty() {
            return Err(SearchError::EmbeddingFailed {
                model: MODEL_NAME.to_string(),
                source: Box::new(std::io::Error::other("empty input")),
            });
        }
        self.embed_one(text)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn model_name(&self) -> &str {
        MODEL_NAME
    }

    fn is_semantic(&self) -> bool {
        true
    }

    fn category(&self) -> ModelCategory {
        ModelCategory::TransformerEmbedder
    }
}

fn mean_pool_and_normalize(
    data: &[f32],
    seq_len: usize,
    hidden_dim: usize,
    attention_mask: &[i64],
) -> Vec<f32> {
    let mut pooled = vec![0.0f32; hidden_dim];
    let mut count = 0.0f32;

    for (token_idx, &mask_val) in attention_mask.iter().enumerate() {
        if mask_val > 0 && token_idx < seq_len {
            let offset = token_idx * hidden_dim;
            for j in 0..hidden_dim {
                pooled[j] += data[offset + j];
            }
            count += 1.0;
        }
    }

    if count > 0.0 {
        for val in &mut pooled {
            *val /= count;
        }
    }

    let norm: f32 = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut pooled {
            *val /= norm;
        }
    }

    pooled
}

fn embedding_failed(msg: &str) -> SearchError {
    SearchError::EmbeddingFailed {
        model: MODEL_NAME.to_string(),
        source: Box::new(std::io::Error::other(msg.to_string())),
    }
}
