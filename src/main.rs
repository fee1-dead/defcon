use std::error::Error;

use chrono::{prelude::*, Duration};
use config;
use futures_util::TryStreamExt;
use lazy_static::lazy_static;

use mw::ua;
use regex::Regex;
use serde_json::Value;

static VANDALISM_KEYWORDS: [&str; 8] = [
    "revert",
    "rv ",
    "long-term abuse",
    "long term abuse",
    "lta",
    "abuse",
    "rvv ",
    "undid",
];
static NOT_VANDALISM_KEYWORDS: [&str; 12] = [
    "uaa",
    "good faith",
    "agf",
    "unsourced",
    "unreferenced",
    "self",
    "speculat",
    "original research",
    "rv tag",
    "typo",
    "incorrect",
    "format",
];
const INTERVAL_IN_MINS: i64 = 60;

lazy_static! {
    static ref SECTION_HEADER_RE: Regex = Regex::new(r"/\*[\s\S]+?\*/").unwrap();
    static ref LEVEL_RE: Regex = Regex::new(r"level\s*=\s*(\d+)").unwrap();
}

fn is_revert_of_vandalism(edit_summary: &str) -> bool {
    let edit_summary = SECTION_HEADER_RE
        .replace(edit_summary, "")
        .to_ascii_lowercase();

    if NOT_VANDALISM_KEYWORDS.iter().any(|kwd| edit_summary.contains(kwd)) {
        return false;
    }

    VANDALISM_KEYWORDS.iter().any(|kwd| edit_summary.contains(kwd))
}

async fn reverts_per_minute(client: &mw::Client) -> Result<f32, Box<dyn Error>> {
    let time_one_interval_ago = Utc::now() - Duration::minutes(INTERVAL_IN_MINS);
    let end_str = time_one_interval_ago.to_rfc3339_opts(SecondsFormat::Secs, true);
    let query = [
        ("action", "query"),
        ("list", "recentchanges"),
        ("rctype", "edit"),
        ("rcstart", &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)),
        ("rcend", &end_str),
        ("rcprop", "comment"),
        ("rclimit", "max"),
    ];
    #[derive(serde::Deserialize)]
    struct Edit {
        comment: String,
    }
    #[derive(serde::Deserialize)]
    struct RecentChanges {
        recentchanges: Vec<Edit>,
    }
    #[derive(serde::Deserialize)]
    struct Res {
        query: RecentChanges,
    }
    let num_reverts = client
        .get_all(query, |res: Res| {
            Ok(vec![res
                .query
                .recentchanges
                .iter()
                .filter(|edit| is_revert_of_vandalism(&edit.comment))
                .count()])
        })
        .try_fold(0, |x, y| async move { Ok(x + y) })
        .await?;
    Ok((num_reverts as f32) / (INTERVAL_IN_MINS as f32))
}

fn rpm_to_level(rpm: f32) -> u8 {
    if rpm <= 2.0 {
        5
    } else if rpm <= 4.0 {
        4
    } else if rpm <= 6.0 {
        3
    } else if rpm <= 8.0 {
        2
    } else {
        1
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = config::Config::builder()
        .add_source(config::File::with_name("settings"))
        .add_source(config::Environment::with_prefix("APP"))
        .build()?;
    let oauth_token = config.get_string("oauth_token")?;

    let (client, _) = mw::ClientBuilder::new("https://en.wikipedia.org/w/api.php").user_agent(
        ua!(concat!("DeadbeefBot/defcon-rs/", env!("CARGO_PKG_VERSION"), " (https://en.wikipedia.org/wiki/User:DeadbeefBot)"))
    ).login_oauth(&oauth_token).await?;

    // get current on-wiki defcon level
    let report_page = config.get_string("report_page")?;
    
    let q = [
        ("action", "query"),
        ("prop", "revisions"),
        ("titles", &report_page),
        ("rvprop", "content"),
        ("rvslots", "main"),
        ("rvlimit", "1"),
    ];
    let res = client.get(q).send().await?.error_for_status()?.json::<Value>().await?;
    let rev = &res["query"]["pages"][0]["revisions"][0];
    let revid = rev["revid"].as_u64().unwrap();
    let curr_text = rev["slots"]["main"]["content"].as_str().unwrap();
    
    let curr_level = if let Some(captures) = LEVEL_RE.captures(curr_text) {
        captures.get(1).unwrap().as_str().parse::<u8>().unwrap()
    } else {
        0
    };

    // compute current defcon level
    let rpm = reverts_per_minute(&client).await?;
    let level = rpm_to_level(rpm);

    if curr_level != level {
        let text = format!(
            "{{{{#switch: {{{{{{1}}}}}}
              | level = {}
              | sign = ~~~~~
              | info = {:.2} RPM according to [[User:DeadbeefBot|DeadbeefBot]]
            }}}}",
            level, rpm
        );
        // todo update
        let summary = format!("[[Wikipedia:Bots/Requests for approval/DeadbeefBot 4|Bot]] updating vandalism level to level {0} ({1:.2} RPM) #DEFCON{0}", level, rpm);
        let token = client.get_token("csrf").await?;
        let q = [
            ("action", "edit"),
            ("title", &report_page),
            ("summary", &summary),
            ("text", &text),
            ("baserevid", &format!("{revid}")),
            ("token", &token),
        ];

        client.post(q).send().await?.error_for_status()?;
    } else {
        // No edit necessary
    }
    Ok(())
}
