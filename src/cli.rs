//! Agent-first CLI: same handlers as the MCP tools, terse text output.

use anyhow::{bail, Result};
use serde_json::{Map, Value};

use crate::client::truncate;
use crate::tools::{self, App};

/// (command, tool, positional argument, summary)
const COMMANDS: &[(&str, &str, Option<&str>, &str)] = &[
    ("status", "careeraxis_status", None, "session check"),
    ("jobs", "search_jobs", Some("text"), "search jobs  [--type internship --industry ... --work-model Remote --order ClosingSoon --page N --size N --expired]"),
    ("job", "get_job", Some("id"), "full job listing"),
    ("filters", "job_filters", None, "valid values for jobs filters"),
    ("recommended", "recommended_jobs", None, "jobs recommended from your preferences  [--take N]"),
    ("events", "search_events", Some("text"), "search events  [--from YYYY-MM-DD --until YYYY-MM-DD --event-type-id N --page N]"),
    ("event", "get_event", Some("id"), "full event details + booking link"),
    ("my-events", "my_events", Some("which"), "your booked events: upcoming | past | attended"),
    ("orgs", "search_organisations", Some("text"), "find employers by name"),
    ("org", "get_organisation", Some("id"), "employer profile + current openings"),
    ("bookmarks", "bookmarks", None, "your saved items"),
    ("appointments", "appointment_topics", Some("search"), "career-advice appointment topics"),
    ("slots", "appointment_availability", Some("topic_id"), "open days for a topic  [--date YYYY-MM-DD | --from --until]"),
    ("notifications", "notifications", None, "your notifications"),
];

const ALIASES: &[(&str, &str)] = &[
    ("type", "type_of_work"),
    ("size", "page_size"),
    ("expired", "include_expired"),
    ("hours", "contract_hours"),
    ("remote", "work_model"),
];

pub fn help() -> String {
    let mut s = String::from(
        "careeraxis-mcp — NTU CareerAxis from your agent. No args: MCP server on stdio.\n\n\
         setup                       log in once in a browser window (saves the session)\n",
    );
    for (cmd, _, pos, about) in COMMANDS {
        let left = match pos {
            Some(p) => format!("{cmd} <{p}>"),
            None => cmd.to_string(),
        };
        s.push_str(&format!("{left:<28}{about}\n"));
    }
    s.push_str("\nFlags: --json (raw JSON), --full (don't truncate table cells).\n");
    s
}

pub async fn run(app: &App, args: &[String]) -> Result<()> {
    let cmd = args[0].as_str();
    let Some(&(_, tool, positional, _)) = COMMANDS.iter().find(|c| c.0 == cmd) else {
        bail!("unknown command '{cmd}'. Try `careeraxis-mcp help`.");
    };
    let def = tools::find(tool).unwrap();
    let schema = (def.schema)();
    let props = &schema["properties"];
    let mut out = Map::new();
    let (mut json, mut full) = (false, false);
    let mut words = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        if let Some(flag) = a.strip_prefix("--") {
            let (flag, inline) = match flag.split_once('=') {
                Some((f, v)) => (f, Some(v.to_string())),
                None => (flag, None),
            };
            match flag {
                "json" => json = true,
                "full" => full = true,
                _ => {
                    let mut key = flag.replace('-', "_");
                    if let Some((_, k)) = ALIASES.iter().find(|(a, _)| *a == flag) {
                        key = k.to_string();
                    }
                    let ty = props[&key]["type"].as_str();
                    if ty.is_none() {
                        bail!("unknown flag --{flag} for `{cmd}`");
                    }
                    let value = if flag == "remote" {
                        Value::from("Remote")
                    } else if ty == Some("boolean") {
                        Value::Bool(inline.as_deref().map_or(true, |v| v != "false"))
                    } else {
                        let v = match inline {
                            Some(v) => v,
                            None => {
                                i += 1;
                                args.get(i).cloned().ok_or_else(|| anyhow::anyhow!("--{flag} needs a value"))?
                            }
                        };
                        coerce(ty, v)
                    };
                    out.insert(key, value);
                }
            }
        } else {
            words.push(a.clone());
        }
        i += 1;
    }
    if let Some(p) = positional {
        if !words.is_empty() {
            out.insert(p.to_string(), coerce(props[p]["type"].as_str(), words.join(" ")));
        }
    }
    let v = tools::call(app, tool, &Value::Object(out)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        print!("{}", render(&v, full));
    }
    Ok(())
}

fn coerce(ty: Option<&str>, v: String) -> Value {
    match ty {
        Some("integer") => v.parse::<i64>().map(Value::from).unwrap_or(Value::String(v)),
        _ => Value::String(v),
    }
}

fn cell(v: &Value, full: bool) -> String {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        x => x.to_string(),
    };
    let s = s.replace(['\t', '\n'], " ");
    if full { s } else { truncate(&s, 60) }
}

fn table(items: &[Value], full: bool) -> String {
    let mut cols: Vec<String> = Vec::new();
    for it in items {
        if let Some(m) = it.as_object() {
            for k in m.keys() {
                if !cols.contains(k) {
                    cols.push(k.clone());
                }
            }
        }
    }
    let mut s = cols.join("\t") + "\n";
    for it in items {
        let row: Vec<String> = cols.iter().map(|c| cell(&it[c], full)).collect();
        s.push_str(&row.join("\t"));
        s.push('\n');
    }
    s
}

pub fn render(v: &Value, full: bool) -> String {
    let Some(m) = v.as_object() else {
        return format!("{v}\n");
    };
    let mut s = String::new();
    if let Some(items) = m.get("items").and_then(|i| i.as_array()) {
        if items.is_empty() {
            s.push_str("(no results)\n");
        } else {
            s.push_str(&table(items, full));
        }
        if let Some(total) = m.get("total") {
            s.push_str(&format!(
                "-- page {}/{} · {} total\n",
                m.get("page").map(|p| p.to_string()).unwrap_or("1".into()),
                m.get("total_pages").map(|p| p.to_string()).unwrap_or("?".into()),
                total
            ));
        }
        return s;
    }
    for (k, val) in m {
        match val {
            Value::String(t) if t.contains('\n') || t.len() > 100 => s.push_str(&format!("\n[{k}]\n{t}\n\n")),
            Value::Array(a) if a.iter().all(|x| x.is_object()) && !a.is_empty() => {
                s.push_str(&format!("\n[{k}]\n{}", table(a, full)));
            }
            Value::Array(a) if a.iter().all(|x| x.is_string()) => {
                let parts: Vec<&str> = a.iter().filter_map(|x| x.as_str()).collect();
                s.push_str(&format!("{k}: {}\n", parts.join("; ")));
            }
            Value::String(t) => s.push_str(&format!("{k}: {t}\n")),
            x => s.push_str(&format!("{k}: {x}\n")),
        }
    }
    s
}
