/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Small utility to query the Gnome tracker database from rofi
 *
 * Copyright (c) 2021 Jeremy Kerr <jk@ozlabs.org>
 */

use std::env;
use std::time::Duration;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use anyhow::{anyhow, Context};
use dbus::blocking::Connection;
use dbus::Message;
use dbus::arg::Variant;
use fork::{daemon, Fork};
use opener;
use percent_encoding::percent_decode_str;
use url::Url;
use fd::Pipe;

use nom::number::complete::u32;
use nom::bytes::complete::tag;
use nom::multi::{count};
use nom::sequence::tuple;

const DBUS_TIMEOUT: Duration = Duration::from_millis(2000);

#[derive(Debug)]
struct QueryResult {
    uuid: String,
    uri: Url,
    title: String,
    _snippet: String,
}

impl QueryResult {
    fn new(uuid: &str, uristr: &str, title: &str, snippet: &str) -> Option<Self> {
        Some(QueryResult {
            uuid: uuid.to_string(),
            uri: Url::parse(uristr).ok()?,
            title: title.to_string(),
            _snippet: snippet.to_string(),
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

//fn parse_one(buf: &[u8]) -> IResult<&[u8], (String, String, String, String)> {
fn parse_one(buf: &[u8]) -> nom::IResult<&[u8], QueryResult> {
    let p = u32(nom::number::Endianness::Native);

    let (b, _) = tag([4u8, 0, 0, 0])(buf)?;
    let (b, _types) = count(p, 4)(b)?;
    let (mut b, lengths) = count(p, 4)(b)?;

    let mut offset = 0;
    let mut res = Vec::new();

    for l in lengths {
        let len = l - offset;
        let (bp, x) = nom::bytes::complete::take(len)(b)?;
        let (bp, _) = nom::bytes::complete::tag(&[0u8])(bp)?;
        b = bp;
        res.push(std::str::from_utf8(x).unwrap());
        offset += len + 1;
    }

    let qr = QueryResult::new(res[0], res[1], res[2], res[3]).unwrap();

    Ok((b, qr))
}

fn tracker_search_v3(q: &str) -> anyhow::Result<Vec<QueryResult>> {
    let conn = Connection::new_session()?;
    let mut pipe = Pipe::new()?;
    let args : HashMap<&str,Variant<u32>> = HashMap::new();

    let query =
            format!(r#"SELECT DISTINCT ?s ?uri ?title fts:snippet(?s, "", "")
                WHERE {{
                    ?s fts:match "{}" .
                    ?s nie:isStoredAs/nie:dataSource/tracker:available
                        | nie:dataSource/tracker:available true
                    .
                    ?s nie:url ?uri .
                    OPTIONAL {{ ?s nie:title ?title . }}
                }}
                OFFSET 0 LIMIT 15"#, sparql_escape(q));

    let msg = Message::new_method_call("org.freedesktop.Tracker3.Miner.Files",
            "/org/freedesktop/Tracker3/Endpoint",
            "org.freedesktop.Tracker3.Endpoint",
            "Query")
        .unwrap()
        .append1(query)
        .append1(pipe.writer)
        .append1(args);

    let reply = conn.channel().send_with_reply_and_block(msg, DBUS_TIMEOUT)?;

    /* ensure we have four columns */
    let res = reply.read1::<Vec<&str>>()?;

    if res.len() != 4 {
        return Err(anyhow!("Invalid search results"));
    }

    let mut buf = Vec::new();
    pipe.reader.read_to_end(&mut buf)?;

    let (_, res) = nom::multi::many0(parse_one)(buf.as_slice()).unwrap();

    Ok(res)
}

fn tracker_query_uuid_v3(uuid: &str) -> anyhow::Result<String> {
    let conn = Connection::new_session()?;
    let mut pipe = Pipe::new()?;
    let args : HashMap<&str,Variant<u32>> = HashMap::new();

    let msg = Message::new_method_call("org.freedesktop.Tracker3.Miner.Files",
                                       "/org/freedesktop/Tracker3/Endpoint",
                                       "org.freedesktop.Tracker3.Endpoint",
                                       "Query")
        .unwrap()
        .append1(format!(r#"SELECT ?url
                 WHERE {{
                    "{}" nie:url ?url
                 }}
                 LIMIT 1"#, sparql_escape(uuid)))
        .append1(pipe.writer)
        .append1(args);

    let reply = conn.channel().send_with_reply_and_block(msg, DBUS_TIMEOUT)?;

    let res = reply.read1::<Vec<&str>>()?;
    if res.len() != 1 {
        return Err(anyhow!("Invalid UUID search result"));
    }

    let mut buf = Vec::new();
    pipe.reader.read_to_end(&mut buf)?;
    let b = buf.as_slice();

    let p = u32(nom::number::Endianness::Native);

    let res : nom::IResult<&[u8],(_, u32,u32)> = tuple((tag([1u8, 0, 0, 0]), p, p))(b);
    let (b, (_, _type, len)) = res.unwrap();
    let res : nom::IResult<&[u8],&[u8]> = nom::bytes::complete::take(len)(b);
    let (_, x) = res.unwrap();

    let uri = std::str::from_utf8(x).unwrap();
    Ok(uri.to_string())
}

fn format_rofi_option<'a, I>(val: Option<&'a str>, meta: I) -> Vec<u8>
where
    I: IntoIterator<Item = (&'a str, &'a str)>
{
    let mut v = Vec::new();
    if let Some(valstr) = val {
        v.extend(valstr.as_bytes())
    }
    v.push(0);
    v.extend(meta.into_iter().map(|(name,val)| {
                let mut optdata = Vec::new();
                optdata.extend(name.as_bytes());
                optdata.push(0x1f);
                optdata.extend(val.as_bytes());
                optdata })
            .collect::<Vec<_>>()
            .join(&0x1fu8));
    v.push(0x0a);
    v
}

fn escape_result(r: &str) -> String
{
    r.replace('\n', " ").replace('\0', "")
}

fn format_result(r: &QueryResult) -> Vec<u8> {
    let opts: Vec<(&str,&str)> = vec![("info", &r.uuid)];
    format_rofi_option(Some(&escape_result(&r.description())), opts)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    /* no args: initial run */
    if args.len() == 1 {
        return Ok(());
    }

    /* if we have an info string, lookup a uuid and open */
    if let Ok(uuid) = env::var("ROFI_INFO") {
        let uri = tracker_query_uuid_v3(&uuid)
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

    let results = tracker_search_v3(&query)
        .with_context(|| format!("failed search for \"{}\"", query))?;

    if results.len() == 0 {
        let opt = format_rofi_option(Some("no results"),
                    vec![("nonselectable", "true")]);
        fd.write_all(&opt).context("write")
    } else {
        results
            .into_iter()
            .map(|r| format_result(&r))
            .map(|s| fd.write_all(&s))
            .fold(anyhow::Result::Ok(()),
                |s,r| { s.and(r.context("write")) })
    }
}
