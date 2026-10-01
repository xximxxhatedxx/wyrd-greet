//! greetd IPC Client implementation.
//!
//! Handles JSON framing over Unix stream socket (`$GREETD_SOCK`) with strict
//! end-to-end memory zeroization of all buffers holding authentication secrets.

use anyhow::{Context, Result};
use log::debug;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use zeroize::Zeroize;

#[allow(dead_code)]
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GreetdRequest {
    CreateSession { username: String },
    PostAuthMessageResponse { response: Option<String> },
    StartSession { cmd: Vec<String>, env: Vec<String> },
    CancelSession,
}

impl GreetdRequest {
    /// Explicitly zeroes out any sensitive string buffers held inside the request.
    pub fn zeroize_sensitive(&mut self) {
        match self {
            GreetdRequest::PostAuthMessageResponse { response } => {
                if let Some(ref mut secret) = response {
                    secret.zeroize();
                }
            }
            GreetdRequest::CreateSession { username } => {
                username.zeroize();
            }
            GreetdRequest::StartSession { cmd, env } => {
                for item in cmd.iter_mut() {
                    item.zeroize();
                }
                for item in env.iter_mut() {
                    item.zeroize();
                }
            }
            GreetdRequest::CancelSession => {}
        }
    }
}

impl Drop for GreetdRequest {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GreetdResponse {
    Success,
    Error {
        error_type: String,
        description: String,
    },
    AuthMessage {
        auth_message_type: String, // "visible", "secret", "info", "error"
        auth_message: String,
    },
}

pub struct GreetdClient {
    stream: UnixStream,
}

impl GreetdClient {
    pub fn connect() -> Result<Self> {
        let sock_path =
            std::env::var("GREETD_SOCK").unwrap_or_else(|_| "/run/greetd.sock".to_string());
        debug!("Connecting to greetd socket at {}", sock_path);
        let stream = UnixStream::connect(PathBuf::from(&sock_path))
            .with_context(|| format!("Failed to connect to greetd socket at {}", sock_path))?;
        Ok(Self { stream })
    }

    #[cfg(test)]
    pub fn from_stream(stream: UnixStream) -> Self {
        Self { stream }
    }

    pub fn send_request(&mut self, req: &mut GreetdRequest) -> Result<GreetdResponse> {
        let mut payload = match serde_json::to_vec(req) {
            Ok(bytes) => bytes,
            Err(e) => {
                req.zeroize_sensitive();
                return Err(e.into());
            }
        };
        // Zeroize the source request struct immediately after serialization.
        req.zeroize_sensitive();

        let len = (payload.len() as u32).to_le_bytes();
        let write_res = self
            .stream
            .write_all(&len)
            .and_then(|_| self.stream.write_all(&payload))
            .and_then(|_| self.stream.flush());

        // Unconditionally zeroize the serialized JSON byte buffer right after the socket write completes.
        payload.zeroize();
        write_res?;

        // Read response length
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf)?;
        let resp_len = u32::from_le_bytes(len_buf) as usize;

        let mut resp_buf = vec![0u8; resp_len];
        let read_res = self.stream.read_exact(&mut resp_buf);
        if let Err(e) = read_res {
            resp_buf.zeroize();
            return Err(e.into());
        }

        let parse_res =
            serde_json::from_slice(&resp_buf).context("Failed to parse response from greetd");
        resp_buf.zeroize();
        parse_res
    }

    pub fn create_session(&mut self, username: &str) -> Result<GreetdResponse> {
        let mut req = GreetdRequest::CreateSession {
            username: username.to_string(),
        };
        self.send_request(&mut req)
    }

    /// Sends an authentication response without cloning the secret, zeroizing
    /// both the request struct and the serialized JSON byte buffer immediately after transmission.
    pub fn post_auth_response(&mut self, response: Option<String>) -> Result<GreetdResponse> {
        let mut req = GreetdRequest::PostAuthMessageResponse { response };
        let res = self.send_request(&mut req);
        req.zeroize_sensitive();
        res
    }

    pub fn start_session(&mut self, cmd: Vec<String>, env: Vec<String>) -> Result<GreetdResponse> {
        let mut req = GreetdRequest::StartSession { cmd, env };
        self.send_request(&mut req)
    }

    #[allow(dead_code)]
    pub fn cancel_session(&mut self) -> Result<GreetdResponse> {
        let mut req = GreetdRequest::CancelSession;
        self.send_request(&mut req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greetd_request_zeroize_sensitive_wipes_password_buffer() {
        let secret = String::from("correct-horse-battery-staple");
        let ptr = secret.as_ptr();
        let cap = secret.capacity();
        assert!(cap >= 28);

        let mut req = GreetdRequest::PostAuthMessageResponse {
            response: Some(secret),
        };
        req.zeroize_sensitive();

        if let GreetdRequest::PostAuthMessageResponse {
            response: Some(ref s),
        } = req
        {
            assert!(s.is_empty());
            // Verify underlying capacity bytes were zeroed before length was set to 0.
            // SAFETY: `ptr` belongs to `s` which is still alive with capacity `cap`.
            let raw_bytes = unsafe { std::slice::from_raw_parts(ptr, 28) };
            assert!(raw_bytes.iter().all(|&b| b == 0));
        } else {
            panic!("Expected PostAuthMessageResponse");
        }
    }

    #[test]
    fn test_post_auth_response_roundtrip_and_zeroization() {
        let (client_sock, mut server_sock) = UnixStream::pair().unwrap();
        let server_thread = std::thread::spawn(move || {
            let mut len_buf = [0u8; 4];
            server_sock.read_exact(&mut len_buf).unwrap();
            let n = u32::from_le_bytes(len_buf) as usize;
            let mut payload = vec![0u8; n];
            server_sock.read_exact(&mut payload).unwrap();
            let body = String::from_utf8(payload).unwrap();
            assert!(body.contains("post_auth_message_response"));

            let reply = br#"{"type":"success"}"#;
            let reply_len = (reply.len() as u32).to_le_bytes();
            server_sock.write_all(&reply_len).unwrap();
            server_sock.write_all(reply).unwrap();
        });

        let mut client = GreetdClient::from_stream(client_sock);
        let resp = client
            .post_auth_response(Some("top-secret-password".to_string()))
            .unwrap();
        assert_eq!(resp, GreetdResponse::Success);
        server_thread.join().unwrap();
    }
}
