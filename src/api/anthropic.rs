//! Anthropic's Messages API, for clients that speak OpenAI's chat API.
//!
//! A chat request is written as a Messages request, and the message (or its
//! event stream) is written back as a chat completion (or its chunks), with
//! usage in OpenAI's terms: `prompt_tokens` counts cached input too, and
//! `prompt_tokens_details` says how much of it was read from or written to
//! the cache.
//!
//! Prompt caching is marked on every request, as with OpenRouter's
//! `anthropic/` models. Thinking comes back as OpenRouter spells it,
//! `reasoning` for its text and `reasoning_details` for the signed blocks, so
//! clients that hand those back (as tool use requires) work with either.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use super::schema::{ChatRequest, Content, Message, Part, ReasoningEffort, ResponseFormat, Stop, ToolChoice, ToolChoiceMode};

/// The API version spoken. Newer features are asked for by field, not by
/// version.
pub const VERSION: &str = "2023-06-01";

/// What `max_tokens` is when the client doesn't say: Anthropic requires it,
/// and this much fits every current model.
const DEFAULT_MAX_TOKENS: u32 = 16384;

/// The `format` of the reasoning details this writes, as OpenRouter names
/// Anthropic's.
const FORMAT: &str = "anthropic-claude-v1";

/// A chat request as a Messages request for `model`.
pub fn request(chat: &ChatRequest, model: &str) -> Map<String, Value> {
    let mut system = Vec::new();
    let mut messages: Vec<(&'static str, Vec<Value>)> = Vec::new();
    for message in &chat.messages {
        match message {
            Message::System { content, .. } | Message::Developer { content, .. } => {
                system.extend(blocks(content).into_iter().filter(|b| b["type"] == "text"));
            }
            Message::User { content, .. } => push(&mut messages, "user", blocks(content)),
            Message::Assistant { content, tool_calls, reasoning_details, .. } => {
                let mut out: Vec<Value> = reasoning_details.iter().flatten().filter_map(thinking_block).collect();
                out.extend(content.as_ref().map(blocks).unwrap_or_default());
                for call in tool_calls.iter().flatten() {
                    let input = serde_json::from_str::<Value>(&call.function.arguments).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
                    out.push(json!({ "type": "tool_use", "id": call.id, "name": call.function.name, "input": input }));
                }
                push(&mut messages, "assistant", out);
            }
            Message::Tool { content, tool_call_id } => {
                push(&mut messages, "user", vec![json!({ "type": "tool_result", "tool_use_id": tool_call_id, "content": blocks(content) })]);
            }
        }
    }

    let max_tokens = chat.max_completion_tokens.or(chat.max_tokens).unwrap_or(DEFAULT_MAX_TOKENS);
    let mut out = Map::new();
    out.insert("model".into(), model.into());
    out.insert("max_tokens".into(), max_tokens.into());
    if !system.is_empty() {
        out.insert("system".into(), system.into());
    }
    out.insert("messages".into(), messages.into_iter().map(|(role, content)| json!({ "role": role, "content": content })).collect());
    if let Some(stop) = &chat.stop {
        let stops = match stop {
            Stop::One(s) => vec![s.clone()],
            Stop::Many(s) => s.clone(),
        };
        out.insert("stop_sequences".into(), stops.into());
    }
    if let Some(tools) = &chat.tools {
        let tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                let mut tool = json!({ "name": t.function.name, "input_schema": t.function.parameters.clone().unwrap_or_else(|| json!({ "type": "object" }).as_object().cloned().unwrap_or_default()) });
                if let Some(description) = &t.function.description {
                    tool["description"] = description.clone().into();
                }
                if t.function.strict == Some(true) {
                    tool["strict"] = true.into();
                }
                tool
            })
            .collect();
        out.insert("tools".into(), tools.into());
    }
    let forced = matches!(chat.tool_choice, Some(ToolChoice::Named { .. }) | Some(ToolChoice::Mode(ToolChoiceMode::Required)));
    let mut tool_choice = chat.tool_choice.as_ref().map(|choice| match choice {
        ToolChoice::Mode(ToolChoiceMode::None) => json!({ "type": "none" }),
        ToolChoice::Mode(ToolChoiceMode::Auto) => json!({ "type": "auto" }),
        ToolChoice::Mode(ToolChoiceMode::Required) => json!({ "type": "any" }),
        ToolChoice::Named { function, .. } => json!({ "type": "tool", "name": function.name }),
    });
    if chat.parallel_tool_calls == Some(false) && chat.tools.is_some() {
        let choice = tool_choice.get_or_insert_with(|| json!({ "type": "auto" }));
        if choice["type"] != "none" {
            choice["disable_parallel_tool_use"] = true.into();
        }
    }
    if let Some(choice) = tool_choice {
        out.insert("tool_choice".into(), choice);
    }

    let mut output_config = Map::new();
    if let Some(ResponseFormat::JsonSchema { json_schema }) = &chat.response_format
        && let Some(schema) = &json_schema.schema
    {
        output_config.insert("format".into(), json!({ "type": "json_schema", "schema": schema }));
    }
    let thinking = thinking(chat.reasoning_effort, version(model), max_tokens, forced);
    if let Some(effort) = thinking.effort {
        output_config.insert("effort".into(), effort.into());
    }
    if !output_config.is_empty() {
        out.insert("output_config".into(), output_config.into());
    }
    match thinking.config {
        // Thinking takes the model's own sampling: a temperature or top_p
        // is refused alongside it.
        Some(config) => {
            out.insert("thinking".into(), config);
        }
        None => {
            if let Some(t) = chat.temperature {
                // OpenAI's runs to 2, Anthropic's to 1.
                out.insert("temperature".into(), t.clamp(0.0, 1.0).into());
            }
            if let Some(p) = chat.top_p {
                out.insert("top_p".into(), p.into());
            }
        }
    }
    if chat.is_stream() {
        out.insert("stream".into(), true.into());
    }
    add_cache_breakpoints(&mut out);
    out
}

/// Add `blocks` to the conversation as `role`, run together with the
/// message before if that's `role`'s too, as Anthropic wants turns to
/// alternate.
fn push(messages: &mut Vec<(&'static str, Vec<Value>)>, role: &'static str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match messages.last_mut() {
        Some((last, content)) if *last == role => content.extend(blocks),
        _ => messages.push((role, blocks)),
    }
}

/// Content as Anthropic's blocks: text (none for empty text) and images.
fn blocks(content: &Content) -> Vec<Value> {
    match content {
        Content::Text(text) if text.is_empty() => vec![],
        Content::Text(text) => vec![json!({ "type": "text", "text": text })],
        Content::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Part::Text { text } if text.is_empty() => None,
                Part::Text { text } => Some(json!({ "type": "text", "text": text })),
                Part::ImageUrl { image_url } => Some(image(&image_url.url)),
            })
            .collect(),
    }
}

/// An image block for a `data:` URL or a web one.
fn image(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:")
        && let Some((media_type, data)) = rest.split_once(";base64,")
    {
        return json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": data } });
    }
    json!({ "type": "image", "source": { "type": "url", "url": url } })
}

/// A thinking block handed back, from the reasoning detail it was sent as:
/// only Anthropic's, which is all Anthropic can read.
fn thinking_block(detail: &Value) -> Option<Value> {
    if detail.get("format").and_then(Value::as_str).is_some_and(|f| f != FORMAT) {
        return None;
    }
    match detail.get("type")?.as_str()? {
        "reasoning.text" => {
            let signature = detail.get("signature")?.as_str()?;
            Some(json!({ "type": "thinking", "thinking": detail.get("text").and_then(Value::as_str).unwrap_or(""), "signature": signature }))
        }
        "reasoning.encrypted" => Some(json!({ "type": "redacted_thinking", "data": detail.get("data")?.as_str()? })),
        _ => None,
    }
}

/// A Claude model's version, `(4, 6)` for `claude-sonnet-4-6`, from its id:
/// the first number in it, and the next if it's a minor version rather than
/// a date (`claude-opus-4-20250514` is 4.0).
fn version(model: &str) -> Option<(u32, u32)> {
    let rest = model.strip_prefix("claude-")?;
    let mut numbers = rest.split('-').skip_while(|t| t.parse::<u32>().is_err());
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next().filter(|t| t.len() <= 2).and_then(|t| t.parse().ok()).unwrap_or(0);
    Some((major, minor))
}

struct Thinking {
    config: Option<Value>,
    effort: Option<&'static str>,
}

/// How to ask for `effort`. Claude 4.6 and later think adaptively, steered by
/// an effort level; earlier models take a token budget, which must leave
/// room in `max_tokens` and can't go with forced tool use. Claude 5 and later
/// think whether asked or not, and are only asked to show it.
fn thinking(effort: Option<ReasoningEffort>, version: Option<(u32, u32)>, max_tokens: u32, forced_tool: bool) -> Thinking {
    let adaptive = version.is_none_or(|v| v >= (4, 6));
    let shown = json!({ "type": "adaptive", "display": "summarized" });
    let always = version.is_some_and(|v| v >= (5, 0));
    let effort = effort.filter(|e| !matches!(e, ReasoningEffort::None));
    match effort {
        None if always => Thinking { config: Some(shown), effort: None },
        None => Thinking { config: None, effort: None },
        Some(effort) if adaptive => {
            let level = match effort {
                ReasoningEffort::None | ReasoningEffort::Minimal | ReasoningEffort::Low => "low",
                ReasoningEffort::Medium => "medium",
                ReasoningEffort::High => "high",
                // Claude 4.6 stops at high.
                ReasoningEffort::Xhigh if version.is_none_or(|v| v >= (4, 7)) => "xhigh",
                ReasoningEffort::Xhigh => "high",
            };
            Thinking { config: Some(shown), effort: Some(level) }
        }
        Some(effort) => {
            let budget: u32 = match effort {
                ReasoningEffort::None | ReasoningEffort::Minimal => 1024,
                ReasoningEffort::Low => 2048,
                ReasoningEffort::Medium => 8192,
                ReasoningEffort::High => 16384,
                ReasoningEffort::Xhigh => 32768,
            };
            let budget = budget.min(max_tokens.saturating_sub(1));
            if budget < 1024 || forced_tool {
                return Thinking { config: None, effort: None };
            }
            Thinking { config: Some(json!({ "type": "enabled", "budget_tokens": budget })), effort: None }
        }
    }
}

/// Mark prompt cache breakpoints: Anthropic allows four, and caches `tools`
/// → `system` → `messages` in that order, so the last tool, the end of the
/// system prompt, and the last two messages (caching nearly all, then all,
/// of the conversation).
fn add_cache_breakpoints(request: &mut Map<String, Value>) {
    let ephemeral = json!({ "type": "ephemeral" });
    let cacheable = |block: &&mut Value| !matches!(block["type"].as_str(), Some("thinking" | "redacted_thinking"));
    if let Some(last) = request.get_mut("tools").and_then(Value::as_array_mut).and_then(|t| t.last_mut()) {
        last["cache_control"] = ephemeral.clone();
    }
    if let Some(last) = request.get_mut("system").and_then(Value::as_array_mut).and_then(|s| s.last_mut()) {
        last["cache_control"] = ephemeral.clone();
    }
    if let Some(messages) = request.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut().rev().take(2) {
            if let Some(block) = message["content"].as_array_mut().and_then(|c| c.iter_mut().rev().find(cacheable)) {
                block["cache_control"] = ephemeral.clone();
            }
        }
    }
}

/// Anthropic's usage, in OpenAI's terms.
fn usage(usage: &Map<String, Value>) -> Value {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (read, write) = (count("cache_read_input_tokens"), count("cache_creation_input_tokens"));
    let input = count("input_tokens") + read + write;
    let output = count("output_tokens");
    json!({
        "prompt_tokens": input,
        "completion_tokens": output,
        "total_tokens": input + output,
        "prompt_tokens_details": { "cached_tokens": read, "cache_write_tokens": write },
    })
}

fn finish_reason(stop_reason: Option<&str>) -> &'static str {
    match stop_reason {
        Some("max_tokens" | "model_context_window_exceeded") => "length",
        Some("tool_use") => "tool_calls",
        Some("refusal") => "content_filter",
        _ => "stop",
    }
}

/// A message as a chat completion.
pub fn completion(message: &Value) -> Value {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut details = Vec::new();
    let mut tool_calls = Vec::new();
    for block in message["content"].as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => text.push_str(block["text"].as_str().unwrap_or_default()),
            Some("thinking") => {
                let thought = block["thinking"].as_str().unwrap_or_default();
                reasoning.push_str(thought);
                details.push(json!({ "type": "reasoning.text", "text": thought, "signature": block["signature"], "format": FORMAT, "index": details.len() }));
            }
            Some("redacted_thinking") => details.push(json!({ "type": "reasoning.encrypted", "data": block["data"], "format": FORMAT, "index": details.len() })),
            Some("tool_use") => tool_calls.push(json!({
                "id": block["id"],
                "type": "function",
                "function": { "name": block["name"], "arguments": block["input"].to_string() },
            })),
            _ => {}
        }
    }
    let mut reply = Map::new();
    reply.insert("role".into(), "assistant".into());
    reply.insert("content".into(), if text.is_empty() && !tool_calls.is_empty() { Value::Null } else { text.into() });
    if !details.is_empty() {
        reply.insert("reasoning".into(), reasoning.into());
        reply.insert("reasoning_details".into(), details.into());
    }
    if !tool_calls.is_empty() {
        reply.insert("tool_calls".into(), tool_calls.into());
    }
    json!({
        "id": message["id"],
        "object": "chat.completion",
        "created": crate::util::now(),
        "model": message["model"],
        "choices": [{ "index": 0, "message": reply, "finish_reason": finish_reason(message["stop_reason"].as_str()) }],
        "usage": message["usage"].as_object().map(usage),
    })
}

/// A message's event stream, rewritten event by event as chat completion
/// chunks.
#[derive(Default)]
pub struct Stream {
    id: Value,
    model: Value,
    created: i64,
    usage: Map<String, Value>,
    /// Each content block by its index.
    blocks: HashMap<u64, Block>,
    tool_calls: usize,
    details: usize,
    finish: Option<&'static str>,
}

enum Block {
    Text,
    /// The `index` of its tool call.
    Tool(usize),
    Thinking { text: String, signature: String },
    Other,
}

impl Stream {
    /// The chunks one event's data makes.
    pub fn push(&mut self, data: &str) -> Vec<String> {
        let Ok(event) = serde_json::from_str::<Value>(data) else { return vec![] };
        let index = event["index"].as_u64().unwrap_or(0);
        let delta = match event["type"].as_str() {
            Some("message_start") => {
                let message = &event["message"];
                self.id = message["id"].clone();
                self.model = message["model"].clone();
                self.created = crate::util::now();
                self.usage = message["usage"].as_object().cloned().unwrap_or_default();
                Some(json!({ "role": "assistant", "content": "" }))
            }
            Some("content_block_start") => {
                let block = &event["content_block"];
                match block["type"].as_str() {
                    Some("text") => {
                        self.blocks.insert(index, Block::Text);
                        block["text"].as_str().filter(|t| !t.is_empty()).map(|t| json!({ "content": t }))
                    }
                    Some("tool_use") => {
                        let n = self.tool_calls;
                        self.tool_calls += 1;
                        self.blocks.insert(index, Block::Tool(n));
                        Some(json!({ "tool_calls": [{ "index": n, "id": block["id"], "type": "function", "function": { "name": block["name"], "arguments": "" } }] }))
                    }
                    Some("thinking") => {
                        self.blocks.insert(index, Block::Thinking { text: String::new(), signature: String::new() });
                        None
                    }
                    Some("redacted_thinking") => {
                        self.blocks.insert(index, Block::Other);
                        Some(self.detail(json!({ "type": "reasoning.encrypted", "data": block["data"] })))
                    }
                    _ => {
                        self.blocks.insert(index, Block::Other);
                        None
                    }
                }
            }
            Some("content_block_delta") => {
                let delta = &event["delta"];
                match (self.blocks.get_mut(&index), delta["type"].as_str()) {
                    (Some(Block::Text), Some("text_delta")) => Some(json!({ "content": delta["text"] })),
                    (Some(Block::Tool(n)), Some("input_json_delta")) => Some(json!({ "tool_calls": [{ "index": n, "function": { "arguments": delta["partial_json"] } }] })),
                    (Some(Block::Thinking { text, .. }), Some("thinking_delta")) => {
                        let thought = delta["thinking"].as_str().unwrap_or_default();
                        text.push_str(thought);
                        Some(json!({ "reasoning": thought }))
                    }
                    (Some(Block::Thinking { signature, .. }), Some("signature_delta")) => {
                        signature.push_str(delta["signature"].as_str().unwrap_or_default());
                        None
                    }
                    _ => None,
                }
            }
            // A thinking block goes to the client whole, once it's signed,
            // so it can be handed back as it came.
            Some("content_block_stop") => match self.blocks.remove(&index) {
                Some(Block::Thinking { text, signature }) => Some(self.detail(json!({ "type": "reasoning.text", "text": text, "signature": signature }))),
                _ => None,
            },
            Some("message_delta") => {
                self.finish = Some(finish_reason(event["delta"]["stop_reason"].as_str()));
                for (key, value) in event["usage"].as_object().into_iter().flatten() {
                    if !value.is_null() {
                        self.usage.insert(key.clone(), value.clone());
                    }
                }
                None
            }
            Some("message_stop") => {
                let finish = self.chunk(json!({}), self.finish.or(Some("stop")));
                let mut last = self.chunk(json!({}), None);
                last["choices"] = json!([]);
                last["usage"] = usage(&self.usage);
                return vec![finish.to_string(), last.to_string()];
            }
            Some("error") => return vec![json!({ "error": event["error"] }).to_string()],
            _ => None,
        };
        delta.map(|d| vec![self.chunk(d, None).to_string()]).unwrap_or_default()
    }

    /// A delta carrying one reasoning detail.
    fn detail(&mut self, mut detail: Value) -> Value {
        detail["format"] = FORMAT.into();
        detail["index"] = self.details.into();
        self.details += 1;
        json!({ "reasoning_details": [detail] })
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(value: Value) -> ChatRequest {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn writes_a_messages_request() {
        let request = request(
            &chat(json!({
                "model": "m",
                "stream": true,
                "temperature": 1.5,
                "stop": "END",
                "messages": [
                    { "role": "system", "content": "sys" },
                    { "role": "developer", "content": [{ "type": "text", "text": "dev" }] },
                    { "role": "user", "content": [{ "type": "text", "text": "look" }, { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }, { "type": "image_url", "image_url": { "url": "https://x/y.png" } }] },
                    { "role": "assistant", "content": "", "tool_calls": [{ "id": "t1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }, { "id": "t2", "type": "function", "function": { "name": "g", "arguments": "nonsense" } }],
                      "reasoning_details": [
                          { "type": "reasoning.text", "text": "hmm", "signature": "sig", "format": FORMAT },
                          { "type": "reasoning.encrypted", "data": "xyz" },
                          { "type": "reasoning.text", "text": "unsigned" },
                          { "type": "reasoning.encrypted", "data": "theirs", "format": "google-gemini-v1" },
                      ] },
                    { "role": "tool", "tool_call_id": "t1", "content": "one" },
                    { "role": "tool", "tool_call_id": "t2", "content": [{ "type": "text", "text": "two" }] },
                    { "role": "user", "content": "thanks" },
                ],
                "tools": [{ "type": "function", "function": { "name": "f", "description": "does f", "parameters": { "type": "object", "properties": {} } } }, { "type": "function", "function": { "name": "g" } }],
                "tool_choice": "required",
                "parallel_tool_calls": false,
                "response_format": { "type": "json_schema", "json_schema": { "name": "s", "schema": { "type": "object" } } },
            })),
            "claude-sonnet-4-6",
        );
        assert_eq!(request["model"], "claude-sonnet-4-6");
        assert_eq!(request["max_tokens"], 16384);
        assert_eq!(request["temperature"], 1.0);
        assert_eq!(request["stop_sequences"], json!(["END"]));
        assert_eq!(request["stream"], true);
        assert_eq!(request["system"], json!([{ "type": "text", "text": "sys" }, { "type": "text", "text": "dev", "cache_control": { "type": "ephemeral" } }]));
        assert_eq!(request["tool_choice"], json!({ "type": "any", "disable_parallel_tool_use": true }));
        assert_eq!(request["tools"][0], json!({ "name": "f", "input_schema": { "type": "object", "properties": {} }, "description": "does f" }));
        assert_eq!(request["tools"][1], json!({ "name": "g", "input_schema": { "type": "object" }, "cache_control": { "type": "ephemeral" } }));
        assert_eq!(request["output_config"], json!({ "format": { "type": "json_schema", "schema": { "type": "object" } } }));

        let messages = request["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert_eq!(messages[0]["content"][1]["source"], json!({ "type": "base64", "media_type": "image/png", "data": "AAAA" }));
        assert_eq!(messages[0]["content"][2]["source"], json!({ "type": "url", "url": "https://x/y.png" }));
        let assistant = &messages[1]["content"];
        assert_eq!(assistant[0], json!({ "type": "thinking", "thinking": "hmm", "signature": "sig" }));
        assert_eq!(assistant[1], json!({ "type": "redacted_thinking", "data": "xyz" }));
        assert_eq!(assistant[2], json!({ "type": "tool_use", "id": "t1", "name": "f", "input": { "a": 1 } }));
        assert_eq!(assistant[3]["input"], json!({}));
        assert_eq!(assistant[3]["cache_control"]["type"], "ephemeral");
        assert_eq!(assistant.as_array().unwrap().len(), 4);
        // Tool results and the user's next words are one turn.
        let user = &messages[2]["content"];
        assert_eq!(user[0], json!({ "type": "tool_result", "tool_use_id": "t1", "content": [{ "type": "text", "text": "one" }] }));
        assert_eq!(user[1]["tool_use_id"], "t2");
        assert_eq!(user[2], json!({ "type": "text", "text": "thanks", "cache_control": { "type": "ephemeral" } }));
    }

    #[test]
    fn asks_each_model_to_think_its_way() {
        assert_eq!(version("claude-sonnet-4-6"), Some((4, 6)));
        assert_eq!(version("claude-opus-4-20250514"), Some((4, 0)));
        assert_eq!(version("claude-3-5-haiku-20241022"), Some((3, 5)));
        assert_eq!(version("claude-opus-5"), Some((5, 0)));
        assert_eq!(version("gpt-5"), None);

        let ask = |model: &str, effort: &str, extra: Value| {
            let mut body = json!({ "model": "m", "messages": [], "reasoning_effort": effort, "temperature": 0.5 });
            for (k, v) in extra.as_object().unwrap() {
                body[k] = v.clone();
            }
            request(&chat(body), model)
        };
        let adaptive = ask("claude-opus-4-8", "xhigh", json!({}));
        assert_eq!(adaptive["thinking"], json!({ "type": "adaptive", "display": "summarized" }));
        assert_eq!(adaptive["output_config"]["effort"], "xhigh");
        assert!(adaptive.get("temperature").is_none());
        assert_eq!(ask("claude-sonnet-4-6", "xhigh", json!({}))["output_config"]["effort"], "high");
        assert_eq!(ask("claude-sonnet-4-6", "minimal", json!({}))["output_config"]["effort"], "low");

        let budget = ask("claude-sonnet-4-5-20250929", "medium", json!({}));
        assert_eq!(budget["thinking"], json!({ "type": "enabled", "budget_tokens": 8192 }));
        assert!(budget.get("output_config").is_none());
        assert_eq!(ask("claude-sonnet-4-5", "high", json!({ "max_tokens": 4000 }))["thinking"]["budget_tokens"], 3999);
        // No room to think, or a tool that must be called: no thinking.
        assert!(ask("claude-sonnet-4-5", "high", json!({ "max_tokens": 1000 })).get("thinking").is_none());
        assert!(ask("claude-sonnet-4-5", "high", json!({ "tool_choice": "required", "tools": [{ "type": "function", "function": { "name": "f" } }] })).get("thinking").is_none());

        // Unasked, older models don't think, and Claude 5 is asked to show
        // the thinking it does anyway.
        let plain = request(&chat(json!({ "model": "m", "messages": [], "temperature": 0.5 })), "claude-sonnet-4-6");
        assert!(plain.get("thinking").is_none());
        assert_eq!(plain["temperature"], 0.5);
        let five = request(&chat(json!({ "model": "m", "messages": [] })), "claude-opus-5");
        assert_eq!(five["thinking"]["type"], "adaptive");
        assert!(ask("claude-sonnet-4-6", "none", json!({})).get("thinking").is_none());
    }

    #[test]
    fn reads_a_message() {
        let message = json!({
            "id": "msg_1", "model": "claude-x", "stop_reason": "tool_use",
            "content": [
                { "type": "thinking", "thinking": "hmm", "signature": "sig" },
                { "type": "text", "text": "Hi" },
                { "type": "tool_use", "id": "t1", "name": "f", "input": { "a": 1 } },
            ],
            "usage": { "input_tokens": 100, "cache_read_input_tokens": 600, "cache_creation_input_tokens": 300, "output_tokens": 50 },
        });
        let completion = completion(&message);
        let choice = &completion["choices"][0];
        assert_eq!(choice["finish_reason"], "tool_calls");
        assert_eq!(choice["message"]["content"], "Hi");
        assert_eq!(choice["message"]["reasoning"], "hmm");
        assert_eq!(choice["message"]["reasoning_details"], json!([{ "type": "reasoning.text", "text": "hmm", "signature": "sig", "format": FORMAT, "index": 0 }]));
        assert_eq!(choice["message"]["tool_calls"], json!([{ "id": "t1", "type": "function", "function": { "name": "f", "arguments": "{\"a\":1}" } }]));
        assert_eq!(completion["usage"], json!({ "prompt_tokens": 1000, "completion_tokens": 50, "total_tokens": 1050, "prompt_tokens_details": { "cached_tokens": 600, "cache_write_tokens": 300 } }));

        // What it reads, it can hand back.
        let reply = &choice["message"];
        let again = request(&chat(json!({ "model": "m", "messages": [reply] })), "claude-x");
        assert_eq!(again["messages"][0]["content"][0], json!({ "type": "thinking", "thinking": "hmm", "signature": "sig" }));
    }

    #[test]
    fn rewrites_a_stream() {
        let events = [
            json!({ "type": "message_start", "message": { "id": "msg_1", "model": "claude-x", "usage": { "input_tokens": 10, "cache_read_input_tokens": 90, "output_tokens": 1 } } }),
            json!({ "type": "ping" }),
            json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "hm" } }),
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Hel" } }),
            json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "lo" } }),
            json!({ "type": "content_block_stop", "index": 1 }),
            json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "tool_use", "id": "t1", "name": "f", "input": {} } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "{\"a\":" } }),
            json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "input_json_delta", "partial_json": "1}" } }),
            json!({ "type": "content_block_stop", "index": 2 }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" }, "usage": { "output_tokens": 20 } }),
            json!({ "type": "message_stop" }),
        ];
        let mut stream = Stream::default();
        let chunks: Vec<Value> = events.iter().flat_map(|e| stream.push(&e.to_string())).map(|c| serde_json::from_str(&c).unwrap()).collect();
        let deltas: Vec<&Value> = chunks.iter().filter_map(|c| c.pointer("/choices/0/delta")).collect();
        assert_eq!(deltas[0], &json!({ "role": "assistant", "content": "" }));
        assert_eq!(deltas[1], &json!({ "reasoning": "hm" }));
        assert_eq!(deltas[2], &json!({ "reasoning_details": [{ "type": "reasoning.text", "text": "hm", "signature": "sig", "format": FORMAT, "index": 0 }] }));
        assert_eq!((deltas[3], deltas[4]), (&json!({ "content": "Hel" }), &json!({ "content": "lo" })));
        assert_eq!(deltas[5]["tool_calls"][0], json!({ "index": 0, "id": "t1", "type": "function", "function": { "name": "f", "arguments": "" } }));
        assert_eq!(deltas[6]["tool_calls"][0]["function"]["arguments"], "{\"a\":");
        assert_eq!(chunks[chunks.len() - 2]["choices"][0]["finish_reason"], "tool_calls");
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"], json!([]));
        assert_eq!(last["usage"]["prompt_tokens"], 100);
        assert_eq!(last["usage"]["completion_tokens"], 20);
        assert_eq!(last["usage"]["prompt_tokens_details"]["cached_tokens"], 90);
        assert!(chunks.iter().all(|c| c["id"] == "msg_1" && c["model"] == "claude-x"));

        let error = Stream::default().push(&json!({ "type": "error", "error": { "type": "overloaded_error", "message": "busy" } }).to_string());
        assert_eq!(error, [json!({ "error": { "type": "overloaded_error", "message": "busy" } }).to_string()]);
    }
}
