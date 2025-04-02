use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct CacheQuery {
    pub action: String,
    pub text: String,
    pub params: Option<Value>,
    pub embedding: Vec<f32>,
    pub results: Value,
}

pub struct Similarity {
    pub query: CacheQuery,
    pub score: f32,
}

#[async_trait]
pub trait SimilarityCache: Send + Sync {
    async fn store(&self, query: CacheQuery) -> Result<()>;
    async fn similarities(&self, query: CacheQuery) -> Result<Vec<Similarity>>;
}

pub struct PassthroughSimilarityCache;

impl PassthroughSimilarityCache {
    pub fn new() -> Self {
        PassthroughSimilarityCache
    }
}

#[async_trait]
impl SimilarityCache for PassthroughSimilarityCache {
    async fn store(&self, _query: CacheQuery) -> Result<()> {
        Ok(())
    }

    async fn similarities(&self, _query: CacheQuery) -> Result<Vec<Similarity>> {
        Ok(vec![])
    }
}
