//! FinTS HTTPS transport layer.
//!
//! Sends FinTS messages to a bank's HBCI/FinTS endpoint via HTTPS POST.
//! Messages are base64-encoded in the request body, and responses are base64-decoded.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use tracing::{debug, trace};

use crate::error::{FinTSError, Result};

/// FinTS HTTP connection to a bank endpoint.
pub struct FinTSConnection {
    url: String,
    client: reqwest::Client,
}

impl FinTSConnection {
    /// Create a new connection to the given bank FinTS URL.
    pub fn new(url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .pool_idle_timeout(std::time::Duration::from_secs(2))
            .pool_max_idle_per_host(0)
            .tcp_nodelay(true)
            .build()
            .map_err(|e| FinTSError::Transport(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            url: url.to_string(),
            client,
        })
    }

    /// Send a raw FinTS message (bytes) to the bank and return the raw response bytes.
    pub async fn send(&self, message_bytes: &[u8]) -> Result<Vec<u8>> {
        // Base64-encode the message
        let encoded = B64.encode(message_bytes);

        debug!(
            "Sending {} bytes (base64: {} bytes) to {}",
            message_bytes.len(),
            encoded.len(),
            self.url
        );
        trace!(
            "Request (raw): {:?}",
            String::from_utf8_lossy(message_bytes)
        );

        // POST with Content-Type: text/plain
        let response = self
            .client
            .post(&self.url)
            .header("Content-Type", "text/plain")
            .body(encoded)
            .send()
            .await
            .map_err(|e| FinTSError::Transport(format!("HTTP request failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(FinTSError::Http {
                status: status.as_u16(),
                message: body,
            });
        }

        // Response body: ISO-8859-1 text containing base64 data
        let response_bytes = response
            .bytes()
            .await
            .map_err(|e| FinTSError::Transport(format!("Failed to read response: {}", e)))?;

        // Decode from ISO-8859-1 (treat as raw bytes, strip any whitespace)
        let response_text: String = response_bytes
            .iter()
            .map(|&b| b as char)
            .filter(|c| !c.is_whitespace())
            .collect();

        debug!("Received {} bytes base64 response", response_text.len());

        // Base64-decode
        let decoded = B64.decode(response_text.as_bytes()).map_err(|e| {
            FinTSError::Transport(format!("Failed to base64-decode response: {}", e))
        })?;

        debug!("Response: {} bytes decoded", decoded.len());
        trace!(
            "Response (raw decoded): {}",
            String::from_utf8_lossy(&decoded)
        );

        Ok(decoded)
    }
}
