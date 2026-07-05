use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::embedder::{Embedder, EmbedderError, EmbedderResult};
use frankensearch::ModelCategory;
use ort::session::Session;
use ort::value::Tensor;
use tokenizers::Tokenizer;

const EMBEDDER_ID: &str = "jina-v2-small-512";
const MODEL_NAME: &str = "jina-embeddings-v2-small-en";
const EXPECTED_DIMENSION: usize = 512;
const ONNX_FILE: &str = "model_q4.onnx";
const TOKENIZER_FILE: &str = "tokenizer.json";
const ONNX_INTRA_THREADS: usize = 4;

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

    pub fn load(data_dir: &Path) -> EmbedderResult<Self> {
        let model_dir = Self::model_dir(data_dir);
        let model_path = model_dir.join(ONNX_FILE);
        let tokenizer_path = model_dir.join(TOKENIZER_FILE);

        if !model_path.is_file() {
            return Err(EmbedderError::EmbedderUnavailable {
                model: MODEL_NAME.to_string(),
                reason: format!(
                    "ONNX model not found at {}. Download model_q4.onnx and tokenizer.json from huggingface.co/xenova/jina-embeddings-v2-small-en",
                    model_path.display()
                ),
            });
        }
        if !tokenizer_path.is_file() {
            return Err(EmbedderError::EmbedderUnavailable {
                model: MODEL_NAME.to_string(),
                reason: format!("tokenizer not found at {}", tokenizer_path.display()),
            });
        }

        let session = Session::builder()
            .map_err(|e| embedding_failed(&format!("session builder: {e}")))?
            .with_intra_threads(ONNX_INTRA_THREADS)
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

    fn embed_one(&self, text: &str) -> EmbedderResult<Vec<f32>> {
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
        let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();

        let seq_len = input_ids.len();

        let ids_tensor = Tensor::from_array((vec![1i64, seq_len as i64], input_ids))
            .map_err(|e| embedding_failed(&format!("input_ids tensor: {e}")))?;
        let mask_tensor = Tensor::from_array((vec![1i64, seq_len as i64], attention_mask.clone()))
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

impl Embedder for JinaEmbedder {
    fn embed_sync(&self, text: &str) -> EmbedderResult<Vec<f32>> {
        if text.is_empty() {
            return Err(EmbedderError::EmbeddingFailed {
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

fn embedding_failed(msg: &str) -> EmbedderError {
    EmbedderError::EmbeddingFailed {
        model: MODEL_NAME.to_string(),
        source: Box::new(std::io::Error::other(msg.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const EPSILON: f32 = 1e-4;

    #[test]
    fn all_ones_mask_averages_every_token() {
        // 2 tokens x 3 dims: token0 = [1,2,3], token1 = [3,4,5].
        let data = vec![1.0, 2.0, 3.0, 3.0, 4.0, 5.0];
        let mask = vec![1i64, 1];
        let result = mean_pool_and_normalize(&data, 2, 3, &mask);

        // Mean before normalization is [2.0, 3.0, 4.0]; norm = sqrt(29).
        let norm = 29f32.sqrt();
        let expected = [2.0 / norm, 3.0 / norm, 4.0 / norm];
        assert_eq!(result.len(), 3);
        for (got, want) in result.iter().zip(expected.iter()) {
            assert!((got - want).abs() < EPSILON, "got {got}, want {want}");
        }
    }

    #[test]
    fn partial_mask_ignores_masked_tokens() {
        // 2 tokens x 2 dims: token0 = [1,2] (kept), token1 = [100,200] (masked out).
        // If the mask were ignored, the huge token1 values would dominate the mean.
        let data = vec![1.0, 2.0, 100.0, 200.0];
        let mask = vec![1i64, 0];
        let result = mean_pool_and_normalize(&data, 2, 2, &mask);

        let norm = 5f32.sqrt();
        let expected = [1.0 / norm, 2.0 / norm];
        assert_eq!(result.len(), 2);
        for (got, want) in result.iter().zip(expected.iter()) {
            assert!((got - want).abs() < EPSILON, "got {got}, want {want}");
        }
    }

    #[test]
    fn all_zero_mask_returns_zero_vector() {
        let data = vec![1.0, 2.0, 3.0, 4.0];
        let mask = vec![0i64, 0];
        let result = mean_pool_and_normalize(&data, 2, 2, &mask);

        assert_eq!(result, vec![0.0, 0.0]);
    }

    #[test]
    fn output_is_unit_length() {
        let data = vec![0.5, -1.5, 2.25, -3.0, 4.0, 0.0];
        let mask = vec![1i64, 1];
        let result = mean_pool_and_normalize(&data, 2, 3, &mask);

        let norm: f32 = result.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < EPSILON,
            "expected unit norm, got {norm}"
        );
    }

    // Points at a cass data directory containing
    // models/jina-embeddings-v2-small-en/{model_q4.onnx,tokenizer.json} — mirrors the
    // FRANKENSEARCH_MODEL_DIR fixture pattern used for FastEmbedder in
    // fastembed_embedder.rs / embedder.rs. No fixture is committed for the Jina ONNX
    // model (it's a real download, not a small test asset), so this test is
    // `#[ignore]`d by default and run against a real model via
    // `cargo test -- --ignored` with `CASS_JINA_MODEL_DIR` set.
    fn jina_data_dir() -> Option<PathBuf> {
        dotenvy::var("CASS_JINA_MODEL_DIR")
            .ok()
            .map(|raw| raw.trim().to_string())
            .filter(|raw| !raw.is_empty())
            .map(PathBuf::from)
    }

    #[test]
    #[ignore = "needs a real jina-embeddings-v2-small-en ONNX bundle via CASS_JINA_MODEL_DIR"]
    fn test_jina_embedder_load_and_embed() {
        let data_dir = jina_data_dir().expect("CASS_JINA_MODEL_DIR must be set for this test");
        let embedder = JinaEmbedder::load(&data_dir).expect("jina model should load");

        assert_eq!(embedder.dimension(), EXPECTED_DIMENSION);
        assert_eq!(embedder.id(), EMBEDDER_ID);
        assert!(embedder.is_semantic());

        let embedding = embedder
            .embed_sync("hello world")
            .expect("embed should succeed");
        assert_eq!(embedding.len(), EXPECTED_DIMENSION);

        let norm: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "expected unit norm, got {norm}");
    }
}
