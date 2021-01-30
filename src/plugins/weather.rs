use anyhow::{anyhow, Result};
use tokio::task::JoinHandle;
use log::*;
use std::fs::File;
use std::path::Path;
use std::collections::HashMap;
use ron::de::from_reader;
use ron::ser::to_writer;
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
type UserWeather = HashMap<String, UserConfig>;

pub struct WeatherPlugin {
    user_db: UserWeather,
    openweathermap_apikey: String,
    geonames_apiuser: String,
}

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

    fn load_from<P: AsRef<Path>>(path: P) -> Result<UserWeather> {
        // TODO use tokio + async instead of std
        let file = File::open(&path)?;
        let user_db: UserWeather = from_reader(file)?;
        Ok(user_db)
    }

    fn save_to<P: AsRef<Path> + std::fmt::Debug>(user_db: &UserWeather, path: P) -> Result<()> {
        // TODO use tokio + async instead of std
        let file = File::create(&path)?;
        to_writer(file, user_db)?;
        Ok(())
    }
}

impl PluginBuilder for WeatherPlugin {
    const NAME: &'static str = "weather";
    type Plugin = WeatherPlugin;

    fn new(server: &str, config: Option<&bot::PluginConfig>) -> Result<WeatherPlugin> {
        if config.is_none() {
            return Err(anyhow!("Weather plugin needs `openweathermap-apikey` and `geonames-apiuser` config keys"));
        }
        let config = config.unwrap();
        // TODO get rid of these clones
        let openweathermap_apikey = config.get("openweathermap-apikey").expect("[Weather] Missing `openweathermap-apikey`").clone();
        let geonames_apiuser = config.get("geonames-apiuser").expect("[Weather] Missing `geonames-apiuser`").clone();
        let user_db_path = WeatherPlugin::db_path(server);
        if let Ok(user_db) = WeatherPlugin::load_from(user_db_path) {
            info!("[{}] Weather DB loaded successfully", server);
            debug!("{:?}", user_db);
            Ok(WeatherPlugin {
                openweathermap_apikey,
                geonames_apiuser,
                user_db,
            })
        } else {
            warn!("[{}] Weather DB not found", server);
            Ok(WeatherPlugin {
                openweathermap_apikey,
                geonames_apiuser,
                user_db: HashMap::new(),
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
    temp: f64,
    feels_like: f64,
    temp_min: f64,
    temp_max: f64,
    pressure: f64,
    humidity: f64,
}

#[derive(Deserialize, Debug, Clone)]
struct Wind {
    speed: f64,
    deg: Option<f64>,
}

#[derive(Deserialize, Debug, Clone)]
struct Sys {
    country: Option<String>,
    // #[serde(with = "timestamp")]
    // sunrise: DateTime<Utc>,
    // #[serde(with = "timestamp")]
    // sunset: DateTime<Utc>,
}

#[derive(Deserialize, Debug, Clone)]
struct Rain {
    #[serde(alias = "3h")]
    three_hour: Option<f64>,
}

#[derive(Deserialize, Debug, Clone)]
struct Snow {
    #[serde(alias = "3h")]
    three_hour: Option<f64>,
}

#[derive(Deserialize, Debug, Clone)]
struct WeatherData {
    coord: Coord,
    weather: Vec<WeatherCond>,
    base: String,
    main: WeatherMain,
    visibility: Option<f64>,
    wind: Wind,
    rain: Option<Rain>,
    snow: Option<Snow>,
    // #[serde(with = "timestamp")]
    // dt: DateTime<Utc>,
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

    fn print_data(&self, units: Option<&Units>, nick: Option<String>) -> String {
        let country = self.sys.country.clone().unwrap_or_else(|| "??".into());
        let units = if let Some(units) = units { units } else if country == "US" { &IMPERIAL } else { &METRIC };
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

#[derive(Deserialize, Debug, Clone)]
struct TimeData {
    // TODO
    // sunrise: String,
    // sunset: String,
    time: String,
}

impl WeatherPlugin {
    async fn get_openweathermap(&self, query: OWMQuery<'_>) -> Result<WeatherData> {
        let url = format!("https://api.openweathermap.org/data/2.5/weather?APPID={}&{}", self.openweathermap_apikey, query);
        let json: WeatherData = reqwest::get(&url).await?.json().await?;
        Ok(json)
    }

    async fn get_geonames(&self, coord: Coord) -> Result<TimeData> {
        let url = format!("http://api.geonames.org/timezoneJSON?username={}{}", self.geonames_apiuser, coord);
        let json: TimeData = reqwest::get(&url).await?.json().await?;
        Ok(json)
    }
}

// TODO maybe use another endpoint/more data for daily/weekly forecasts
// TODO configurable and global command prefix
// TODO convenience functions for sending a message back
impl Plugin for WeatherPlugin {
    fn spawn_task(mut self, mut irc: irc::IRC) -> Result<JoinHandle<Result<()>>> {
        let handle = tokio::spawn(async move {
            loop {
                while let Ok(msg) = irc.received_messages.recv().await {
                    if let irc::Command::Privmsg = msg.command {
                        if msg.target.is_none() || msg.parameters.len() != 1 {
                            error!("Unexpected PRIVMSG format, ignoring");
                            continue;
                        }

                        // TODO ideally this only happens if theres a command
                        let user = if let Some(user) = msg.source_as_user() { user } else {
                            error!("PRIVMSG without user, ignoring");
                            continue;
                        };
                        let target = msg.target.unwrap();

                        let (cmd, msg) = split_first_word(&msg.parameters[0]);
                        match cmd {
                            r"\w" | r"\t" => {
                                let nick = user.nick.to_lowercase();

                                let user_units = if let Some(UserConfig { location: _, units: Some(units) }) = self.user_db.get(&nick) {
                                    Some(units)
                                } else {
                                    None
                                };

                                let (query_string, target_nick) = if let Some(msg) = msg {
                                    if let Some(target_nick) = msg.strip_prefix("@") {
                                        let target_nick = target_nick.to_lowercase();
                                        if let Some(UserConfig { location: Some(user_loc), units: _ }) = self.user_db.get(&target_nick) {
                                            (user_loc.clone(), Some(target_nick))
                                        } else {
                                            let reply = format!("{}: Could not find saved weather location for `{}`", nick, target_nick);
                                            irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                            continue;
                                        }
                                    } else {
                                        (msg.to_owned(), None)
                                    }
                                } else {
                                    // no message, look up in user_db
                                    if let Some(UserConfig { location: Some(user_loc), units: _ }) = self.user_db.get(&nick) {
                                        (user_loc.clone(), Some(nick.clone()))
                                    } else {
                                        let reply = format!("{}: Could not find your saved weather location; try using \\wset first", nick);
                                        irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                        continue;
                                    }
                                };

                                let query = if let Some(id) = query_string.strip_prefix("id:") {
                                    OWMQuery::Id(id)
                                } else if query_string.chars().all(|c| c.is_ascii_digit()) {
                                    OWMQuery::USZip(&query_string)
                                } else {
                                    OWMQuery::Simple(&query_string)
                                };

                                let weather_data = if let Ok(data) = self.get_openweathermap(query).await {
                                    data
                                } else {
                                    let reply = format!("{}: Could not find `{}`", nick, query_string);
                                    irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                    continue;
                                };

                                if cmd == r"\w" {
                                    let reply = weather_data.print_data(user_units, target_nick);
                                    irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                } else if cmd == r"\t" {
                                    let geonames_data = if let Ok(data) = self.get_geonames(weather_data.coord).await {
                                        data
                                    } else {
                                        let reply = format!("{}: Unexpected geonames error", nick);
                                        irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                        continue;
                                    };

                                    let geoplace = if let Some(target_nick) = target_nick {
                                        format!("for {}", target_nick)
                                    } else {
                                        format!("in {}, {}", weather_data.name, weather_data.sys.country.unwrap())
                                    };
                                    let reply = format!("{}: The curent time {} is {}", nick, geoplace, geonames_data.time);
                                    irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                }
                            },
                            r"\wset" => {
                                let nick = user.nick.to_lowercase();
                                let reply = if let Some(msg) = msg {
                                    let reply = format!("{}: Updated your weather entry to `{}`", nick, msg);
                                    if let Some(user_conf) = self.user_db.get_mut(&nick) {
                                        user_conf.location = Some(msg.into());
                                    } else {
                                        self.user_db.insert(nick, UserConfig {
                                            location: Some(msg.into()),
                                            units: None,
                                        });
                                    }
                                    reply
                                } else {
                                    let reply = format!("{}: Removed you from the weather database", nick);
                                    if let Some(UserConfig { location: _, units: Some(units) }) = self.user_db.remove(&nick) {
                                        self.user_db.insert(nick, UserConfig {
                                            location: None,
                                            units: Some(units),
                                        });
                                    }
                                    reply
                                };
                                irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;

                                // TODO improve this ugly ass part
                                let db_path = WeatherPlugin::db_path(&irc.server);
                                WeatherPlugin::save_to(&self.user_db, db_path).unwrap();
                            },
                            r"\units" => {
                                let nick = user.nick.to_lowercase();
                                let reply = if let Some(msg) = msg {
                                    let units = match msg.to_lowercase().as_str() {
                                        "metric" => METRIC,
                                        "imperial" => IMPERIAL,
                                        _ => {
                                            let reply = format!("{}: Use \\units [metric|imperial] to set your preference", user.nick);
                                            irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;
                                            continue;
                                        },
                                    };
                                    let reply = format!("{}: Updated your units preference to `{:?}`", nick, units);
                                    if let Some(user_conf) = self.user_db.get_mut(&nick) {
                                        user_conf.units = Some(units);
                                    } else {
                                        self.user_db.insert(nick, UserConfig {
                                            location: None,
                                            units: Some(units),
                                        });
                                    }
                                    reply
                                } else {
                                    let reply = format!("{}: Removed your saved unit preferences. Set it with \\units [metric|imperial]", nick);
                                    if let Some(UserConfig { location: Some(location), units: _ }) = self.user_db.remove(&nick) {
                                        self.user_db.insert(nick, UserConfig {
                                            location: Some(location),
                                            units: None,
                                        });
                                    }
                                    reply
                                };
                                irc.send_messages.send(irc::Message::privmsg(target, reply)).await?;

                                // TODO improve this ugly ass part
                                let db_path = WeatherPlugin::db_path(&irc.server);
                                WeatherPlugin::save_to(&self.user_db, db_path).unwrap();
                            },
                            _ => {},
                        }
                    }
                }
            }
        });
        Ok(handle)
    }
}
