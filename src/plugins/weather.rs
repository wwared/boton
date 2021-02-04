use async_trait::async_trait;
use anyhow::{anyhow, Result};
use tokio::fs::{File, read_to_string};
use tokio::task::JoinHandle;
use tokio::sync::RwLock;
use tokio::io::AsyncWriteExt;
use log::*;
use std::collections::HashMap;
use std::sync::Arc;
use chrono::{naive::NaiveDateTime, DateTime, Utc, FixedOffset, Duration};
use ron::de::from_str;
use ron::ser::to_string;
use serde::{Deserialize, Serialize};
use crate::irc;
use crate::bot;
use crate::plugins::{Plugin, PluginBuilder};

#[derive(Debug, Deserialize, Serialize, Clone)]
enum Speed {
    MPH,
    KMH,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
enum Temperature {
    Celsius,
    Fahrenheit,
}

type Units = (Temperature, Speed);
const IMPERIAL: Units = (Temperature::Fahrenheit, Speed::MPH);
const METRIC: Units = (Temperature::Celsius, Speed::KMH);

#[derive(Debug, Deserialize, Serialize, Clone)]
struct UserConfig {
    location: Option<String>,
    units: Option<Units>,
}
type WeatherDB = RwLock<HashMap<String, UserConfig>>;

#[derive(Clone)]
pub struct WeatherPlugin {
    user_db: Arc<WeatherDB>,
    http_client: reqwest::Client,
    openweathermap_apikey: String,
}

// TODO support lat/lon queries too?
enum OWMQuery<'a> {
    Simple(&'a str),
    Id(&'a str),
    USZip(&'a str),
}

use std::fmt;
use fmt::Display;
impl<'a> Display for OWMQuery<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OWMQuery::Simple(query) => write!(f, "q={}", query),
            OWMQuery::Id(id)        => write!(f, "id={}", id),
            OWMQuery::USZip(zip)    => write!(f, "zip={}", zip),
        }
    }
}

impl WeatherPlugin {
    fn db_path(server: &str) -> String {
        format!("data/{}-weather", server)
    }

    async fn load_db(server: &str) -> Result<WeatherDB> {
        let db_path = WeatherPlugin::db_path(server);
        let data = read_to_string(&db_path).await?;
        let user_db: HashMap<String, UserConfig> = from_str(&data)?;
        Ok(RwLock::new(user_db))
    }

    async fn save_db(&self, server: &str) -> Result<()> {
        let db_path = WeatherPlugin::db_path(server);
        let mut file = File::create(&db_path).await?;
        let user_db = self.user_db.read().await;
        let data = to_string(&*user_db)?;
        file.write_all(data.as_bytes()).await?;
        Ok(())
    }

    async fn get_user_config(&self, nick: &str) -> Option<UserConfig> {
        let user_db = self.user_db.read().await;
        user_db.get(nick).cloned()
    }

    async fn set_user_units(&self, nick: &str, units: Option<Units>) {
        let mut user_db = self.user_db.write().await;
        let mut delete = false;
        if let Some(user_conf) = user_db.get_mut(nick) {
            if units.is_none() && user_conf.location.is_none() { delete = true; }
            user_conf.units = units;
        } else if units.is_some() {
            user_db.insert(nick.into(), UserConfig {
                location: None,
                units,
            });
        }
        if delete {
            user_db.remove(nick);
        }
    }

    async fn set_user_location(&self, nick: &str, location: Option<String>) {
        let mut user_db = self.user_db.write().await;
        let mut delete = false;
        if let Some(user_conf) = user_db.get_mut(nick) {
            if location.is_none() && user_conf.units.is_none() { delete = true; }
            user_conf.location = location;
        } else if location.is_some() {
            user_db.insert(nick.into(), UserConfig {
                units: None,
                location,
            });
        }
        if delete {
            user_db.remove(nick);
        }
    }
}

#[async_trait]
impl PluginBuilder for WeatherPlugin {
    const NAME: &'static str = "weather";
    type Plugin = WeatherPlugin;

    async fn new(server: &str, config: Option<&bot::PluginConfig>) -> Result<WeatherPlugin> {
        if config.is_none() {
            return Err(anyhow!("Weather plugin requires `openweathermap-apikey` in its config section"));
        }
        let config = config.unwrap();
        // TODO get rid of these clones
        let openweathermap_apikey = config.get("openweathermap-apikey").expect("[Weather] Missing `openweathermap-apikey`").clone();

        let http_client = reqwest::Client::builder()
            .connect_timeout(Duration::seconds(10).to_std()?)
            .connection_verbose(true)
            .build()?;

        if let Ok(user_db) = WeatherPlugin::load_db(server).await {
            info!("[{}] Weather DB loaded successfully", server);
            debug!("{:?}", user_db);
            Ok(WeatherPlugin {
                openweathermap_apikey,
                http_client,
                user_db: Arc::new(user_db),
            })
        } else {
            warn!("[{}] Weather DB not found", server);
            Ok(WeatherPlugin {
                openweathermap_apikey,
                http_client,
                user_db: Arc::new(RwLock::new(HashMap::new())),
            })
        }
    }
}

fn split_first_word(text: &str) -> (&str, Option<&str>) {
    if let Some(space) = text.find(' ') {
        (&text[..space], Some(&text[space+1..]))
    } else {
        (text, None)
    }
}

mod unix_ts {
    use serde::{self, Deserialize, Deserializer};
    use chrono::{DateTime, TimeZone, Utc};

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        i64::deserialize(deserializer).map(|t| Utc.timestamp(t, 0))
    }
}

// structs taken from https://github.com/ddboline/weather_util_rust
#[derive(Deserialize, Debug, Clone)]
struct Coord {
    lon: f64,
    lat: f64,
}

impl Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "&lat={}&lng={}", self.lat, self.lon)
    }
}

#[derive(Deserialize, Debug, Clone)]
struct WeatherCond {
    main: String,
    description: String,
    icon: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct WeatherMain {
    temp: f64, // K
    feels_like: f64, // K
    temp_min: f64, // K
    temp_max: f64, // K
    pressure: f64, // hPa
    humidity: f64, // %
}

#[derive(Deserialize, Debug, Clone)]
struct Wind {
    speed: f64, // m/s
    gust: Option<f64>, // m/s
    deg: Option<f64>, // °
}

#[derive(Deserialize, Debug, Clone)]
struct Sys {
    country: Option<String>,
    #[serde(with = "unix_ts")]
    sunrise: DateTime<Utc>,
    #[serde(with = "unix_ts")]
    sunset: DateTime<Utc>,
}

#[derive(Deserialize, Debug, Clone)]
struct Rain {
    #[serde(alias = "1h")]
    volume: f64, // mm
    #[serde(alias = "3h")]
    three_hour: Option<f64>, // mm
}

#[derive(Deserialize, Debug, Clone)]
struct Snow {
    #[serde(alias = "1h")]
    volume: f64, // mm
    #[serde(alias = "3h")]
    three_hour: Option<f64>, // mm
}

#[derive(Deserialize, Debug, Clone)]
struct Cloud {
    #[serde(alias = "all")]
    cloudiness: f64, // %
}

#[derive(Deserialize, Debug, Clone)]
struct WeatherData {
    coord: Coord,
    weather: Vec<WeatherCond>,
    base: String,
    main: WeatherMain,
    visibility: Option<f64>,
    wind: Wind,
    clouds: Cloud,
    rain: Option<Rain>,
    snow: Option<Snow>,
    #[serde(with = "unix_ts")]
    dt: DateTime<Utc>,
    sys: Sys,
    timezone: i32,
    name: String,
}

impl Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Speed::KMH => write!(f, "Km/h"),
            Speed::MPH => write!(f, "mph"),
        }
    }
}

impl Display for Temperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Temperature::Celsius    => write!(f, "°C"),
            Temperature::Fahrenheit => write!(f, "°F"),
        }
    }
}


impl WeatherData {
    fn convert_temp(kelvin: f64, format: &Temperature) -> f64 {
        match format {
            Temperature::Celsius => {
                kelvin - 273.15
            },
            Temperature::Fahrenheit => {
                (kelvin - 273.15) * 9./5. + 32.
            },
        }
    }

    fn convert_speed(meters_per_sec: f64, format: &Speed) -> f64 {
        match format {
            Speed::KMH => { meters_per_sec * 3.6 },
            Speed::MPH => { meters_per_sec * 2.237 },
        }
    }

    fn convert_wind_dir(degrees: f64) -> Result<&'static str> {
        if      (0.0   ..= 22.5).contains(&degrees) { Ok(" N") }
        else if (22.5  ..= 67.5).contains(&degrees) { Ok(" NE") }
        else if (67.5  ..= 112.).contains(&degrees) { Ok(" E") }
        else if (112.5 ..= 157.).contains(&degrees) { Ok(" SE") }
        else if (157.5 ..= 202.).contains(&degrees) { Ok(" S") }
        else if (202.5 ..= 247.).contains(&degrees) { Ok(" SW") }
        else if (247.5 ..= 292.).contains(&degrees) { Ok(" W") }
        else if (292.5 ..= 337.).contains(&degrees) { Ok(" NW") }
        else if (337.5 ..= 360.).contains(&degrees) { Ok(" N") }
        else { Err(anyhow!("Wind direction out of range")) }
    }

    fn get_icon(icon: &str) -> Result<&'static str> {
        match icon {
            "01d" => { Ok("\u{2600}\u{FE0F}") }, // sun
            "01n" => { Ok("\u{1F319}") }, // moon
            "02d" => { Ok("\u{26C5}") }, // sun behind cloud
            "03d" | "04d" | "02n" | "03n" | "04n" => { Ok("\u{2601}\u{FE0F}") }, // cloud
            "09d" | "09n" | "10n" => { Ok("\u{1F327}\u{FE0F}") }, // cloud with rain
            "10d" => { Ok("\u{1F326}\u{FE0F}") }, // sun behind cloud with rain
            "11d" | "11n" => { Ok("\u{1F329}\u{FE0F}") }, // cloud with lightning
            "13d" | "13n" => { Ok("\u{1F328}\u{FE0F}") }, // cloud with snow
            "50d" | "50n" => { Ok("\u{1F32B}\u{FE0F}") }, // fog
            _ => { Err(anyhow!("Unknown icon value `{}`", icon)) }
        }
    }

    // TODO air pollution too?
    fn print_data(&self, units: Option<Units>, nick: Option<String>) -> String {
        let country = self.sys.country.clone().unwrap_or_else(|| "??".into());
        let units = if let Some(units) = units { units } else if country == "US" { IMPERIAL } else { METRIC };
        let prefix = nick.unwrap_or_else(|| format!("{}, {}", self.name, country));
        let (temp, min, max, feels) = (
            WeatherData::convert_temp(self.main.temp, &units.0),
            WeatherData::convert_temp(self.main.temp_min, &units.0),
            WeatherData::convert_temp(self.main.temp_max, &units.0),
            WeatherData::convert_temp(self.main.feels_like, &units.0),
        );
        let temperature = format!("{:.1} {} · {:.1}⌄ {:.1}⌃ (feels like {:.1})", temp, units.0, min, max, feels);
        let description = if let Some(icon) = &self.weather[0].icon {
            let icon = WeatherData::get_icon(icon).unwrap();
            format!(" 〜 {} {}", icon, self.weather[0].description)
        } else {
            format!(" 〜 {}", self.weather[0].description)
        };
        let humidity = format!(" 〜 \u{1F4A7} {}%", self.main.humidity);
        let wind_dir = if let Some(deg) = self.wind.deg {
            WeatherData::convert_wind_dir(deg).unwrap()
        } else {
            ""
        };
        let wind_speed = WeatherData::convert_speed(self.wind.speed, &units.1);
        let wind = format!(" 〜 \u{1F4A8} {:.1} {}{}", wind_speed, units.1, wind_dir);

        format!("Weather for {}: {}{}{}{}", prefix, temperature, description, humidity, wind)
    }
}

mod geoname_time {
    use serde::{self, Deserialize, Deserializer};
    use chrono::naive::NaiveDateTime;

    const FORMAT: &str = "%Y-%m-%d %H:%M";

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<NaiveDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveDateTime::parse_from_str(&s, FORMAT).map_err(serde::de::Error::custom)
    }
}

#[derive(Deserialize, Debug, Clone)]
struct TimeData {
    #[serde(with = "geoname_time")]
    sunrise: NaiveDateTime,
    #[serde(with = "geoname_time")]
    sunset: NaiveDateTime,

    #[serde(with = "geoname_time")]
    time: NaiveDateTime,
}

impl WeatherPlugin {
    async fn get_openweathermap(&self, query: OWMQuery<'_>) -> Result<WeatherData> {
        let url = format!("https://api.openweathermap.org/data/2.5/weather?APPID={}&{}", self.openweathermap_apikey, query);
        let json: WeatherData = self.http_client.get(&url).send().await?.json().await?;
        debug!("Weather data:\n{:#?}", json);
        Ok(json)
    }
}

// TODO use more data and reformat stuff; remove temp_min and temp_max
// TODO factor out the code into functions and organize stuff better
// TODO configurable and global command prefix (for the factored privmsg handling; move it out of this file)
// TODO convenience function for sending a privmsg in IRC
impl Plugin for WeatherPlugin {
    fn spawn_task(self, mut irc: irc::IRC) -> Result<JoinHandle<Result<()>>> {
        let handle = tokio::spawn(async move {
            loop {
                while let Ok(msg) = irc.received_messages.recv().await {
                    if let irc::Command::Privmsg = msg.command {
                        let plugin = self.clone();
                        let irc = irc.clone();
                        // TODO maybe some "standard" plugin way of spawning tasks/registering handles
                        // so when the plugin gets cancelled/restarted they can be aborted?
                        // doesn't really matter in the weather plugin case i believe
                        tokio::spawn(async move {
                            if msg.target.is_none() || msg.parameters.len() != 1 {
                                error!("Unexpected PRIVMSG format, ignoring");
                                return;
                            }

                            // TODO ideally this only happens if theres a command
                            let user = if let Some(user) = msg.source_as_user() { user } else {
                                error!("PRIVMSG without user, ignoring");
                                return;
                            };
                            let target = msg.target.unwrap();

                            let (cmd, msg) = split_first_word(&msg.parameters[0]);
                            match cmd {
                                r"\w" | r"\t" => {
                                    let nick = user.nick.to_lowercase();

                                    let user_units = if let Some(UserConfig { location: _, units: Some(units) }) = plugin.get_user_config(&nick).await {
                                        Some(units)
                                    } else {
                                        None
                                    };

                                    let (query_string, target_nick) = if let Some(msg) = msg {
                                        if let Some(target_nick) = msg.strip_prefix("@") {
                                            let target_nick = target_nick.to_lowercase();
                                            if let Some(UserConfig { location: Some(user_loc), units: _ }) = plugin.get_user_config(&target_nick).await {
                                                (user_loc, Some(target_nick))
                                            } else {
                                                let reply = format!("{}: Could not find saved weather location for `{}`", nick, target_nick);
                                                irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();
                                                return;
                                            }
                                        } else {
                                            (msg.to_owned(), None)
                                        }
                                    } else {
                                        // no message, look up in user_db
                                        if let Some(UserConfig { location: Some(user_loc), units: _ }) = plugin.get_user_config(&nick).await {
                                            (user_loc, Some(nick.clone()))
                                        } else {
                                            let reply = format!("{}: Inform a city, or optionally set a city using \\wset. Accepted formats: `city`, `city, country` (ISO country code), US zip codes, `id:1234` (OpenWeatherMap ID)", nick);
                                            irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();
                                            return;
                                        }
                                    };

                                    let query = if let Some(id) = query_string.strip_prefix("id:") {
                                        OWMQuery::Id(id)
                                    } else if query_string.chars().all(|c| c.is_ascii_digit()) {
                                        OWMQuery::USZip(&query_string)
                                    } else {
                                        OWMQuery::Simple(&query_string)
                                    };

                                    let weather = plugin.get_openweathermap(query).await;
                                    let weather_data = if let Ok(data) = weather {
                                        data
                                    } else {
                                        debug!("Weather error: query_string: {}, response: {:?}", query_string, weather);
                                        let reply = format!("{}: Could not get weather, sorry! Maybe the query is invalid?", nick);
                                        irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();
                                        return;
                                    };

                                    if cmd == r"\w" {
                                        let reply = weather_data.print_data(user_units, target_nick);
                                        irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();
                                    } else if cmd == r"\t" {
                                        let current_time = Utc::now().with_timezone(&FixedOffset::east(weather_data.timezone));

                                        let geoplace = if let Some(target_nick) = target_nick {
                                            format!("for {}", target_nick)
                                        } else {
                                            format!("in {}, {}", weather_data.name, weather_data.sys.country.unwrap())
                                        };
                                        let reply = format!("The curent date and time {} is {}", geoplace, current_time);
                                        irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();
                                    }
                                },
                                r"\wset" => {
                                    let nick = user.nick.to_lowercase();
                                    let reply = if let Some(msg) = msg {
                                        let reply = format!("{}: Updated your saved weather location to `{}`", nick, msg);
                                        plugin.set_user_location(&nick, Some(msg.into())).await;
                                        reply
                                    } else {
                                        let reply = format!("{}: Removed your saved weather location", nick);
                                        plugin.set_user_location(&nick, None).await;
                                        reply
                                    };
                                    irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();

                                    // TODO improve this ugly ass part
                                    plugin.save_db(&irc.server).await.unwrap();
                                },
                                r"\units" => {
                                    let nick = user.nick.to_lowercase();
                                    let reply = if let Some(msg) = msg {
                                        let units = match msg.to_lowercase().as_str() {
                                            "metric" => METRIC,
                                            "imperial" => IMPERIAL,
                                            _ => {
                                                let reply = format!("{}: Use \\units [metric|imperial] to set your saved preference", user.nick);
                                                irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();
                                                return;
                                            },
                                        };
                                        let reply = format!("{}: Updated your saved units preference to `{:?}`", nick, units);
                                        plugin.set_user_units(&nick, Some(units)).await;
                                        reply
                                    } else {
                                        let reply = format!("{}: Removed your saved unit preferences. Set it again with \\units [metric|imperial]", nick);
                                        plugin.set_user_units(&nick, None).await;
                                        reply
                                    };
                                    irc.send_messages.send(irc::Message::privmsg(target, reply)).await.unwrap();

                                    // TODO improve this ugly ass part
                                    plugin.save_db(&irc.server).await.unwrap();
                                },
                                _ => {},
                            }
                        });
                    }
                }
            }
        });
        Ok(handle)
    }
}
