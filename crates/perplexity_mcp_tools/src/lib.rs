use std::{env, sync::Arc};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use context_server::{Tool, ToolContent, ToolExecutor};
use http_client::{HttpClient, Request, RequestBuilderExt, ResponseAsyncBodyExt};
use indoc::formatdoc;
use serde_json::{Value, json};

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
    search_recency_filter: Option<&str>,
) -> Result<Value> {
    let api_key = env::var("PERPLEXITY_API_KEY")
        .map_err(|_| anyhow!("PERPLEXITY_API_KEY not set in environment"))?;

    let mut request_body = json!({
        "model": model,
        "messages": messages
    });

    if let Some(filter) = search_recency_filter {
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

    response
        .json()
        .await
        .map_err(|err| anyhow!("{}", err.to_string()))
}

pub struct SearchTool {
    http_client: Arc<dyn HttpClient>,
}

impl SearchTool {
    pub fn new(http_client: Arc<dyn HttpClient>) -> Self {
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body = call_perplexity_api(
            &self.http_client,
            "sonar-reasoning-pro",
            messages,
            search_recency_filter,
        )
        .await?;

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
}

impl GetDocumentationTool {
    pub fn new(http_client: Arc<dyn HttpClient>) -> Self {
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages, None).await?;

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
}

impl FindApisTool {
    pub fn new(http_client: Arc<dyn HttpClient>) -> Self {
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages, None).await?;

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
}

impl CheckDeprecatedCodeTool {
    pub fn new(http_client: Arc<dyn HttpClient>) -> Self {
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

        let messages = json!([{"role": "user", "content": prompt}]);

        let response_body =
            call_perplexity_api(&self.http_client, "sonar-reasoning-pro", messages, None).await?;

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

pub struct DeepResearchTool {
    http_client: Arc<dyn HttpClient>,
}

impl DeepResearchTool {
    pub fn new(http_client: Arc<dyn HttpClient>) -> Self {
        Self { http_client }
    }
}

#[async_trait]
impl ToolExecutor for DeepResearchTool {
    async fn execute(&self, arguments: Option<Value>) -> Result<Vec<ToolContent>> {
        let args = arguments.ok_or_else(|| anyhow!("Missing arguments"))?;

        let topic = args
            .get("topic")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing or invalid research topic"))?;

        let depth = args
            .get("depth")
            .and_then(|v| v.as_str())
            .unwrap_or("comprehensive");

        let focus = args.get("focus").and_then(|v| v.as_str()).unwrap_or("");

        let time_constraint = args
            .get("time_constraint")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let citation_style = args
            .get("citation_style")
            .and_then(|v| v.as_str())
            .unwrap_or("apa");

        // Construct a robust prompt for deep research
        let prompt = formatdoc!(
            "Conduct a deep research investigation on: {}

            Please approach this as an expert researcher would, conducting multiple searches and analyzing diverse sources to provide comprehensive information. Your research should be {}{}{}

            When preparing your report:
            1. Start with an executive summary of key findings
            2. Organize information in a logical structure with headings and subheadings
            3. Include critical analysis and multiple perspectives
            4. Cite all sources using {} format
            5. Prioritize recent, peer-reviewed, and authoritative sources
            6. Identify any gaps in existing research
            7. Conclude with practical implications and future directions",
            topic,
            depth,
            if !focus.is_empty() {
                format!(", focused on {}", focus)
            } else {
                String::new()
            },
            if !time_constraint.is_empty() {
                format!(". Consider the time period: {}", time_constraint)
            } else {
                String::new()
            },
            citation_style
        );

        // Use Perplexity's dedicated Deep Research model
        let model = "sonar-deep-research";

        // Configure for Deep Research format with extensive search parameters
        let messages = json!([{
            "role": "system",
            "content": "You are a Deep Research agent capable of conducting comprehensive research by performing multiple searches. Your goal is to create an in-depth report that combines information from hundreds of sources, analyzes contradictions, and presents a complete picture of the topic."
        }, {
            "role": "user",
            "content": prompt
        }]);

        // Apply extended context window and reasoning parameters
        let mut request_body = json!({
            "model": model,
            "messages": messages,
            "temperature": 0.2,  // Lower temperature for more factual output
            "max_tokens": 4000,  // Substantial response
            "search_iterations": 10  // Enable multiple search rounds
        });

        // Add search recency filter if time constraint is specified
        if time_constraint.contains("recent") || time_constraint.contains("latest") {
            request_body["search_recency_filter"] = json!("week");
        } else if time_constraint.contains("year") {
            request_body["search_recency_filter"] = json!("month");
        }

        // Custom API call to handle deep research parameters
        let api_key = env::var("PERPLEXITY_API_KEY")
            .map_err(|_| anyhow!("PERPLEXITY_API_KEY not set in environment"))?;

        let response = self
            .http_client
            .send(
                Request::builder()
                    .method("POST")
                    .uri("https://api.perplexity.ai/chat/completions")
                    .header("Authorization", format!("Bearer {}", api_key))
                    .header("Content-Type", "application/json")
                    .json(request_body)?,
            )
            .await?;

        let response_body = response
            .json()
            .await
            .map_err(|err| anyhow!("{}", err.to_string()))?;

        // Format response with enhanced reference formatting
        let content = format_deep_research_response(&response_body, citation_style)?;

        Ok(vec![ToolContent::Text { text: content }])
    }

    fn to_tool(&self) -> Tool {
        Tool {
            name: "deep_research".into(),
            description: Some(
                "Conduct in-depth research on complex topics by analyzing hundreds of sources"
                    .into(),
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "topic": {
                        "type": "string",
                        "description": "The research topic or question to investigate in depth"
                    },
                    "depth": {
                        "type": "string",
                        "description": "Desired research depth (brief, comprehensive, exhaustive)",
                        "enum": ["brief", "comprehensive", "exhaustive"]
                    },
                    "focus": {
                        "type": "string",
                        "description": "Optional focus area (academic, business, technical, historical, etc.)"
                    },
                    "time_constraint": {
                        "type": "string",
                        "description": "Optional time period to focus on (recent, last year, historical, etc.)"
                    },
                    "citation_style": {
                        "type": "string",
                        "description": "Citation style for references (apa, mla, chicago, ieee)",
                        "enum": ["apa", "mla", "chicago", "ieee"]
                    }
                },
                "required": ["topic"]
            }),
        }
    }
}

// Helper function to format deep research responses with enhanced citation handling
fn format_deep_research_response(response_body: &Value, citation_style: &str) -> Result<String> {
    // Extract the main content
    let content = response_body["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("Failed to extract content from response"))?
        .to_string();

    // Process citations with appropriate formatting based on selected style
    if let Some(citations) = response_body.get("citations").and_then(|c| c.as_array()) {
        if !citations.is_empty() {
            let mut formatted_refs = String::new();

            match citation_style {
                "apa" => {
                    formatted_refs.push_str("\n\n## References\n\n");
                    for (i, citation) in citations.iter().enumerate() {
                        if let (Some(title), Some(url)) = (
                            citation.get("title").and_then(|t| t.as_str()),
                            citation.get("url").and_then(|u| u.as_str()),
                        ) {
                            let authors = citation
                                .get("authors")
                                .and_then(|a| a.as_array())
                                .map(|authors| {
                                    authors
                                        .iter()
                                        .filter_map(|a| a.as_str())
                                        .collect::<Vec<&str>>()
                                        .join(", ")
                                })
                                .unwrap_or_else(|| "".to_string());

                            let date = citation.get("date").and_then(|d| d.as_str()).unwrap_or("");

                            formatted_refs.push_str(&format!(
                                "[{}] {}{} ({}). *{}*. {}\n\n",
                                i + 1,
                                if !authors.is_empty() {
                                    format!("{}. ", authors)
                                } else {
                                    "".to_string()
                                },
                                if !date.is_empty() {
                                    format!("({}). ", date)
                                } else {
                                    "".to_string()
                                },
                                citation
                                    .get("publisher")
                                    .and_then(|p| p.as_str())
                                    .unwrap_or(""),
                                title,
                                url
                            ));
                        } else {
                            formatted_refs.push_str(&format!(
                                "[{}] {}\n\n",
                                i + 1,
                                citation.as_str().unwrap_or("Unknown source")
                            ));
                        }
                    }
                }
                _ => {
                    // Default citation format for other styles
                    formatted_refs.push_str("\n\n## Sources\n\n");
                    for (i, citation) in citations.iter().enumerate() {
                        formatted_refs.push_str(&format!(
                            "[{}]: {}\n",
                            i + 1,
                            citation.as_str().unwrap_or("Unknown URL")
                        ));
                    }
                }
            }

            // Add source quality assessment
            formatted_refs.push_str("\n## Source Assessment\n\n");
            formatted_refs.push_str("| Category | Metrics |\n");
            formatted_refs.push_str("|----------|--------|\n");
            formatted_refs.push_str("| Total Sources | ");
            formatted_refs.push_str(&format!("{} |\n", citations.len()));

            // Add search depth information if available
            if let Some(search_info) = response_body.get("search_info") {
                if let Some(iterations) = search_info.get("iterations").and_then(|i| i.as_i64()) {
                    formatted_refs.push_str(&format!("| Search Iterations | {} |\n", iterations));
                }
            }

            return Ok(format!("{}\n{}", content, formatted_refs));
        }
    }

    // If no citations available, return just the content
    Ok(content)
}
