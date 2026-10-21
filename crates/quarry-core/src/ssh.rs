//! "Over SSH": a local port forwarded through an SSH server to the database.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use russh::client;
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshConfig {
    pub host: String,
    /// 0 means 22.
    pub port: u16,
    pub user: String,
    /// Kept in the OS keychain.
    #[serde(skip_serializing)]
    pub password: Option<String>,
    /// Private key file; empty = password authentication.
    pub key_path: String,
    /// Kept in the OS keychain.
    #[serde(skip_serializing)]
    pub key_passphrase: Option<String>,
}

impl SshConfig {
    pub fn port_or_default(&self) -> u16 {
        if self.port == 0 { 22 } else { self.port }
    }
}

/// Our own trust-on-first-use host key file; `~/.ssh/known_hosts` is only read.
fn quarry_known_hosts() -> PathBuf {
    crate::store::config_dir().join("known_hosts")
}

fn user_known_hosts() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

struct HostCheck {
    host: String,
    port: u16,
}

impl client::Handler for HostCheck {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        let (host, port) = (self.host.as_str(), self.port);
        if let Some(path) = user_known_hosts().filter(|p| p.exists()) {
            match keys::check_known_hosts_path(host, port, key, &path) {
                Ok(true) => return Ok(true),
                Err(keys::Error::KeyChanged { .. }) => return Ok(false),
                _ => {}
            }
        }
        let ours = quarry_known_hosts();
        match keys::check_known_hosts_path(host, port, key, &ours) {
            Ok(true) => Ok(true),
            // A different key than the one seen before: possible man in the middle.
            Err(keys::Error::KeyChanged { .. }) => Ok(false),
            _ => {
                let _ = keys::known_hosts::learn_known_hosts_path(host, port, key, &ours);
                Ok(true)
            }
        }
    }
}

/// Open while held; dropping it closes the forward.
pub struct Tunnel {
    pub local_port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(path),
    }
}

/// Connects to the SSH server and forwards `127.0.0.1:<local_port>` to
/// `remote_host:remote_port` as seen from that server.
pub async fn open(ssh: &SshConfig, remote_host: &str, remote_port: u16) -> Result<Tunnel> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    });
    let port = ssh.port_or_default();
    let check = HostCheck { host: ssh.host.clone(), port };
    let mut handle = tokio::time::timeout(Duration::from_secs(15), client::connect(config, (ssh.host.as_str(), port), check))
        .await
        .context("SSH connection timed out")?
        .with_context(|| format!("could not connect to SSH server {}:{port} (or its host key changed)", ssh.host))?;
    let auth = if ssh.key_path.trim().is_empty() {
        handle.authenticate_password(&ssh.user, ssh.password.clone().unwrap_or_default()).await?
    } else {
        let key = keys::load_secret_key(expand_home(ssh.key_path.trim()), ssh.key_passphrase.as_deref())
            .with_context(|| format!("could not load SSH key {}", ssh.key_path))?;
        let hash = handle.best_supported_rsa_hash().await?.flatten();
        handle.authenticate_publickey(&ssh.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash)).await?
    };
    if !auth.success() {
        bail!("SSH authentication failed for {}@{}", ssh.user, ssh.host);
    }
    // Probe once so a refused forward is reported here, not as a dropped connection.
    handle
        .channel_open_direct_tcpip(remote_host, remote_port as u32, "127.0.0.1", 0)
        .await
        .with_context(|| format!("the SSH server refused to forward to {remote_host}:{remote_port}"))?
        .close()
        .await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local_port = listener.local_addr()?.port();
    let handle = Arc::new(handle);
    let remote_host = remote_host.to_string();
    let task = tokio::spawn(async move {
        while let Ok((mut socket, peer)) = listener.accept().await {
            let (handle, host) = (handle.clone(), remote_host.clone());
            tokio::spawn(async move {
                let channel = handle
                    .channel_open_direct_tcpip(host, remote_port as u32, peer.ip().to_string(), peer.port() as u32)
                    .await;
                if let Ok(channel) = channel {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                }
            });
        }
    });
    Ok(Tunnel { local_port, task })
}
