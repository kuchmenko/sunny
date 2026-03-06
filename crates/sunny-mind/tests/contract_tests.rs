use serde_json::json;
use sunny_mind::{
    ChatMessage, ChatRole, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolCall,
    ToolChoice, ToolDefinition,
};

/// Test that minimal LlmRequest serializes to JSON with only "messages" key (no null fields)
#[test]
fn test_request_minimal_contract() {
    let request = LlmRequest {
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: "Hello".to_string(),
        }],
        max_tokens: None,
        temperature: None,
        tools: None,
        tool_choice: None,
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
            },
            ChatMessage {
                role: ChatRole::User,
                content: "Hello".to_string(),
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
        }],
        max_tokens: Some(100),
        temperature: Some(0.5),
        tools: None,
        tool_choice: None,
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
    };
    let json = serde_json::to_string(&response).expect("should serialize");
    let back: LlmResponse = serde_json::from_str(&json).expect("should deserialize");
    assert_eq!(response, back);
}
