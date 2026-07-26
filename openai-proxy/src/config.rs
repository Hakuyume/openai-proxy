use crate::Error;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone, Debug, serde::Deserialize)]
pub enum Bind {
    #[serde(rename = "tcp")]
    Tcp(SocketAddr),
    #[serde(rename = "unix")]
    Unix(PathBuf),
}

impl Bind {
    pub fn bind(self) -> impl Future<Output = io::Result<tokio_net_incoming::Listener>> {
        let bind = match self {
            Bind::Tcp(bind) => tokio_net_incoming::OneOf::Tcp(bind),
            Bind::Unix(bind) => tokio_net_incoming::OneOf::Unix(bind),
        };
        tokio_net_incoming::Listener::bind(bind)
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub enum Authorization {
    #[serde(rename = "bearer")]
    Bearer(Bearer),
}

#[derive(Clone, Debug, serde::Deserialize)]
pub enum Bearer {
    #[serde(rename = "token_path")]
    TokenPath(PathBuf),
}

impl Authorization {
    pub async fn value(&self) -> Result<String, Error> {
        match self {
            Self::Bearer(Bearer::TokenPath(path)) => {
                Ok(format!("Bearer {}", tokio::fs::read_to_string(path).await?))
            }
        }
    }
}
