use std::{fs::File, path::Path};

use anyhow::Result;
use tokio::task::JoinHandle;
use ron::de::from_reader;
use serde::Deserialize;
use log::*;

use crate::irc;

/// Global configuration, including possibly many bots
#[derive(Debug, Deserialize)]
pub struct Config {
    bots: Vec<Bot>,
}

/// Configuration for one instance of the bot
#[derive(Debug, Deserialize)]
struct Bot {
    /// Hostname and port of IRC server
    server: (String, u16),
    /// Whether TLS should be used
    use_tls: bool,
    // TODO:
    // /// Whether the server TLS certificate should be validated (using system store)
    // validate_cert: bool,

    /// Bot nickname
    nick: String,
    /// Bot ident/username
    ident: String,
    /// Bot realname
    real_name: String,

    /// Channels to join after connecting and remain joined
    channels: Vec<String>,
}

impl Bot {
    // TODO handle reconnection

    // TODO try to go back to old nick if changed
    // TODO handle kicks/parts/whatever and rejoin?

    async fn spawn_tasks(self) -> Result<JoinHandle<Result<()>>> {
        info!("Starting bot for {}", self.server.0);
        let handle = tokio::spawn(async move {
            let (mut irc, _irc_handle) = if self.use_tls {
                irc::connect_tls(&self.server, self.server.0.as_str()).await?
            } else {
                irc::connect(&self.server).await?
            };

            irc.authenticate(self.nick, self.ident, self.real_name).await?;

            loop {
                while let Ok(msg) = irc.received_messages.recv().await {
                    match msg.command {
                        irc::Command::Ping => irc.reply_pong(msg).await?,
                        irc::Command::ErrNicknameInUse => irc.reply_nick_in_use(msg).await?,
                        irc::Command::RplWelcome => irc.join(&self.channels).await?,
                        _ => debug!("[{}] Ignoring {:?}", self.server.0, msg),
                    }
                }
            }
        });
        Ok(handle)
    }
}

impl Config {
    pub fn load_from<P: AsRef<Path>>(path: P) -> Result<Config> {
        let file = File::open(&path)?;
        let config: Config = from_reader(file)?;
        Ok(config)
    }

    pub async fn spawn_tasks(self) -> Result<Vec<JoinHandle<Result<()>>>> {
        let mut handles = vec![];
        for bot in self.bots {
            handles.push(bot.spawn_tasks().await?);
        }
        Ok(handles)
    }
}
