//! Tool catalogue + handlers, shared by the MCP server and the CLI.
//! Every handler returns a trimmed JSON value (only the fields an agent needs).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use scraper::{Html, Selector};
use serde_json::{json, Map, Value};
use tokio::sync::{Mutex, OnceCell};

use crate::client::{Client, BASE};
use crate::html::{fragment_to_text, page_text, select_text};

pub struct App {
    pub client: Arc<Client>,
    job_config: OnceCell<Value>,
    login_task: Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
}

impl App {
    pub fn new(client: Client) -> Self {
        App { client: Arc::new(client), job_config: OnceCell::new(), login_task: Mutex::new(None) }
    }
}

// ---------------------------------------------------------------- catalogue

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "careeraxis_status",
        description: "Check the CareerAxis session: whether you are logged in, when the session was last renewed, and how many jobs are open.",
        schema: || obj(json!({}), &[]),
    },
    ToolDef {
        name: "careeraxis_login",
        description: "Open a browser window for the user to sign in to NTU CareerAxis. Use only when another tool reports NEEDS_LOGIN. Returns waiting_for_user if the user is still signing in; call careeraxis_status afterwards.",
        schema: || obj(json!({}), &[]),
    },
    ToolDef {
        name: "search_jobs",
        description: "Search CareerAxis job and internship listings. Filters take names or ids from job_filters (substring match, e.g. type_of_work='intern'). Returns a page of summaries; use get_job for the full description.",
        schema: || obj(json!({
            "text": {"type": "string", "description": "Keywords, e.g. 'machine learning'"},
            "type_of_work": {"type": "string", "description": "Comma-separated: Internship, Graduate Job, Contract Job, Immediate Start Job, Postgraduate Job"},
            "industry": {"type": "string", "description": "Comma-separated industry names or ids"},
            "occupation": {"type": "string", "description": "Comma-separated occupation names or ids"},
            "work_model": {"type": "string", "description": "Comma-separated: OnSite, Remote, Hybrid"},
            "contract_hours": {"type": "string", "description": "Comma-separated: FullTime, PartTime, Casual"},
            "order": {"type": "string", "enum": ["Relevance", "Published", "ClosingSoon"], "description": "Default: Relevance when text is given, else Published"},
            "include_expired": {"type": "boolean"},
            "page": {"type": "integer", "minimum": 1, "description": "1-based page"},
            "page_size": {"type": "integer", "minimum": 1, "maximum": 100, "description": "Default 20"}
        }), &[]),
    },
    ToolDef {
        name: "get_job",
        description: "Full details of one job: description, pay, dates, how to apply, contact, employer.",
        schema: || obj(json!({"id": {"type": "integer"}}), &["id"]),
    },
    ToolDef {
        name: "job_filters",
        description: "List the valid filter values for search_jobs (types of work, industries, occupations, work models, contract hours).",
        schema: || obj(json!({}), &[]),
    },
    ToolDef {
        name: "recommended_jobs",
        description: "Jobs CareerAxis recommends for the user from their saved preferences.",
        schema: || obj(json!({"take": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Default 10"}}), &[]),
    },
    ToolDef {
        name: "search_events",
        description: "Search CareerAxis events (career fairs, talks, on-campus interviews, workshops).",
        schema: || obj(json!({
            "text": {"type": "string"},
            "event_type_id": {"type": "integer", "description": "From the 'type' field of results"},
            "from": {"type": "string", "description": "Start date lower bound, YYYY-MM-DD"},
            "until": {"type": "string", "description": "Start date upper bound, YYYY-MM-DD"},
            "page": {"type": "integer", "minimum": 1}
        }), &[]),
    },
    ToolDef {
        name: "get_event",
        description: "Full details of one event: date, venue, sessions, description and booking link.",
        schema: || obj(json!({"id": {"type": "integer"}}), &["id"]),
    },
    ToolDef {
        name: "my_events",
        description: "Events the user has booked (upcoming), booked in the past, or attended.",
        schema: || obj(json!({
            "which": {"type": "string", "enum": ["upcoming", "past", "attended"], "description": "Default upcoming"},
            "page": {"type": "integer", "minimum": 1}
        }), &[]),
    },
    ToolDef {
        name: "search_organisations",
        description: "Find employers on CareerAxis by name. Returns ids for get_organisation.",
        schema: || obj(json!({"text": {"type": "string"}}), &["text"]),
    },
    ToolDef {
        name: "get_organisation",
        description: "An employer's profile (location, website, industry, about) and its current openings.",
        schema: || obj(json!({"id": {"type": "integer"}}), &["id"]),
    },
    ToolDef {
        name: "bookmarks",
        description: "The user's saved items (jobs, events, employers, resources).",
        schema: || obj(json!({"text": {"type": "string"}, "page": {"type": "integer", "minimum": 1}}), &[]),
    },
    ToolDef {
        name: "appointment_topics",
        description: "Career-advice appointment topics the user can book with the Career & Attachment Office.",
        schema: || obj(json!({"search": {"type": "string"}, "page": {"type": "integer", "minimum": 1}}), &[]),
    },
    ToolDef {
        name: "appointment_availability",
        description: "Availability for one appointment topic: the days with open slots (default next 30 days), or the slots on one date.",
        schema: || obj(json!({
            "topic_id": {"type": "integer"},
            "date": {"type": "string", "description": "YYYY-MM-DD: list the slots on this day"},
            "from": {"type": "string", "description": "YYYY-MM-DD"},
            "until": {"type": "string", "description": "YYYY-MM-DD"}
        }), &["topic_id"]),
    },
    ToolDef {
        name: "notifications",
        description: "The user's CareerAxis notifications.",
        schema: || obj(json!({"take": {"type": "integer", "minimum": 1, "maximum": 50}}), &[]),
    },
];

pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

// ---------------------------------------------------------------- helpers

fn enc(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

#[derive(Default)]
struct Q(Vec<String>);
impl Q {
    fn add(&mut self, k: &str, v: impl ToString) -> &mut Self {
        self.0.push(format!("{k}={}", enc(&v.to_string())));
        self
    }
    fn s(&self) -> String {
        self.0.join("&")
    }
}

fn arg_str(a: &Value, k: &str) -> Option<String> {
    match &a[k] {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(v) => Some(v.iter().filter_map(|x| x.as_str().map(String::from).or_else(|| x.as_i64().map(|n| n.to_string()))).collect::<Vec<_>>().join(",")),
        _ => None,
    }
}

fn arg_list(a: &Value, k: &str) -> Vec<String> {
    arg_str(a, k)
        .map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect())
        .unwrap_or_default()
}

fn arg_int(a: &Value, k: &str) -> Option<i64> {
    match &a[k] {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn req_int(a: &Value, k: &str) -> Result<i64> {
    arg_int(a, k).ok_or_else(|| anyhow!("missing integer argument '{k}'"))
}

fn arg_bool(a: &Value, k: &str) -> bool {
    match &a[k] {
        Value::Bool(b) => *b,
        Value::String(s) => matches!(s.as_str(), "true" | "1" | "yes"),
        _ => false,
    }
}

fn day(v: &Value) -> Value {
    v.as_str().map(|s| Value::from(s.chars().take(10).collect::<String>())).unwrap_or(Value::Null)
}

fn when(v: &Value) -> Value {
    v.as_str()
        .map(|s| Value::from(s.chars().take(16).collect::<String>().replace('T', " ")))
        .unwrap_or(Value::Null)
}

/// Drop nulls, empty strings and empty arrays so results stay small.
fn compact(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| (k, compact(v)))
                .filter(|(k, v)| k == "items" || match v {
                    Value::Null => false,
                    Value::String(s) => !s.is_empty(),
                    Value::Array(a) => !a.is_empty(),
                    _ => true,
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(compact).collect()),
        x => x,
    }
}

fn paged(items: Vec<Value>, p: &Value) -> Value {
    json!({
        "items": items,
        "page": p["pageIndex"].as_i64().map(|i| i + 1),
        "total_pages": p["totalPages"],
        "total": p["totalCount"],
    })
}

fn resolve(options: &Value, label_key: &str, id_key: &str, wanted: &[String], what: &str) -> Result<Vec<String>> {
    let opts = options.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for w in wanted {
        if w.chars().all(|c| c.is_ascii_digit()) {
            out.push(w.clone());
            continue;
        }
        let wl = w.to_lowercase();
        let label = |o: &Value| o[label_key].as_str().unwrap_or_default().to_lowercase();
        let hit = opts
            .iter()
            .find(|o| label(o) == wl || o[id_key].as_str().is_some_and(|v| v.to_lowercase() == wl))
            .or_else(|| opts.iter().find(|o| label(o).contains(&wl)));
        match hit {
            Some(o) => out.push(match &o[id_key] {
                Value::String(s) => s.clone(),
                v => v.to_string(),
            }),
            None => {
                let names: Vec<&str> = opts.iter().filter_map(|o| o[label_key].as_str()).collect();
                bail!("unknown {what} '{w}'. Valid: {}", names.join("; "))
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- dispatch

pub async fn call(app: &App, name: &str, a: &Value) -> Result<Value> {
    let c = &app.client;
    let v = match name {
        "careeraxis_status" => status(app).await?,
        "careeraxis_login" => login(app).await?,
        "search_jobs" => search_jobs(app, a).await?,
        "get_job" => get_job(c, req_int(a, "id")?).await?,
        "job_filters" => job_filters(app).await?,
        "recommended_jobs" => {
            let take = arg_int(a, "take").unwrap_or(10);
            let r = c.get_json(&format!("/services/students/jobs/recommendations?studentSiteId=1&take={take}")).await?;
            let list = r.as_array().or_else(|| r["items"].as_array()).cloned().unwrap_or_default();
            json!({"items": list.iter().map(job_row).collect::<Vec<_>>()})
        }
        "search_events" => search_events(c, a).await?,
        "get_event" => get_event(c, req_int(a, "id")?).await?,
        "my_events" => {
            let which = arg_str(a, "which").unwrap_or_else(|| "upcoming".into());
            if !["upcoming", "past", "attended"].contains(&which.as_str()) {
                bail!("which must be upcoming, past or attended");
            }
            let page = arg_int(a, "page").unwrap_or(1);
            let r = c.get_json(&format!("/services/students/events/{which}?page={page}&studentSiteId=1")).await?;
            let p = if r["data"].is_object() { &r["data"] } else { &r };
            let items = p["items"].as_array().cloned().unwrap_or_default();
            paged(items.iter().map(|i| event_row(if i["event"].is_object() { &i["event"] } else { i })).collect(), p)
        }
        "search_organisations" => {
            let text = arg_str(a, "text").ok_or_else(|| anyhow!("missing 'text'"))?;
            let r = c.get_json(&format!("/services/students/organisations/suggest?text={}", enc(&text))).await?;
            let items: Vec<Value> = r
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|o| json!({"id": o["id"], "name": o["value"]}))
                .collect();
            json!({"items": items})
        }
        "get_organisation" => get_organisation(c, req_int(a, "id")?).await?,
        "bookmarks" => {
            let mut q = Q::default();
            q.add("page", arg_int(a, "page").unwrap_or(1));
            if let Some(t) = arg_str(a, "text") {
                q.add("text", t);
            }
            let r = c.get_json(&format!("/services/students/bookmarks?{}", q.s())).await?;
            let items = r["items"].as_array().cloned().unwrap_or_default();
            paged(items.iter().map(bookmark_row).collect(), &r)
        }
        "appointment_topics" => {
            let mut q = Q::default();
            q.add("siteId", 1).add("pageNumber", arg_int(a, "page").unwrap_or(1));
            if let Some(t) = arg_str(a, "search") {
                q.add("search", t);
            }
            let r = c.get_json(&format!("/services/students/appointments/topics?{}", q.s())).await?;
            let items: Vec<Value> = r["items"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|t| {
                    json!({
                        "id": t["id"], "name": t["name"], "available": t["available"],
                        "description": t["description"].as_str().map(fragment_to_text),
                    })
                })
                .collect();
            paged(items, &r)
        }
        "appointment_availability" => {
            let id = req_int(a, "topic_id")?;
            if let Some(d) = arg_str(a, "date") {
                let r = c.get_json(&format!("/services/students/appointments/topics/{id}/appointments?siteId=1&date={}", enc(&d))).await?;
                let rows: Vec<Value> = r
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|s| json!({
                        "start": when(&s["start"]),
                        "end": when(&s["end"]),
                        "with": s["consultantName"],
                        "type": s["typeName"],
                        "where": s["locationName"],
                        "drop_in": s["dropIn"].as_bool().filter(|b| *b),
                        "available": s["available"],
                        "booked": s["booked"].as_bool().filter(|b| *b),
                        "book_url": format!("{BASE}/students/appointments/topics/{id}"),
                    }))
                    .collect();
                json!({"items": rows})
            } else {
                let mut q = Q::default();
                q.add("siteId", 1).add("pageSize", 30).add("orderByAscending", true);
                if let Some(f) = arg_str(a, "from") {
                    q.add("fromDate", f);
                }
                if let Some(u) = arg_str(a, "until") {
                    q.add("untilDate", u);
                }
                c.get_json(&format!("/services/students/appointments/topics/{id}/available-days?{}", q.s())).await?
            }
        }
        "notifications" => {
            let take = arg_int(a, "take").unwrap_or(10);
            c.get_json(&format!("/services/students/notifications?skip=0&take={take}")).await?
        }
        _ => bail!("unknown tool '{name}'"),
    };
    Ok(compact(v))
}

// ---------------------------------------------------------------- handlers

async fn status(app: &App) -> Result<Value> {
    let jar = app.client.jar().await;
    let probe = app
        .client
        .get_json("/services/students/jobs/search?studentSiteId=1&pageIndex=0&pageSize=1&saveAsRecent=false")
        .await;
    let jar_after = app.client.jar().await;
    let ago = |t: Option<u64>| t.map(|t| format!("{} min ago", crate::session::now().saturating_sub(t) / 60));
    Ok(match probe {
        Ok(r) => json!({
            "logged_in": true,
            "open_jobs": r["items"]["totalCount"],
            "last_login": ago(jar_after.login_at),
            "last_silent_renewal": ago(jar_after.renewed_at),
        }),
        Err(e) => json!({
            "logged_in": false,
            "saved_ntu_login": jar.has_idp_session(),
            "error": format!("{e:#}"),
            "fix": "run `careeraxis-mcp setup` or call careeraxis_login",
        }),
    })
}

async fn login(app: &App) -> Result<Value> {
    let mut slot = app.login_task.lock().await;
    if slot.as_ref().map_or(true, |h| h.is_finished()) {
        let client = app.client.clone();
        *slot = Some(tokio::spawn(async move {
            let jar = crate::browser::interactive_login(Duration::from_secs(15 * 60)).await?;
            jar.save()?;
            client.install(jar).await;
            Ok(())
        }));
    }
    let handle = slot.as_mut().unwrap();
    match tokio::time::timeout(Duration::from_secs(45), handle).await {
        Ok(joined) => {
            *slot = None;
            joined??;
            Ok(json!({"status": "logged_in"}))
        }
        Err(_) => Ok(json!({
            "status": "waiting_for_user",
            "message": "A CareerAxis login window is open. Ask the user to sign in there, then call careeraxis_status.",
        })),
    }
}

async fn job_config(app: &App) -> Result<&Value> {
    app.job_config
        .get_or_try_init(|| app.client.get_json("/services/students/jobs/search/configuration"))
        .await
}

async fn job_filters(app: &App) -> Result<Value> {
    let cfg = job_config(app).await?;
    let names = |k: &str, label: &str| -> Value {
        cfg[k].as_array().cloned().unwrap_or_default().iter().map(|o| o[label].clone()).collect()
    };
    Ok(json!({
        "type_of_work": names("typeOfWorkOptions", "name"),
        "industry": names("industryOptions", "name"),
        "occupation": names("occupationOptions", "name"),
        "work_model": cfg["workModelOptions"].as_array().cloned().unwrap_or_default().iter().map(|o| o["value"].clone()).collect::<Value>(),
        "contract_hours": cfg["contractHoursOptions"].as_array().cloned().unwrap_or_default().iter().map(|o| o["value"].clone()).collect::<Value>(),
        "order": ["Relevance", "Published", "ClosingSoon"],
    }))
}

fn job_row(j: &Value) -> Value {
    json!({
        "id": j["id"].as_i64().or(j["entityId"].as_i64()),
        "title": j["name"].as_str().or(j["title"].as_str()),
        "employer": j["employerName"],
        "location": j["location"],
        "hours": j["contractHours"],
        "work_model": j["workModelDisplay"],
        "published": day(&j["published"]),
    })
}

async fn search_jobs(app: &App, a: &Value) -> Result<Value> {
    let page = arg_int(a, "page").unwrap_or(1).max(1);
    let size = arg_int(a, "page_size").unwrap_or(20).clamp(1, 100);
    let text = arg_str(a, "text");
    let mut q = Q::default();
    q.add("studentSiteId", 1)
        .add("pageIndex", page - 1)
        .add("pageSize", size)
        .add("saveAsRecent", false);
    let order = arg_str(a, "order").unwrap_or_else(|| if text.is_some() { "Relevance".into() } else { "Published".into() });
    q.add("order", order);
    if let Some(t) = &text {
        q.add("text", t);
    }
    if arg_bool(a, "include_expired") {
        q.add("includeExpired", true);
    }
    let needs_cfg = ["type_of_work", "industry", "occupation", "work_model", "contract_hours"]
        .iter()
        .any(|k| !arg_list(a, k).is_empty());
    if needs_cfg {
        let cfg = job_config(app).await?;
        for (arg, param, opts, label, id) in [
            ("type_of_work", "typeOfWork", "typeOfWorkOptions", "name", "id"),
            ("industry", "industry", "industryOptions", "name", "id"),
            ("occupation", "occupation", "occupationOptions", "name", "id"),
            ("work_model", "workModels", "workModelOptions", "label", "value"),
            ("contract_hours", "contractHours", "contractHoursOptions", "label", "value"),
        ] {
            for v in resolve(&cfg[opts], label, id, &arg_list(a, arg), arg)? {
                q.add(param, v);
            }
        }
    }
    let r = app.client.get_json(&format!("/services/students/jobs/search?{}", q.s())).await?;
    let p = &r["items"];
    let items = p["items"].as_array().cloned().unwrap_or_default();
    Ok(paged(items.iter().map(job_row).collect(), p))
}

async fn get_job(c: &Client, id: i64) -> Result<Value> {
    let r = c.get_json(&format!("/services/students/jobs/{id}?studentSiteId=1")).await?;
    let j = &r["job"];
    let text = |html: &Value| html.as_str().map(fragment_to_text);
    Ok(json!({
        "id": j["id"],
        "title": j["name"],
        "employer": r["employerName"],
        "employer_id": r["employerId"],
        "url": format!("{BASE}/students/jobs/{id}"),
        "locations": j["locations"],
        "types_of_work": j["typesOfWork"],
        "contract": format!("{} / {}", j["contractType"].as_str().unwrap_or("?"), j["contractHours"].as_str().unwrap_or("?")),
        "work_model": j["workModelDisplay"],
        "pay": j["remuneration"],
        "commences": j["commences"],
        "published": day(&j["publish"]),
        "closes": when(&j["expire"]),
        "expired_or_withdrawn": j["inactive"].as_bool().unwrap_or(false) || j["withdrawn"].as_bool().unwrap_or(false),
        "summary": j["summary"],
        "description": text(&j["detailsHtml"]),
        "how_to_apply": text(&j["applicationProceduresHtml"]),
        "application_info": text(&j["applicationInformationHtml"]),
        "apply_url": j["applicationUrl"],
        "contact": j["displayContact"],
        "employer_website": r["organisation"]["website"],
        "attachments": j["attachments"].as_array().map(|a| a.iter().map(|f| json!({
            "name": f["name"],
            "url": f["url"].as_str().map(|u| if u.starts_with('/') { format!("{BASE}{u}") } else { u.to_string() }),
        })).collect::<Vec<_>>()),
        "bookmarked": j["isBookmarked"],
    }))
}

fn event_row(e: &Value) -> Value {
    json!({
        "id": e["entityId"].as_i64().or(e["id"].as_i64()),
        "title": e["title"],
        "type": e["type"]["name"],
        "type_id": e["type"]["id"],
        "start": when(&e["start"]),
        "end": when(&e["end"]),
        "location": e["location"],
        "cancelled": e["isCancelled"].as_bool().filter(|b| *b),
        "booking": e["bookingType"],
    })
}

async fn search_events(c: &Client, a: &Value) -> Result<Value> {
    let mut q = Q::default();
    q.add("page", arg_int(a, "page").unwrap_or(1)).add("studentSiteId", 1);
    if let Some(t) = arg_str(a, "text") {
        q.add("text", t);
    }
    if let Some(t) = arg_int(a, "event_type_id") {
        q.add("eventTypeIds", t);
    }
    if let Some(f) = arg_str(a, "from") {
        q.add("fromStart", f);
    }
    if let Some(u) = arg_str(a, "until") {
        q.add("untilStart", u);
    }
    let r = c.get_json(&format!("/services/students/events/search?{}", q.s())).await?;
    let items = r["items"].as_array().cloned().unwrap_or_default();
    let rows = items
        .iter()
        .map(|i| {
            let e = &i["event"];
            let mut row = event_row(e);
            row["summary"] = e["summary"].clone();
            row
        })
        .collect();
    Ok(paged(rows, &r))
}

async fn get_event(c: &Client, id: i64) -> Result<Value> {
    let html = c.get_html(&format!("/students/events/detail/{id}")).await?;
    let doc = Html::parse_document(&html);
    let header = page_text(&doc, ".banner-info").or_else(|| page_text(&doc, ".banner-header"));
    let body = page_text(&doc, ".main-content .col-md-8");
    let link_sel = Selector::parse("a.external-booking-url-btn, a[href^='http'].btn-primary").unwrap();
    let booking_url = doc
        .select(&link_sel)
        .filter_map(|a| a.value().attr("href"))
        .find(|h| h.starts_with("http"))
        .map(String::from);
    if header.is_none() && body.is_none() {
        bail!("could not read event {id}");
    }
    Ok(json!({
        "id": id,
        "url": format!("{BASE}/students/events/detail/{id}"),
        "header": header,
        "details": body,
        "booking_url": booking_url,
    }))
}

async fn get_organisation(c: &Client, id: i64) -> Result<Value> {
    let (detail_path, jobs_path) =
        (format!("/students/organisations/detail/{id}"), format!("/students/organisations/jobs/{id}"));
    let (detail, jobs) = tokio::join!(c.get_html(&detail_path), c.get_html(&jobs_path));
    let detail = Html::parse_document(&detail?);
    let head = page_text(&detail, ".org-head");
    let about = page_text(&detail, ".main-content .col-md-9");
    let mut openings = Vec::new();
    if let Ok(jobs) = jobs {
        let doc = Html::parse_document(&jobs);
        let item_sel = Selector::parse(".job-list .list-group-item").unwrap();
        let link_sel = Selector::parse("a[href^='/students/jobs/']").unwrap();
        for item in doc.select(&item_sel) {
            let Some(link) = item.select(&link_sel).next() else { continue };
            let href = link.value().attr("href").unwrap_or_default();
            let jid: Option<i64> = href.trim_start_matches("/students/jobs/").split(['/', '?']).next().and_then(|s| s.parse().ok());
            openings.push(json!({
                "id": jid,
                "title": link.text().collect::<String>().trim(),
                "location": select_text(item, ".job-list-location"),
                "closes": select_text(item, ".job-list-close").map(|s| s.trim_start_matches("Closes - ").to_string()),
                "summary": select_text(item, ".job-list-summary"),
            }));
        }
    }
    Ok(json!({
        "id": id,
        "url": format!("{BASE}/students/organisations/detail/{id}"),
        "profile": head,
        "about": about,
        "openings": openings,
    }))
}

fn bookmark_row(b: &Value) -> Value {
    for kind in ["job", "event", "organisation", "faq", "infoPage", "document", "link", "video", "news", "activity"] {
        let e = &b[kind];
        if e.is_object() {
            return json!({
                "kind": kind,
                "id": b["entityId"],
                "title": e["title"].as_str().or(e["name"].as_str()),
                "start": when(&e["start"]),
            });
        }
    }
    b.clone()
}
