use anyhow::Result;
use tokio::task::JoinHandle;
use log::*;
use crate::irc;
use crate::bot;
use crate::plugins::{Plugin, PluginBuilder};

pub struct EchoPlugin;

impl PluginBuilder for EchoPlugin {
    const NAME: &'static str = "echo";
    type Plugin = EchoPlugin;

    fn new(_server: &str, _config: Option<&bot::PluginConfig>) -> Result<EchoPlugin> {
        Ok(EchoPlugin)
    }
}

impl Plugin for EchoPlugin {
    fn spawn_task(self, mut irc: irc::IRC) -> Result<JoinHandle<Result<()>>> {
        info!("Registering echo");
        let handle = tokio::spawn(async move {
            loop {
                while let Ok(msg) = irc.received_messages.recv().await {
                    if let irc::Command::Privmsg = msg.command {
                        assert!(msg.parameters.len() == 1);
                        assert!(msg.target.is_some());
                        let user = msg.source_as_user().unwrap();
                        let target = msg.target.unwrap();
                        let reply = format!("Hey {:?} thanks for saying `{}'! Much appreciated", user, msg.parameters[0]);
                        irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                    }
                }
            }
        });
        Ok(handle)
    }
}
