use proptest::prelude::*;
use serde_json::json;
use sunny_mind::{
    ChatMessage, ChatRole, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolCall,
    ToolChoice, ToolDefinition,
};

fn arb_chat_role() -> impl Strategy<Value = ChatRole> {
    prop_oneof![
        Just(ChatRole::System),
        Just(ChatRole::User),
        Just(ChatRole::Assistant),
        Just(ChatRole::Tool),
    ]
}

fn arb_chat_message() -> impl Strategy<Value = ChatMessage> {
    (
        arb_chat_role(),
        proptest::string::string_regex("(?s).{0,40}").expect("valid regex"),
    )
        .prop_map(|(role, content)| ChatMessage {
            role,
            content,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        })
}

fn arb_tool_definition() -> impl Strategy<Value = ToolDefinition> {
    (
        proptest::string::string_regex("[a-z_]{1,12}").expect("valid regex"),
        proptest::string::string_regex("(?s).{0,40}").expect("valid regex"),
        proptest::string::string_regex("[a-z_]{1,12}").expect("valid regex"),
    )
        .prop_map(|(name, description, field_name)| ToolDefinition {
            name,
            description,
            parameters: json!({
                "type": "object",
                "properties": {
                    field_name: { "type": "string" }
                }
            }),
        })
}

fn arb_tool_choice() -> impl Strategy<Value = ToolChoice> {
    prop_oneof![
        Just(ToolChoice::Auto),
        Just(ToolChoice::None),
        Just(ToolChoice::Required),
        proptest::string::string_regex("[a-z_]{1,12}")
            .expect("valid regex")
            .prop_map(ToolChoice::Specific),
    ]
}

fn arb_tool_call() -> impl Strategy<Value = ToolCall> {
    (
        proptest::string::string_regex("[a-z0-9_]{1,12}").expect("valid regex"),
        proptest::string::string_regex("[a-z_]{1,12}").expect("valid regex"),
        proptest::string::string_regex("(?s).{0,40}").expect("valid regex"),
        0usize..4,
    )
        .prop_map(|(id, name, arguments, execution_depth)| ToolCall {
            id,
            name,
            arguments,
            execution_depth,
        })
}

fn arb_llm_request() -> impl Strategy<Value = LlmRequest> {
    (
        prop::collection::vec(arb_chat_message(), 1..4),
        prop::option::of(1u32..4096),
        prop::option::of((0u16..1000).prop_map(|value| value as f32 / 1000.0)),
        prop::option::of(prop::collection::vec(arb_tool_definition(), 0..3)),
        prop::option::of(arb_tool_choice()),
        prop::option::of(1u32..32_768),
    )
        .prop_map(
            |(messages, max_tokens, temperature, tools, tool_choice, thinking_budget)| LlmRequest {
                messages,
                max_tokens,
                temperature,
                tools,
                tool_choice,
                thinking_budget,
            },
        )
}

fn arb_llm_response() -> impl Strategy<Value = LlmResponse> {
    (
        proptest::string::string_regex("(?s).{0,60}").expect("valid regex"),
        0u32..5000,
        0u32..5000,
        proptest::string::string_regex("[a-z_]{1,16}").expect("valid regex"),
        proptest::string::string_regex("[a-z0-9._-]{1,16}").expect("valid regex"),
        proptest::string::string_regex("[a-z0-9._-]{1,20}").expect("valid regex"),
        prop::option::of(prop::collection::vec(arb_tool_call(), 0..3)),
    )
        .prop_map(
            |(
                content,
                input_tokens,
                output_tokens,
                finish_reason,
                provider_id,
                model_id,
                tool_calls,
            )| {
                LlmResponse {
                    content,
                    usage: TokenUsage {
                        input_tokens,
                        output_tokens,
                        total_tokens: input_tokens + output_tokens,
                    },
                    finish_reason,
                    provider_id: ProviderId(provider_id),
                    model_id: ModelId(model_id),
                    tool_calls,
                    reasoning_content: None,
                }
            },
        )
}

/// Test that minimal LlmRequest serializes to JSON with only "messages" key (no null fields)
#[test]
fn test_request_minimal_contract() {
    let request = LlmRequest {
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "Hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        max_tokens: None,
        temperature: None,
        tools: None,
        tool_choice: None,
        thinking_budget: None,
    };

    let json = serde_json::to_string(&request).expect("should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");

    // Should only have "messages" field, no null fields
    assert!(value.get("messages").is_some(), "must have messages field");
    assert_eq!(
        value.as_object().unwrap().len(),
        1,
        "minimal request should only have messages field, got: {}",
        json
    );

    // Verify messages structure
    let messages = value.get("messages").unwrap().as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].get("role").unwrap(), "user");
    assert_eq!(messages[0].get("content").unwrap(), "Hello");
}

/// Test that LlmRequest with all fields populated serializes correctly
#[test]
fn test_request_full_contract() {
    let request = LlmRequest {
        messages: vec![
            ChatMessage {
                role: ChatRole::System,
                content: "You are a helpful assistant".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: "Hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ],
        max_tokens: Some(256),
        temperature: Some(0.7),
        tools: Some(vec![ToolDefinition {
            name: "fs_read".to_string(),
            description: "Read a file".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                }
            }),
        }]),
        tool_choice: Some(ToolChoice::Auto),
        thinking_budget: None,
    };

    let json = serde_json::to_string(&request).expect("should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");

    // All fields should be present
    assert!(value.get("messages").is_some());
    assert!(value.get("max_tokens").is_some());
    assert!(value.get("temperature").is_some());
    assert!(value.get("tools").is_some());
    assert!(value.get("tool_choice").is_some());

    // Verify field values
    assert_eq!(value.get("max_tokens").unwrap(), 256);
    assert_eq!(value.get("temperature").unwrap(), 0.7);
    assert_eq!(value.get("tool_choice").unwrap(), "auto");

    // Verify tools structure
    let tools = value.get("tools").unwrap().as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].get("name").unwrap(), "fs_read");
    assert_eq!(tools[0].get("description").unwrap(), "Read a file");
}

/// Test that LlmResponse without tool_calls serializes without "tool_calls" key
#[test]
fn test_response_minimal_contract() {
    let response = LlmResponse {
        content: "Hello, world!".to_string(),
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
        },
        finish_reason: "stop".to_string(),
        provider_id: ProviderId("kimi".to_string()),
        model_id: ModelId("moonshot-v1".to_string()),
        tool_calls: None,
        reasoning_content: None,
    };

    let json = serde_json::to_string(&response).expect("should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");

    // Required fields should be present
    assert!(value.get("content").is_some());
    assert!(value.get("usage").is_some());
    assert!(value.get("finish_reason").is_some());
    assert!(value.get("provider_id").is_some());
    assert!(value.get("model_id").is_some());

    // tool_calls should NOT be present when None
    assert!(
        value.get("tool_calls").is_none(),
        "tool_calls should be omitted when None, got: {}",
        json
    );

    // Verify content
    assert_eq!(value.get("content").unwrap(), "Hello, world!");
}

/// Test that LlmResponse with tool_calls includes proper structure
#[test]
fn test_response_with_tool_calls_contract() {
    let response = LlmResponse {
        content: "".to_string(),
        usage: TokenUsage {
            input_tokens: 25,
            output_tokens: 20,
            total_tokens: 45,
        },
        finish_reason: "tool_calls".to_string(),
        provider_id: ProviderId("kimi".to_string()),
        model_id: ModelId("moonshot-v1".to_string()),
        tool_calls: Some(vec![ToolCall {
            id: "call_123".to_string(),
            name: "fs_read".to_string(),
            arguments: "{\"path\":\"/tmp/test.txt\"}".to_string(),
            execution_depth: 0,
        }]),
        reasoning_content: None,
    };

    let json = serde_json::to_string(&response).expect("should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");

    // tool_calls should be present
    assert!(value.get("tool_calls").is_some());

    // Verify tool_calls structure
    let tool_calls = value.get("tool_calls").unwrap().as_array().unwrap();
    assert_eq!(tool_calls.len(), 1);

    let tool_call = &tool_calls[0];
    assert_eq!(tool_call.get("id").unwrap(), "call_123");
    assert_eq!(tool_call.get("name").unwrap(), "fs_read");
    assert_eq!(
        tool_call.get("arguments").unwrap(),
        "{\"path\":\"/tmp/test.txt\"}"
    );
    assert_eq!(tool_call.get("execution_depth").unwrap(), 0);
}

/// Test that all ChatRole variants serialize to lowercase
#[test]
fn test_chat_role_variants_contract() {
    // System
    let msg = ChatMessage {
        role: ChatRole::System,
        content: "System message".to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("should serialize");
    assert!(
        json.contains("\"role\":\"system\""),
        "System should serialize to 'system', got: {}",
        json
    );

    // User
    let msg = ChatMessage {
        role: ChatRole::User,
        content: "User message".to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("should serialize");
    assert!(
        json.contains("\"role\":\"user\""),
        "User should serialize to 'user', got: {}",
        json
    );

    // Assistant
    let msg = ChatMessage {
        role: ChatRole::Assistant,
        content: "Assistant message".to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("should serialize");
    assert!(
        json.contains("\"role\":\"assistant\""),
        "Assistant should serialize to 'assistant', got: {}",
        json
    );

    // Tool
    let msg = ChatMessage {
        role: ChatRole::Tool,
        content: "Tool result".to_string(),
        tool_calls: None,
        tool_call_id: None,
        reasoning_content: None,
    };
    let json = serde_json::to_string(&msg).expect("should serialize");
    assert!(
        json.contains("\"role\":\"tool\""),
        "Tool should serialize to 'tool', got: {}",
        json
    );
}

/// Test that ToolDefinition JSON shape matches OpenAI format
#[test]
fn test_tool_definition_contract() {
    let tool = ToolDefinition {
        name: "fs_scan".to_string(),
        description: "Scan a directory".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to scan"
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Whether to scan recursively"
                }
            },
            "required": ["path"]
        }),
    };

    let json = serde_json::to_string(&tool).expect("should serialize");
    let value: serde_json::Value = serde_json::from_str(&json).expect("should parse as JSON");

    // Should have name, description, parameters
    assert!(
        value.get("name").is_some(),
        "ToolDefinition must have 'name' field"
    );
    assert!(
        value.get("description").is_some(),
        "ToolDefinition must have 'description' field"
    );
    assert!(
        value.get("parameters").is_some(),
        "ToolDefinition must have 'parameters' field"
    );

    // Verify values
    assert_eq!(value.get("name").unwrap(), "fs_scan");
    assert_eq!(value.get("description").unwrap(), "Scan a directory");

    // Verify parameters structure
    let params = value.get("parameters").unwrap();
    assert_eq!(params.get("type").unwrap(), "object");
    assert!(params.get("properties").is_some());
    assert!(params.get("required").is_some());
}

/// Test ToolChoice variants serialize correctly
#[test]
fn test_tool_choice_contract() {
    // Auto
    let choice = ToolChoice::Auto;
    let json = serde_json::to_string(&choice).expect("should serialize");
    assert_eq!(json, "\"auto\"");

    // None
    let choice = ToolChoice::None;
    let json = serde_json::to_string(&choice).expect("should serialize");
    assert_eq!(json, "\"none\"");

    // Required
    let choice = ToolChoice::Required;
    let json = serde_json::to_string(&choice).expect("should serialize");
    assert_eq!(json, "\"required\"");

    // Specific - tuple variant serializes as {"specific": "value"}
    let choice = ToolChoice::Specific("fs_read".to_string());
    let json = serde_json::to_string(&choice).expect("should serialize");
    assert_eq!(json, "{\"specific\":\"fs_read\"}");
}

/// Test roundtrip serialization for all types
#[test]
fn test_contract_roundtrip() {
    // LlmRequest roundtrip
    let request = LlmRequest {
        messages: vec![ChatMessage {
            role: ChatRole::System,
            content: "Test".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }],
        max_tokens: Some(100),
        temperature: Some(0.5),
        tools: None,
        tool_choice: None,
        thinking_budget: None,
    };
    let json = serde_json::to_string(&request).expect("should serialize");
    let back: LlmRequest = serde_json::from_str(&json).expect("should deserialize");
    assert_eq!(request, back);

    // LlmResponse roundtrip
    let response = LlmResponse {
        content: "Response".to_string(),
        usage: TokenUsage {
            input_tokens: 5,
            output_tokens: 10,
            total_tokens: 15,
        },
        finish_reason: "stop".to_string(),
        provider_id: ProviderId("test".to_string()),
        model_id: ModelId("test-model".to_string()),
        tool_calls: Some(vec![ToolCall {
            id: "call_1".to_string(),
            name: "test_tool".to_string(),
            arguments: "{}".to_string(),
            execution_depth: 1,
        }]),
        reasoning_content: None,
    };
    let json = serde_json::to_string(&response).expect("should serialize");
    let back: LlmResponse = serde_json::from_str(&json).expect("should deserialize");
    assert_eq!(response, back);
}

proptest! {
    #[test]
    fn proptest_contract_roundtrip_llm_request(req in arb_llm_request()) {
        let json = serde_json::to_string(&req).expect("request should serialize");
        let back: LlmRequest = serde_json::from_str(&json).expect("request should deserialize");
        prop_assert_eq!(req, back);
    }

    #[test]
    fn proptest_contract_roundtrip_llm_response(res in arb_llm_response()) {
        let json = serde_json::to_string(&res).expect("response should serialize");
        let back: LlmResponse = serde_json::from_str(&json).expect("response should deserialize");
        prop_assert_eq!(res, back);
    }
}
