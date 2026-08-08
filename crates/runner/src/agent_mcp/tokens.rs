/// Estimate LLM tokens without an API call (≈4 chars/token for EN/code mix).
pub fn estimate_tokens(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    ((text.len() as f64) / 4.0).ceil().max(1.0) as u64
}

#[derive(Debug, Clone, Default)]
pub struct TokenBudget {
    pub schema_context: u64,
    pub nl_prompts: u64,
    pub query_generation: u64,
    pub tool_responses: u64,
}

impl TokenBudget {
    pub fn record_schema(&mut self, text: &str) {
        self.schema_context += estimate_tokens(text);
    }

    pub fn record_probe(&mut self, nl: &str, query: &str, response: &str) {
        self.nl_prompts += estimate_tokens(nl);
        self.query_generation += estimate_tokens(query);
        self.tool_responses += estimate_tokens(response);
    }

    /// Tokens the LLM would read (context + prompts + tool results).
    pub fn input_tokens(&self) -> u64 {
        self.schema_context + self.nl_prompts + self.tool_responses
    }

    /// Tokens the LLM would generate (SQL / TypeQL).
    pub fn output_tokens(&self) -> u64 {
        self.query_generation
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens() + self.output_tokens()
    }
}
