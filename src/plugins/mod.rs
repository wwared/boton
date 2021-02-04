use async_trait::async_trait;
use anyhow::Result;
use tokio::task::JoinHandle;

use crate::irc;
use crate::bot;

pub mod echo;
pub mod weather;


use std::collections::HashMap;
pub async fn spawn_plugins(irc: &irc::IRC, config: HashMap<String, bot::PluginConfig>) -> Result<HashMap<String, JoinHandle<Result<()>>>> {
    macro_rules! spawn_plugin {
        ($p:ident, $ty:ty) => {
            let plug = <$ty>::new(&irc.server, config.get(<$ty>::NAME)).await?;
            let plug = plug.spawn_task(irc.clone())?;
            $p.insert(<$ty>::NAME.into(), plug);
        }
    }

    let mut plugins = HashMap::new();
    // spawn_plugin!(plugins, echo::EchoPlugin);
    spawn_plugin!(plugins, weather::WeatherPlugin);
    Ok(plugins)
}

// TODO figure out some way of managing errors from plugins
// TODO logging and auto-respawning the plugin tasks if they die for whatever reason

#[async_trait]
pub trait PluginBuilder {
    const NAME: &'static str;
    type Plugin;

    async fn new(server: &str, config: Option<&bot::PluginConfig>) -> Result<Self::Plugin>;
}

pub trait Plugin {
    fn spawn_task(self, irc: irc::IRC) -> Result<JoinHandle<Result<()>>>;
}
