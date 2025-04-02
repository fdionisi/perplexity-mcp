use std::{env, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use context_server::{Tool, ToolContent, ToolExecutor};
use http_client::{HttpClient, Request, RequestBuilderExt, ResponseAsyncBodyExt};
use indoc::formatdoc;
use serde_json::{Value, json};
use similarity_cache::{CacheQuery, PassthroughSimilarityCache, SimilarityCache};
use usage_reporter::{NoopUsageReporter, Usage, UsageReport, UsageReporter};

fn format_response_with_references(response_body: &Value) -> Result<String> {
    log::debug!("Formatting response with references");
    let content = response_body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("Failed to extract content from response"))?
        .to_string();

    if let Some(citations) = response_body.get("citations").and_then(|c| c.as_array()) {
        if !citations.is_empty() {
            log::info!("Found {} citations", citations.len());
            let references = citations
                .iter()
                .enumerate()
                .map(|(i, citation)| {
                    format!(
                        "[{}]: {}",
                        i + 1,
                        citation.as_str().unwrap_or("Unknown URL")
                    )
                })
                .collect::<Vec<String>>()
                .join("\n");

            return Ok(format!("{}\n\nReferences:\n{}", content, references));
        }
    }

    log::info!("No citations found in response");
    Ok(content)
}

async fn call_perplexity_api(
    http_client: &Arc<dyn HttpClient>,
    similarity_cache: &Arc<dyn SimilarityCache>,
    model: &str,
    messages: Value,
    search_recency_filter: Option<&str>,
) -> Result<Value> {
    log::debug!("Calling Perplexity API with model: {}", model);

    // Create a Query object for similarity cache
    let query_embedding = vec![0.0; 1]; // Placeholder for actual embedding computation
    let query = CacheQuery {
        action: "perplexity_api_call".to_string(),
        text: format!("{:?}", messages),
        params: Some(json!({
            "model": model,
            "search_recency_filter": search_recency_filter
        })),
        embedding: query_embedding,
        results: Value::Null,
    };

    // Check similarity cache for existing results
    let similarities = similarity_cache.similarities(query.clone()).await?;
    if let Some(similar_query) = similarities.first() {
        if similar_query.score > 0.95 {
            // High similarity threshold
            log::info!(
                "Found cached similar response with score: {}",
                similar_query.score
            );
            return Ok(similar_query.query.results.clone());
        }
    }

    let api_key = env::var("PERPLEXITY_API_KEY").map_err(|_| {
        log::error!("PERPLEXITY_API_KEY not set in environment");
        anyhow!("PERPLEXITY_API_KEY not set in environment")
    })?;

    let mut request_body = json!({
        "model": model,
        "messages": messages
    });

    if let Some(filter) = search_recency_filter {
        log::info!("Applying search recency filter: {}", filter);
        request_body["search_recency_filter"] = json!(filter);
    }

    let response = http_client
        .send(
            Request::builder()
                .method("POST")
                .uri("https://api.perplexity.ai/chat/completions")
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(request_body)?,
        )
        .await?;

    let response_json: Value = response.json().await.map_err(|err| {
        log::error!("Failed to parse API response: {}", err);
        anyhow!("{}", err.to_string())
    })?;

    // Store the result in the similarity cache
    let mut cached_query = query.clone();
    cached_query.results = response_json.clone();
    let _ = similarity_cache.store(cached_query).await;

    Ok(response_json)
}

pub struct SearchTool {
    http_client: Arc<dyn HttpClient>,
    usage_reporter: Arc<dyn UsageReporter>,
    similarity_cache: Arc<dyn SimilarityCache>,
}

impl SearchTool {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        usage_reporter: Option<Arc<dyn UsageReporter>>,
        similarity_cache: Option<Arc<dyn SimilarityCache>>,
    ) -> Self {
        Self {
            http_client,
            usage_reporter: usage_reporter.unwrap_or_else(|| Arc::new(NoopUsageReporter)),
            similarity_cache: similarity_cache
                .unwrap_or_else(|| Arc::new(PassthroughSimilarityCache)),
        }
    }
}

#[async_trait]
impl ToolExecutor for SearchTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        log::debug!("Executing SearchTool");
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid query"))?;

        let detail_level = args
            .get("detail_level")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");

        let search_recency_filter = args.get("search_recency_filter").and_then(|v| v.as_str());

        let prompt = match detail_level {
            "brief" => format!("Provide a brief, concise answer to: {}", query),
            "detailed" => format!(
                "Provide a comprehensive, detailed analysis of: {}. Include relevant examples, context, and supporting information where applicable.",
                query
            ),
            _ => format!(
                "Provide a clear, balanced answer to: {}. Include key points and relevant context.",
                query
            ),
        };

        log::info!("Prepared search prompt with detail level: {}", detail_level);

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body = call_perplexity_api(
            &self.http_client,
            &self.similarity_cache,
            "sonar-reasoning-pro",
            messages,
            search_recency_filter,
        )
        .await?;

        // Report usage if available
        if let (Some(usage), Some(model)) = (
            response_body.get("usage"),
            response_body.get("model").and_then(|m| m.as_str()),
        ) {
            if let (Some(completion_tokens), Some(prompt_tokens), Some(total_tokens)) = (
                usage.get("completion_tokens").and_then(|t| t.as_u64()),
                usage.get("prompt_tokens").and_then(|t| t.as_u64()),
                usage.get("total_tokens").and_then(|t| t.as_u64()),
            ) {
                let _ = self.usage_reporter.report(UsageReport {
                    model: model.to_string(),
                    usage: Usage {
                        completion_tokens,
                        prompt_tokens,
                        total_tokens,
                    },
                });
            }
        }

        let content = format_response_with_references(&response_body)?;

        Ok(vec![ToolContent::Text { text: content }])
    }

    fn to_tool(&self) -> Tool {
        Tool {
            name: "search".into(),
            description: Some(
                "Perform a general search query to get comprehensive information on any topic"
                    .into(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query or question"
                    },
                    "detail_level": {
                        "type": "string",
                        "description": "Optional: Desired level of detail (brief, normal, detailed)",
                        "enum": ["brief", "normal", "detailed"]
                    },
                    "search_recency_filter": {
                        "type": "string",
                        "description": "Optional: Filter for search results recency (month, week, day, hour)",
                        "enum": ["month", "week", "day", "hour"]
                    }
                },
                "required": ["query"]
            }),
        }
    }
}

pub struct GetDocumentationTool {
    http_client: Arc<dyn HttpClient>,
    usage_reporter: Arc<dyn UsageReporter>,
    similarity_cache: Arc<dyn SimilarityCache>,
}

impl GetDocumentationTool {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        usage_reporter: Option<Arc<dyn UsageReporter>>,
        similarity_cache: Option<Arc<dyn SimilarityCache>>,
    ) -> Self {
        Self {
            http_client,
            usage_reporter: usage_reporter.unwrap_or_else(|| Arc::new(NoopUsageReporter)),
            similarity_cache: similarity_cache
                .unwrap_or_else(|| Arc::new(PassthroughSimilarityCache)),
        }
    }
}

#[async_trait]
impl ToolExecutor for GetDocumentationTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        log::debug!("Executing GetDocumentationTool");
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid query"))?;

        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        let prompt = formatdoc!(
            "Provide comprehensive documentation and usage examples for {}. {} Include:
            1. Basic overview and purpose
            2. Key features and capabilities
            3. Installation/setup if applicable
            4. Common usage examples
            5. Best practices
            6. Common pitfalls to avoid
            7. Links to official documentation if available",
            query,
            if !context.is_empty() {
                format!("Focus on: {}. ", context)
            } else {
                String::new()
            }
        );

        log::info!("Prepared documentation prompt for: {}", query);

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body = call_perplexity_api(
            &self.http_client,
            &self.similarity_cache,
            "sonar-reasoning-pro",
            messages,
            None,
        )
        .await?;

        // Report usage if available
        if let (Some(usage), Some(model)) = (
            response_body.get("usage"),
            response_body.get("model").and_then(|m| m.as_str()),
        ) {
            if let (Some(completion_tokens), Some(prompt_tokens), Some(total_tokens)) = (
                usage.get("completion_tokens").and_then(|t| t.as_u64()),
                usage.get("prompt_tokens").and_then(|t| t.as_u64()),
                usage.get("total_tokens").and_then(|t| t.as_u64()),
            ) {
                let _ = self.usage_reporter.report(UsageReport {
                    model: model.to_string(),
                    usage: Usage {
                        completion_tokens,
                        prompt_tokens,
                        total_tokens,
                    },
                });
            }
        }

        let content = format_response_with_references(&response_body)?;

        Ok(vec![ToolContent::Text { text: content }])
    }

    fn to_tool(&self) -> Tool {
        Tool {
            name: "get_documentation".into(),
            description: Some(
                "Get documentation and usage examples for a specific technology, library, or API"
                    .into(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The technology, library, or API to get documentation for"
                    },
                    "context": {
                        "type": "string",
                        "description": "Additional context or specific aspects to focus on"
                    }
                },
                "required": ["query"]
            }),
        }
    }
}

pub struct FindApisTool {
    http_client: Arc<dyn HttpClient>,
    usage_reporter: Arc<dyn UsageReporter>,
    similarity_cache: Arc<dyn SimilarityCache>,
}

impl FindApisTool {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        usage_reporter: Option<Arc<dyn UsageReporter>>,
        similarity_cache: Option<Arc<dyn SimilarityCache>>,
    ) -> Self {
        Self {
            http_client,
            usage_reporter: usage_reporter.unwrap_or_else(|| Arc::new(NoopUsageReporter)),
            similarity_cache: similarity_cache
                .unwrap_or_else(|| Arc::new(PassthroughSimilarityCache)),
        }
    }
}

#[async_trait]
impl ToolExecutor for FindApisTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        log::debug!("Executing FindApisTool");
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let requirement = args
            .get("requirement")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid requirement"))?;

        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        let prompt = formatdoc!(
            "Find and evaluate APIs that could be used for: {}. {} For each API, provide:
            1. Name and brief description
            2. Key features and capabilities
            3. Pricing model (if available)
            4. Integration complexity
            5. Documentation quality
            6. Community support and popularity
            7. Any potential limitations or concerns
            8. Code example of basic usage",
            requirement,
            if !context.is_empty() {
                format!("Context: {}. ", context)
            } else {
                String::new()
            }
        );

        log::info!(
            "Prepared API search prompt for requirement: {}",
            requirement
        );

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body = call_perplexity_api(
            &self.http_client,
            &self.similarity_cache,
            "sonar-reasoning-pro",
            messages,
            None,
        )
        .await?;

        // Report usage if available
        if let (Some(usage), Some(model)) = (
            response_body.get("usage"),
            response_body.get("model").and_then(|m| m.as_str()),
        ) {
            if let (Some(completion_tokens), Some(prompt_tokens), Some(total_tokens)) = (
                usage.get("completion_tokens").and_then(|t| t.as_u64()),
                usage.get("prompt_tokens").and_then(|t| t.as_u64()),
                usage.get("total_tokens").and_then(|t| t.as_u64()),
            ) {
                let _ = self.usage_reporter.report(UsageReport {
                    model: model.to_string(),
                    usage: Usage {
                        completion_tokens,
                        prompt_tokens,
                        total_tokens,
                    },
                });
            }
        }

        let content = format_response_with_references(&response_body)?;

        Ok(vec![ToolContent::Text { text: content }])
    }

    fn to_tool(&self) -> Tool {
        Tool {
            name: "find_apis".into(),
            description: Some(
                "Find and evaluate APIs that could be integrated into a project".into(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "requirement": {
                        "type": "string",
                        "description": "The functionality or requirement you're looking to fulfill"
                    },
                    "context": {
                        "type": "string",
                        "description": "Additional context about the project or specific needs"
                    }
                },
                "required": ["requirement"]
            }),
        }
    }
}

pub struct CheckDeprecatedCodeTool {
    http_client: Arc<dyn HttpClient>,
    usage_reporter: Arc<dyn UsageReporter>,
    similarity_cache: Arc<dyn SimilarityCache>,
}

impl CheckDeprecatedCodeTool {
    pub fn new(
        http_client: Arc<dyn HttpClient>,
        usage_reporter: Option<Arc<dyn UsageReporter>>,
        similarity_cache: Option<Arc<dyn SimilarityCache>>,
    ) -> Self {
        Self {
            http_client,
            usage_reporter: usage_reporter.unwrap_or_else(|| Arc::new(NoopUsageReporter)),
            similarity_cache: similarity_cache
                .unwrap_or_else(|| Arc::new(PassthroughSimilarityCache)),
        }
    }
}

#[async_trait]
impl ToolExecutor for CheckDeprecatedCodeTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        log::debug!("Executing CheckDeprecatedCodeTool");
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let code = args
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid code"))?;

        let technology = args
            .get("technology")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let prompt = formatdoc!(
            "Analyze this code for deprecated features or patterns{}:

            {}

            Please provide:
            1. Identification of any deprecated features, methods, or patterns
            2. Current recommended alternatives
            3. Migration steps if applicable
            4. Impact of the deprecation
            5. Timeline of deprecation if known
            6. Code examples showing how to update to current best practices",
            if !technology.is_empty() {
                format!(" in {}", technology)
            } else {
                String::new()
            },
            code
        );

        log::info!(
            "Prepared code deprecation check prompt for technology: {}",
            technology
        );

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body = call_perplexity_api(
            &self.http_client,
            &self.similarity_cache,
            "sonar-reasoning-pro",
            messages,
            None,
        )
        .await?;

        // Report usage if available
        if let (Some(usage), Some(model)) = (
            response_body.get("usage"),
            response_body.get("model").and_then(|m| m.as_str()),
        ) {
            if let (Some(completion_tokens), Some(prompt_tokens), Some(total_tokens)) = (
                usage.get("completion_tokens").and_then(|t| t.as_u64()),
                usage.get("prompt_tokens").and_then(|t| t.as_u64()),
                usage.get("total_tokens").and_then(|t| t.as_u64()),
            ) {
                let _ = self.usage_reporter.report(UsageReport {
                    model: model.to_string(),
                    usage: Usage {
                        completion_tokens,
                        prompt_tokens,
                        total_tokens,
                    },
                });
            }
        }

        let content = format_response_with_references(&response_body)?;

        Ok(vec![ToolContent::Text { text: content }])
    }

    fn to_tool(&self) -> Tool {
        Tool {
            name: "check_deprecated_code".into(),
            description: Some(
                "Check if code or dependencies might be using deprecated features".into(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The code snippet or dependency to check"
                    },
                    "technology": {
                        "type": "string",
                        "description": "The technology or framework context (e.g., 'React', 'Node.js')"
                    }
                },
                "required": ["code"]
            }),
        }
    }
}
