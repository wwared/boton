use std::{fs::File, path::Path};

use anyhow::{anyhow, Result};
use tokio::task::JoinHandle;
use ron::de::from_reader;
use serde::Deserialize;
use log::*;
use std::collections::HashMap;

use crate::irc;
use crate::plugins;

/// Arbitrary optional configuration for a given plugin
pub type PluginConfig = HashMap<String, String>;

/// Global configuration, including possibly many bots
#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    bots: Vec<Bot>,
    plugins: HashMap<String, PluginConfig>,
}

/// Configuration for one instance of the bot
#[derive(Debug, Deserialize, Clone)]
struct Bot {
    /// Hostname and port of IRC server
    server: (String, u16),
    /// Whether TLS should be used
    use_tls: bool,
    // /// Whether the server TLS certificate should be validated (using system store)
    // validate_cert: bool, // TODO

    /// Bot nickname
    nick: String,
    /// Bot ident/username
    ident: String,
    /// Bot realname
    real_name: String,

    /// Channls to join after connecting and remain joined
    channels: Vec<String>,
}

impl Bot {
    // TODO try to go back to old nick if changed
    // TODO handle kicks/parts/whatever and rejoin?

    pub async fn spawn_tasks(self, plugin_configs: HashMap<String, PluginConfig>) -> Result<JoinHandle<Result<()>>> {
        let server = self.server.0.clone();
        info!("[{}] Starting bot", server);
        let handle = tokio::spawn((async move || -> Result<()> {
            let (mut irc, irc_handle) = if self.use_tls {
                irc::connect_tls(server.as_str(), &self.server, self.server.0.as_str()).await?
            } else {
                irc::connect(server.as_str(), &self.server).await?
            };

            info!("[{}] Loading plugins", server);
            let plugs = plugins::spawn_plugins(&irc, plugin_configs).await?;

            let send_handle = tokio::spawn((async move || -> Result<()> {
                irc.authenticate(self.nick, self.ident, self.real_name).await?;

                loop {
                    while let Ok(msg) = irc.received_messages.recv().await {
                        match msg.command {
                            irc::Command::Ping => irc.reply_pong(msg).await?,
                            irc::Command::ErrNicknameInUse => irc.reply_nick_in_use(msg).await?,
                            irc::Command::RplWelcome => irc.join(&self.channels).await?,
                            _ => trace!("[{}] Ignoring {:?}", server, msg),
                        }
                    }
                    debug!("bot task got a recv_msgs error =========loop=========");
                }
            })());

            let res = irc_handle.await?;
            debug!("irc task exited: {:?}", res);
            for (_, handle) in plugs.iter() {
                handle.abort();
            }
            send_handle.abort();
            res
        })());
        Ok(handle)
    }
}

impl Config {
    pub fn load_from<P: AsRef<Path>>(path: P) -> Result<Config> {
        let file = File::open(&path)?;
        let config: Config = from_reader(file)?;
        Ok(config)
    }

    pub async fn spawn_tasks(&self) -> Result<Vec<JoinHandle<Result<()>>>> {
        let mut handles = vec![];
        for bot in self.bots.clone() {
            handles.push((bot.server.0.clone(), bot.spawn_tasks(self.plugins.clone()).await?));
        }
        let mut reconnection_handles = vec![];
        for (server, mut handle) in handles {
            let bots = self.clone();
            reconnection_handles.push(tokio::spawn((async move || -> Result<()> {
                while handle.await?.is_err() {
                    info!("[{}] Connection closed, restarting bot...", server);
                    handle = bots.spawn_task(&server).await?;
                }
                info!("[{}] Closed cleanly, shutting down bot...", server);
                Ok(())
            })()));
        }
        Ok(reconnection_handles)
    }

    pub async fn spawn_task(&self, server: &str) -> Result<JoinHandle<Result<()>>> {
        let bot = self.bots.iter().find(|b| b.server.0 == server).map(Ok).unwrap_or_else(|| Err(anyhow!("could not find server {}", server)))?;
        Ok(bot.clone().spawn_tasks(self.plugins.clone()).await?)
    }
}
