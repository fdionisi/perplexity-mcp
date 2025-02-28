mod prompt_registry;
mod resource_registry;
mod tool_registry;

use std::{env, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use context_server::{
    ContextServer, ContextServerRpcRequest, ContextServerRpcResponse, Tool, ToolContent,
    ToolExecutor,
};
use http_client::{HttpClient, Request, RequestBuilderExt, ResponseAsyncBodyExt};
use http_client_reqwest::HttpClientReqwest;
use serde_json::{Value, json};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{
    prompt_registry::PromptRegistry, resource_registry::ResourceRegistry,
    tool_registry::ToolRegistry,
};

struct ContextServerState {
    rpc: ContextServer,
}

impl ContextServerState {
    fn new(http_client: Arc<dyn HttpClient>) -> Result<Self> {
        let resource_registry = Arc::new(ResourceRegistry::default());

        let tool_registry = Arc::new(ToolRegistry::default());

        tool_registry.register(Arc::new(SearchTool::new(http_client.clone())));
        tool_registry.register(Arc::new(GetDocumentationTool::new(http_client.clone())));
        tool_registry.register(Arc::new(FindApisTool::new(http_client.clone())));
        tool_registry.register(Arc::new(CheckDeprecatedCodeTool::new(http_client.clone())));

        let prompt_registry = Arc::new(PromptRegistry::default());

        Ok(Self {
            rpc: ContextServer::builder()
                .with_server_info((env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")))
                .with_resources(resource_registry)
                .with_tools(tool_registry)
                .with_prompts(prompt_registry)
                .build()?,
        })
    }

    async fn process_request(
        &self,
        request: ContextServerRpcRequest,
    ) -> Result<Option<ContextServerRpcResponse>> {
        self.rpc.handle_incoming_message(request).await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let http_client = Arc::new(HttpClientReqwest::default());

    if env::var("PERPLEXITY_API_KEY").is_err() {
        eprintln!("PERPLEXITY_API_KEY environment variable is required");
        std::process::exit(1);
    }

    let state = ContextServerState::new(http_client)?;

    let mut stdin = BufReader::new(io::stdin()).lines();
    let mut stdout = io::stdout();

    while let Some(line) = stdin.next_line().await? {
        let request: ContextServerRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                eprintln!("Error parsing request: {}", e);
                continue;
            }
        };

        if let Some(response) = state.process_request(request).await? {
            let response_json = serde_json::to_string(&response)?;
            stdout.write_all(response_json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }

    Ok(())
}

fn format_response_with_references(response_body: &Value) -> Result<String> {
    let content = response_body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("Failed to extract content from response"))?
        .to_string();

    if let Some(citations) = response_body.get("citations").and_then(|c| c.as_array()) {
        if !citations.is_empty() {
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

    Ok(content)
}

async fn call_perplexity_api(
    http_client: &Arc<dyn HttpClient>,
    model: &str,
    messages: Value,
) -> Result<Value> {
    let api_key = env::var("PERPLEXITY_API_KEY")
        .map_err(|_| anyhow!("PERPLEXITY_API_KEY not set in environment"))?;

    let request_body = json!({
        "model": model,
        "messages": messages
    });

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

    response
        .json()
        .await
        .map_err(|err| anyhow!("{}", err.to_string()))
}

struct SearchTool {
    http_client: Arc<dyn HttpClient>,
}

impl SearchTool {
    fn new(http_client: Arc<dyn HttpClient>) -> Self {
        Self { http_client }
    }
}

#[async_trait]
impl ToolExecutor for SearchTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid query"))?;

        let detail_level = args
            .get("detail_level")
            .and_then(|v| v.as_str())
            .unwrap_or("normal");

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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages).await?;

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
                    }
                },
                "required": ["query"]
            }),
        }
    }
}

struct GetDocumentationTool {
    http_client: Arc<dyn HttpClient>,
}

impl GetDocumentationTool {
    fn new(http_client: Arc<dyn HttpClient>) -> Self {
        Self { http_client }
    }
}

#[async_trait]
impl ToolExecutor for GetDocumentationTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid query"))?;

        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        let prompt = format!(
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages).await?;

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

struct FindApisTool {
    http_client: Arc<dyn HttpClient>,
}

impl FindApisTool {
    fn new(http_client: Arc<dyn HttpClient>) -> Self {
        Self { http_client }
    }
}

#[async_trait]
impl ToolExecutor for FindApisTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let requirement = args
            .get("requirement")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid requirement"))?;

        let context = args.get("context").and_then(|v| v.as_str()).unwrap_or("");

        let prompt = format!(
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages).await?;

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

struct CheckDeprecatedCodeTool {
    http_client: Arc<dyn HttpClient>,
}

impl CheckDeprecatedCodeTool {
    fn new(http_client: Arc<dyn HttpClient>) -> Self {
        Self { http_client }
    }
}

#[async_trait]
impl ToolExecutor for CheckDeprecatedCodeTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let code = args
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid code"))?;

        let technology = args
            .get("technology")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let prompt = format!(
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages).await?;

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
