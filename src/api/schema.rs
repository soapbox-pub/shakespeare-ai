//! A chat completion request, in our own strict shape.
//!
//! Clients speak OpenAI's chat API, but only the part of it that providers
//! generally share, plus `reasoning_effort`. A request is parsed into these
//! types and the upstream request is written from them, so nothing a client
//! sends reaches a provider unless it's named here: no routing options
//! (OpenRouter's `models`, `provider`, `plugins`…), no `n`, no unknown
//! fields. Fields outside the schema are ignored; a field in it with the
//! wrong shape is an error.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub max_completion_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub stop: Option<Stop>,
    #[serde(default)]
    pub seed: Option<i64>,
    #[serde(default)]
    pub frequency_penalty: Option<f64>,
    #[serde(default)]
    pub presence_penalty: Option<f64>,
    #[serde(default)]
    pub tools: Option<Vec<Tool>>,
    #[serde(default)]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    #[serde(default)]
    pub response_format: Option<ResponseFormat>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The conversation this request belongs to, as OpenRouter takes it (or
    /// the `x-session-id` header). It never reaches a provider as sent:
    /// see [`crate::api::chat::session_key`].
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: Content,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Developer {
        content: Content,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    User {
        content: Content,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Assistant {
        #[serde(default)]
        content: Option<Content>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<ToolCall>>,
        /// OpenRouter's signed reasoning blocks, handed back so a reasoning
        /// model can carry on where it left off. Only OpenRouter gets them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_details: Option<Vec<Value>>,
    },
    Tool {
        content: Content,
        tool_call_id: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ImageDetail>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageDetail {
    Auto,
    Low,
    High,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FunctionType,
    pub function: FunctionCall,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// The only kind of tool there is.
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FunctionType {
    Function,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: FunctionType,
    pub function: FunctionDefinition,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A JSON Schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ToolChoice {
    Mode(ToolChoiceMode),
    Named { #[serde(rename = "type")] kind: FunctionType, function: NamedFunction },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoiceMode {
    None,
    Auto,
    Required,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct NamedFunction {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Stop {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: JsonSchemaFormat },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct JsonSchemaFormat {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

/// The fields of a request that go upstream as they are.
#[derive(Serialize)]
struct Upstream<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<&'a Stop>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<&'a ResponseFormat>,
}

impl ChatRequest {
    pub fn is_stream(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    /// The most output the client asked for, if it said.
    pub fn max_output(&self) -> Option<u32> {
        self.max_completion_tokens.or(self.max_tokens)
    }

    /// Roughly how many tokens the prompt is, before a provider says: all of
    /// it at about four bytes a token, and each image as a typical one. (An
    /// image's bytes say nothing of its tokens: a provider scales it.)
    ///
    /// All of it means every byte it's written in, keys and numbers and
    /// punctuation too, not only its strings: a provider reads a tool's
    /// schema whole, and a hold that counted only its strings could be made
    /// as small as anyone liked. Only the images' own URLs are left out.
    pub fn prompt_tokens_estimate(&self) -> u64 {
        /// About what a 1024×1024 image is to Claude or GPT.
        const IMAGE_TOKENS: u64 = 1600;
        let images = self.messages.iter().flat_map(|message| match message {
            Message::System { content, .. } | Message::Developer { content, .. } | Message::User { content, .. } | Message::Tool { content, .. } => Some(content),
            Message::Assistant { content, .. } => content.as_ref(),
        });
        let images: Vec<&str> = images
            .flat_map(|content| match content {
                Content::Text(_) => &[][..],
                Content::Parts(parts) => parts.as_slice(),
            })
            .filter_map(|part| match part {
                Part::ImageUrl { image_url } => Some(image_url.url.as_str()),
                Part::Text { .. } => None,
            })
            .collect();
        let bytes = (json_length(&self.messages) + json_length(&self.tools)).saturating_sub(images.iter().map(|url| url.len() as u64).sum());
        bytes.div_ceil(4) + images.len() as u64 * IMAGE_TOKENS
    }

    /// The request for a provider, as a JSON object to add our own fields
    /// to: `model` is the provider's id, and OpenRouter gets its own spelling
    /// of reasoning effort and the reasoning blocks it sent before.
    pub fn upstream(&self, model: &str, openrouter: bool) -> serde_json::Map<String, Value> {
        let upstream = Upstream {
            model,
            messages: &self.messages,
            max_tokens: self.max_tokens,
            max_completion_tokens: self.max_completion_tokens,
            temperature: self.temperature,
            top_p: self.top_p,
            stop: self.stop.as_ref(),
            seed: self.seed,
            frequency_penalty: self.frequency_penalty,
            presence_penalty: self.presence_penalty,
            tools: self.tools.as_deref(),
            tool_choice: self.tool_choice.as_ref(),
            parallel_tool_calls: self.parallel_tool_calls,
            response_format: self.response_format.as_ref(),
        };
        let Ok(Value::Object(mut out)) = serde_json::to_value(&upstream) else {
            unreachable!("a struct serializes to an object")
        };
        if !openrouter && let Some(Value::Array(messages)) = out.get_mut("messages") {
            for message in messages.iter_mut().filter_map(Value::as_object_mut) {
                message.remove("reasoning_details");
            }
        }
        if let Some(effort) = self.reasoning_effort {
            let effort = serde_json::to_value(effort).unwrap_or(Value::Null);
            if openrouter {
                out.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
            } else {
                out.insert("reasoning_effort".into(), effort);
            }
        }
        if self.is_stream() {
            out.insert("stream".into(), true.into());
            out.insert("stream_options".into(), serde_json::json!({ "include_usage": true }));
        }
        out
    }
}

/// How many bytes `value` is as JSON, counted rather than written out.
fn json_length(value: &impl Serialize) -> u64 {
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    let _ = serde_json::to_writer(&mut counter, value);
    counter.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(value: Value) -> Result<ChatRequest, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn drops_everything_outside_the_schema() {
        let request = parse(json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "sys", "cache_control": { "type": "ephemeral" } },
                { "role": "user", "content": [{ "type": "text", "text": "hi", "cache_control": {} }, { "type": "image_url", "image_url": { "url": "data:x", "detail": "low" } }] },
                { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "f", "arguments": "{}" } }], "reasoning_details": [{ "type": "reasoning.text" }], "reasoning_content": "x" },
                { "role": "tool", "tool_call_id": "c1", "content": "ok" },
            ],
            "models": ["anthropic/claude-opus-4"],
            "provider": { "order": ["x"] },
            "plugins": [{ "id": "web" }],
            "route": "fallback",
            "n": 10,
            "user": "u",
            "stream": true,
            "stream_options": { "include_usage": false },
            "usage": { "include": false },
            "max_tokens": 100,
            "reasoning_effort": "high",
            "tools": [{ "type": "function", "function": { "name": "f", "parameters": { "type": "object" } } }],
            "tool_choice": "auto",
        }))
        .unwrap();

        let openrouter = request.upstream("google/gemma", true);
        let keys: Vec<&str> = openrouter.keys().map(String::as_str).collect();
        assert_eq!(keys, ["model", "messages", "max_tokens", "tools", "tool_choice", "reasoning", "stream", "stream_options"]);
        assert_eq!(openrouter["model"], "google/gemma");
        assert_eq!(openrouter["reasoning"], json!({ "effort": "high" }));
        assert_eq!(openrouter["stream_options"], json!({ "include_usage": true }));
        assert_eq!(openrouter["messages"][0], json!({ "role": "system", "content": "sys" }));
        assert_eq!(openrouter["messages"][1]["content"][0], json!({ "type": "text", "text": "hi" }));
        assert_eq!(openrouter["messages"][2]["reasoning_details"], json!([{ "type": "reasoning.text" }]));
        assert!(openrouter["messages"][2].get("reasoning_content").is_none());

        let other = request.upstream("gemma", false);
        assert_eq!(other["reasoning_effort"], "high");
        assert!(other["messages"][2].get("reasoning_details").is_none());
        // Writing one upstream's request leaves the next's alone.
        assert_eq!(request.upstream("google/gemma", true)["messages"][2]["reasoning_details"], json!([{ "type": "reasoning.text" }]));
    }

    #[test]
    fn estimates_every_byte_of_the_prompt() {
        let estimate = |body: Value| parse(body).unwrap().prompt_tokens_estimate();
        let big = "x".repeat(40_000);
        // A schema's keys and numbers are read by the provider, so they count.
        let mut properties = serde_json::Map::new();
        for i in 0..1000 {
            properties.insert(format!("{big:.36}{i:04}"), json!({ "enum": [1, 2, 3] }));
        }
        let tools = json!([{ "type": "function", "function": { "name": "f", "parameters": { "type": "object", "properties": properties } } }]);
        assert!(estimate(json!({ "model": "m", "messages": [], "tools": tools })) > 15_000);
        // A string under a key called `url` is only an image in an image part.
        let disguised = json!([{ "type": "function", "function": { "name": "f", "parameters": { "url": big } } }]);
        assert!(estimate(json!({ "model": "m", "messages": [], "tools": disguised })) > 10_000);
        let reasoning = json!([{ "role": "assistant", "content": null, "reasoning_details": [{ "url": big }] }]);
        assert!(estimate(json!({ "model": "m", "messages": reasoning })) > 10_000);
        // A real image is a typical image, however big its data.
        let image = json!([{ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{big}") } }] }]);
        assert!((1600..1650).contains(&estimate(json!({ "model": "m", "messages": image }))));
        assert!(estimate(json!({ "model": "m", "messages": [{ "role": "user", "content": big }] })) >= 10_000);
    }

    #[test]
    fn refuses_what_it_cannot_read() {
        for bad in [
            json!({ "model": "m", "messages": [{ "role": "function", "content": "x" }] }),
            json!({ "model": "m", "messages": [{ "role": "user", "content": [{ "type": "input_audio" }] }] }),
            json!({ "model": "m", "messages": [{ "role": "user" }] }),
            json!({ "model": "m", "messages": [], "tool_choice": "sometimes" }),
            json!({ "model": "m", "messages": [], "reasoning_effort": "extreme" }),
            json!({ "model": "m", "messages": [], "max_tokens": -1 }),
            json!({ "model": "m", "messages": [], "tools": [{ "type": "web_search" }] }),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
        let ok = parse(json!({ "model": "m", "messages": [], "stream": null, "max_tokens": null, "tool_choice": { "type": "function", "function": { "name": "f" } } }));
        assert!(ok.is_ok());
    }
}
