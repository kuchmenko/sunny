use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Debug)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u32,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<u32>,
    pub result: Option<Value>,
    pub error: Option<ResponseError>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
}

pub fn encode(msg: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let json = serde_json::to_string(msg)?;
    let header = format!("Content-Length: {}\r\n\r\n", json.len());
    let mut bytes = header.into_bytes();
    bytes.extend_from_slice(json.as_bytes());
    Ok(bytes)
}

pub fn decode_content_length(header: &str) -> Option<usize> {
    header.strip_prefix("Content-Length: ")?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{decode_content_length, encode, Notification};

    #[test]
    fn test_lsp_jsonrpc_framing() {
        let msg = Notification {
            jsonrpc: "2.0".to_string(),
            method: "workspace/didChangeConfiguration".to_string(),
            params: None,
        };

        let encoded = encode(&msg).expect("test: encode notification");
        let split = encoded
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("test: framing separator exists");

        let header = std::str::from_utf8(&encoded[..split]).expect("test: header is utf8");
        let body = &encoded[(split + 4)..];

        let content_length = decode_content_length(header).expect("test: parse content length");
        assert_eq!(content_length, body.len());
    }
}
