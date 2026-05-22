use chrono::{DateTime, NaiveDate, Utc};

use chrono_tz::America::New_York;

use serde::{Deserialize, Serialize};

use std::fs;

struct EventbriteEvent {
    id: String,
    name: String,
    opens: DateTime<Utc>,
    url: String,
    image: String,
    description: String,
    status: String,
    sales_status: String,
    listed: bool,
}

#[derive(serde::Serialize)]
struct EmbedImage {
    url: String,
}

#[derive(serde::Serialize)]
struct EmbedAuthor {
    name: String,
    url: Option<String>,
    icon_url: Option<String>,
    proxy_icon_url: Option<String>,
}

#[derive(serde::Serialize)]
struct Embed {
    title: Option<String>,
    url: Option<String>,
    image: Option<EmbedImage>,
    description: Option<String>,
    color: Option<u32>,
    author: Option<EmbedAuthor>,
}

#[derive(serde::Serialize)]
struct EmbedAllowedMentions {
    parse: Vec<String>,
}

#[derive(serde::Serialize)]
struct Message {
    content: String,
    embeds: Vec<Embed>,
    mention_roles: Option<Vec<u64>>,
    allowed_mentions: Option<EmbedAllowedMentions>,
}

fn within_one_week(date: NaiveDate) -> bool {
    let today = Utc::now().date_naive();
    (date - today).num_days().abs() <= 7
}

impl EventbriteEvent {
    fn new(event: &serde_json::Value) -> Self {
        Self {
            id: event["id"].as_str().unwrap().to_string(),
            name: event["name"]["text"].as_str().unwrap().parse().unwrap(),
            opens: event["start"]["utc"].as_str().unwrap().parse().unwrap(),
            url: event["url"].as_str().unwrap().to_owned(),
            image: event["logo"]["url"].as_str().unwrap().to_owned(),
            description: event["description"]["text"].as_str().unwrap().to_owned(),
            status: event["status"].as_str().unwrap().to_owned(),
            listed: event["listed"].as_bool().unwrap(),
            sales_status: event["event_sales_status"]["sales_status"]
                .as_str()
                .unwrap()
                .to_owned(),
        }
    }

    fn active(&self) -> bool {
        self.status == "live"
            && self.listed
            && within_one_week(self.opens.date_naive())
            && self.sales_status == "on_sale"
    }
}

use clap::Parser;

/// Checks if any Riichi Mahjong eventbrite events have been added, and triggers a webhook if they
/// have.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Eventbrite API Key.
    #[arg(short, long, env = "EVENTBRITE_KEY")]
    eventbrite_key: String,

    /// Thursday Discord Webhook URI.
    #[arg(long, env = "THURSDAY_URI")]
    thursday_uri: Option<String>,

    /// Thursday Discord Role ID.
    #[arg(long, env = "THURSDAY_ROLE")]
    thursday_role: Option<String>,

    /// Sunday Discord Webhook URI.
    #[arg(long, env = "SUNDAY_URI")]
    sunday_uri: Option<String>,

    /// Sunday Discord Role ID.
    #[arg(long, env = "SUNDAY_ROLE")]
    sunday_role: Option<String>,

    #[arg(long, env = "EVENTBRITE_ORGANIZER_ID", default_value_t = String::from("121101767636"))]
    organizer_id: String,

    /// Color of the embed accent.
    #[arg(long, env = "EVENTBRITE_COLOR", default_value_t = String::from("df4601"))]
    color: String,

    /// Directory where data is stored.
    #[arg(short, long, env="EVENTBRITE_DATA_DIR", default_value_t = String::from("./"))]
    data_dir: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Data {
    notified: Vec<String>,
}

impl Data {
    fn notified_mut(&mut self) -> &mut Vec<String> {
        &mut self.notified
    }
}

struct DataLock {
    data: Data,
    lockfile: String,
    datafile: String,
}

impl DataLock {
    fn new(args: &Args) -> Result<Self, Box<dyn std::error::Error>> {
        let lockfile = format!("{}/data.lock", args.data_dir);
        let datafile = format!("{}/data.ron", args.data_dir);
        let data = if fs::exists(&datafile)? {
            ron::from_str(
                &fs::read_to_string(&datafile).expect("Should have been able to read the file"),
            )
            .unwrap()
        } else {
            let d = Data { notified: vec![] };
            fs::write(&datafile, ron::to_string(&d)?)?;
            d
        };
        fs::write(&lockfile, "")?;
        Ok(Self {
            data,
            lockfile,
            datafile,
        })
    }

    fn data_mut(&mut self) -> &mut Data {
        &mut self.data
    }

    fn write_data(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        fs::write(&self.datafile, ron::to_string(&self.data)?)?;
        Ok(())
    }
}

impl Drop for DataLock {
    fn drop(&mut self) {
        fs::remove_file(&self.lockfile)
            .expect("Failed to remove lock file! Something went very wrong!");
    }
}

fn get_uri_role(event: &EventbriteEvent, args: &Args) -> Option<(String, u64)> {
    let found_dow = format!("{}", event.opens.with_timezone(&New_York).format("%A"));

    let (uri_opt, role_opt) = if found_dow == "Thursday" {
        (
            args.thursday_uri.clone(),
            args.thursday_role.clone()?.parse().ok(),
        )
    } else if found_dow == "Sunday" {
        (
            args.sunday_uri.clone(),
            args.sunday_role.clone()?.parse().ok(),
        )
    } else {
        return None;
    };

    Some((uri_opt?, role_opt?))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Create a lockfile that will automatically be removed when program finishes.
    let mut datalock = DataLock::new(&args)?;

    // Get all (visible) eventbrite events.
    let body = ureq::get(format!(
        "https://www.eventbriteapi.com/v3/organizers/{}/events/?expand=event_sales_status",
        args.organizer_id
    ))
    .header("Authorization", format!("Bearer {}", args.eventbrite_key))
    .call()?
    .body_mut()
    .read_json::<serde_json::Value>()?;

    // Scoped to correctly handle borrowing
    {
        // Get the current data, currently only used to avoid sending the same message twice.
        let data = datalock.data_mut();

        // Loop through each event.
        for event_value in body["events"].as_array().ok_or("Err")? {
            let event = EventbriteEvent::new(event_value);

            // If we haven't seen this event before
            if !data.notified.contains(&event.id) {
                // And it contains meetup
                if event.name.contains("Meetup") {
                    // Check the day of the week
                    let found_dow =
                        format!("{}", event.opens.with_timezone(&New_York).format("%A"));

                    // Sanity check that the day of week the event starts is in the name, and make
                    // sure it looks like this data is sometime within a week.
                    if event.active() && event.name.contains(&found_dow) {
                        let (uri, role) = match get_uri_role(&event, &args) {
                            Some(p) => p,
                            None => continue,
                        };

                        // Send the message
                        let out = ureq::post(uri).send_json(Message {
                            mention_roles: Some(vec![role]),
                            content: format!("<@&{role}> {}", &event.url),
                            embeds: vec![Embed {
                                description: Some(event.description),
                                url: Some(event.url),
                                image: Some(EmbedImage { url: event.image }),
                                title: Some(event.name),
                                color: u32::from_str_radix(&args.color, 16).ok(),
                                author: Some(EmbedAuthor {
                                    name: "Charm City Riichi".to_owned(),
                                    url: None,
                                    icon_url: None,
                                    proxy_icon_url: None,
                                }),
                            }],
                            allowed_mentions: Some(EmbedAllowedMentions {
                                parse: vec!["roles".to_string()],
                            }),
                        })?;

                        println!("Sent notification for event {}.", event.id);

                        #[cfg(debug_assertions)]
                        println!("Got: {out:?}");

                        data.notified_mut().push(event.id);
                    }
                }
            }
        }
    }

    // Write any changes to data to the file.
    datalock.write_data()?;
    Ok(())
}
