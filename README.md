# Perplexity MCP Tools

Integration of Perplexity AI capabilities with the Model Control Protocol (MCP).

## Features

This package provides a set of Perplexity AI tools that can be used with MCP:

- **Search**: Perform general search queries to get comprehensive information on any topic
- **Get Documentation**: Retrieve documentation and usage examples for technologies, libraries, or APIs
- **Find APIs**: Discover and evaluate APIs that could be integrated into a project
- **Check Deprecated Code**: Analyze code or dependencies for deprecated features
- **Deep Research**: Conduct in-depth research on complex topics by analyzing hundreds of sources

## Installation

```bash
cargo install perplexity-mcp
```

## Configuration

Set your Perplexity API key as an environment variable:

```bash
export PERPLEXITY_API_KEY="your-api-key-here"
```

## Tool: Deep Research

The Deep Research tool leverages Perplexity's dedicated `sonar-deep-research` model to conduct comprehensive research on complex topics. It performs multiple search iterations and analyzes hundreds of sources to generate detailed, expert-level reports.

### Parameters

| Parameter | Description | Required | Default |
|-----------|-------------|----------|---------|
| topic | The research topic or question to investigate | Yes | - |
| depth | Research depth (brief, comprehensive, exhaustive) | No | "comprehensive" |
| focus | Focus area (academic, business, technical, etc.) | No | - |
| time_constraint | Time period to focus on (recent, last year, etc.) | No | - |
| citation_style | Citation style (apa, mla, chicago, ieee) | No | "apa" |

### Example Usage

```json
{
  "topic": "The impact of quantum computing on cryptography",
  "depth": "comprehensive",
  "focus": "cybersecurity implications",
  "time_constraint": "recent developments",
  "citation_style": "ieee"
}
```

## License

[MIT](LICENSE)