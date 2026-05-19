use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    pub ssid: String,
    pub signal: u8,
    pub security: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConnectionRequest {
    pub ssid: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectAccepted {
    pub status: &'static str,
    pub message: &'static str,
}
