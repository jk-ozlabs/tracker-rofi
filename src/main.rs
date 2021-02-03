/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Small utility to query the Gnome tracker database from rofi
 *
 * Copyright (c) 2021 Jeremy Kerr <jk@ozlabs.org>
 */

use std::env;
use std::time::Duration;
use std::io::{self, Write};
use anyhow::{anyhow, Context};
use dbus::blocking::Connection;
use dbus::Message;
use fork::{daemon, Fork};
use opener;
use percent_encoding::percent_decode_str;
use url::Url;

const DBUS_TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Debug)]
struct QueryResult {
    uuid: String,
    uri: Url,
    title: String,
    snippet: String,
}

impl QueryResult {
    fn new(uuid: &str, uristr: &str, title: &str, snippet: &str) -> Option<Self> {
        Some(QueryResult {
            uuid: uuid.to_string(),
            uri: Url::parse(uristr).ok()?,
            title: title.to_string(),
            snippet: snippet.to_string(),
        })
    }

    fn description(&self) -> String {
        let decode = |s| percent_decode_str(s).decode_utf8_lossy();

        let (fname, pname) = match self.uri.path_segments() {
            Some(mut c) => {
                let f = c.next_back().map(decode);
                let p = c.map(decode).collect::<Vec<_>>().join("/");
                (f, Some(p))
            }
            None => (None, None),
        };

        let mut s: String = String::new();

        if let Some(f) = fname {
            s += format!("{}: ", f).as_str();
        }

        if self.title.len() > 0 {
            s += &self.title;
        }

        if let Some(p) = pname {
            s += format!(" [{}]", p.as_str()).as_str();
        }

        s
    }
}


fn sparql_escape(s: &str) -> String {
    s
        .replace('\\', r#"\\"#)
        .replace('"',  r#"\""#)
        .replace('\'', r#"\'"#)
}

fn tracker_search(q: &str) -> anyhow::Result<Vec<QueryResult>> {
    let conn = Connection::new_session()?;

    let query =
            format!(r#"SELECT ?s ?uri ?title fts:snippet(?s, "", "")
                WHERE {{
                    ?s fts:match "{}" .
                    ?s tracker:available true .
                    ?s nie:url ?uri .
                    OPTIONAL {{ ?s nie:title ?title . }}
                }}
                ORDER BY DESC(?r) ?uri OFFSET 0 LIMIT 15"#, sparql_escape(q));

    let msg = Message::new_method_call("org.freedesktop.Tracker1",
            "/org/freedesktop/Tracker1/Resources",
            "org.freedesktop.Tracker1.Resources",
            "SparqlQuery")
        .unwrap()
        .append1(query);

    let reply = conn.channel().send_with_reply_and_block(msg, DBUS_TIMEOUT)?;

    let res = reply.read1::<Vec<Vec<&str>>>()?
        .iter()
        .filter_map(|v| {
            QueryResult::new(v[0], v[1], v[2], v[3])
        })
        .collect();

    Ok(res)
}

fn tracker_query_uuid(uuid: &str) -> anyhow::Result<String> {
    let conn = Connection::new_session()?;

    let msg = Message::new_method_call("org.freedesktop.Tracker1",
                                       "/org/freedesktop/Tracker1/Resources",
                                       "org.freedesktop.Tracker1.Resources",
                                       "SparqlQuery")
        .unwrap()
        .append1(format!(r#"SELECT ?url
                 WHERE {{
                    "{}" nie:url ?url
                 }}
                 LIMIT 1"#, sparql_escape(uuid)));

    let reply = conn.channel().send_with_reply_and_block(msg, DBUS_TIMEOUT)?;

    let res = reply.read1::<Vec<Vec<&str>>>()
        .context("Can't parse query results")?;

    let uri = res
        .get(0)
        .ok_or(anyhow!("No results"))?
        .get(0)
        .ok_or(anyhow!("Invalid results"))?;

    Ok(uri.to_string())
}

fn escape_result(r: &str) -> String
{
    r.replace('\n', " ").replace('\0', "")
}

fn format_result(r: &QueryResult) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(escape_result(&r.description()).as_bytes());
    v.extend(b"\0info\x1f");
    v.extend(r.uuid.as_bytes());
    v.push(b'\n');
    v
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    /* no args: initial run */
    if args.len() == 1 {
        return Ok(());
    }

    /* if we have an info string, lookup a uuid and open */
    if let Ok(uuid) = env::var("ROFI_INFO") {
        let uri = tracker_query_uuid(&uuid)
            .with_context(|| format!("can't lookup UUID '{}'", uuid))?;
        return match daemon(false, false) {
            Err(_) => Err(anyhow!("can't fork")),
            Ok(Fork::Child) => opener::open(uri).context("can't open file"),
            Ok(Fork::Parent(_)) => Ok(()),
        }
    }

    /* otherwise, search and return results */
    let query = args[1..].join(" ");

    let stdout = io::stdout();
    let mut fd = stdout.lock();

    tracker_search(&query)
        .with_context(|| format!("failed search for \"{}\"", query))?
        .into_iter()
        .map(|r| format_result(&r))
        .map(|s| fd.write_all(&s))
        .fold(anyhow::Result::Ok(()),
            |s,r| { s.and(r.context("write")) })
}
