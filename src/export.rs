//! Export one chat to <name>.html / <name>.csv / <name>.json (+ images/).

use crate::graph::Graph;
use crate::model::{ChatInfo, ChatKind, Me};
use crate::util::*;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct ExportOptions {
    pub output_dir: PathBuf,
    pub include_images: bool,
    pub include_system: bool,
}

#[derive(Clone, Debug)]
pub struct ExportResult {
    pub label: String,
    pub kind: ChatKind,
    pub messages: usize,
    pub images: usize,
    pub folder: Option<PathBuf>,
    pub html: Option<PathBuf>,
    pub error: Option<String>,
}

struct Record {
    timestamp: String,
    sender: String,
    kind: String,
    message: String,
    attachments: String,
    reactions: String,
    edited: bool,
    deleted: bool,
    id: String,
}

pub struct Exporter<'a> {
    g: &'a Graph,
    me: &'a Me,
    /// user id -> display name (None = looked up, unknown)
    names: HashMap<String, Option<String>>,
}

fn graph_img_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)https://graph\.microsoft\.com/(?:v1\.0|beta)/chats/[^"'\s<>]+?/hostedContents/[^"'\s<>/]+/\$value"#)
            .unwrap()
    })
}

fn leftover_img_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)<img[^>]+src="https://graph\.microsoft\.com/[^"]*hostedContents[^"]*"[^>]*>"#,
        )
        .unwrap()
    })
}

fn cleanup_re() -> &'static [Regex; 2] {
    static RE: OnceLock<[Regex; 2]> = OnceLock::new();
    RE.get_or_init(|| {
        [
            Regex::new(r"(?is)<script.*?</script>").unwrap(),
            Regex::new(r"(?i)<attachment[^>]*>\s*</attachment>").unwrap(),
        ]
    })
}

fn ext_for(content_type: &str) -> &'static str {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("jpeg") || ct.contains("jpg") {
        "jpg"
    } else if ct.contains("gif") {
        "gif"
    } else if ct.contains("webp") {
        "webp"
    } else if ct.contains("bmp") {
        "bmp"
    } else {
        "png"
    }
}

/// File/folder base name: the other person's email for 1:1 chats, otherwise the chat name.
pub fn file_base(chat: &ChatInfo) -> String {
    match &chat.other_email {
        Some(e) => sanitize_filename(e, 80),
        None => sanitize_filename(&chat.label, 80),
    }
}

impl<'a> Exporter<'a> {
    pub fn new(g: &'a Graph, me: &'a Me) -> Self {
        let mut names = HashMap::new();
        names.insert(me.id.clone(), Some(me.name.clone()));
        Self { g, me, names }
    }

    fn name_for_id(&mut self, id: &str) -> Option<String> {
        if id.is_empty() {
            return None;
        }
        if let Some(n) = self.names.get(id) {
            return n.clone();
        }
        let n = self
            .g
            .get_json(&self.g.url(&format!("/users/{id}?$select=displayName")))
            .ok()
            .and_then(|v| jstr(&v, &["displayName"]).map(String::from));
        self.names.insert(id.to_string(), n.clone());
        n
    }

    fn sender_name(&mut self, m: &Value) -> String {
        if let Some(n) = jstr(m, &["from", "user", "displayName"]) {
            return n.to_string();
        }
        if let Some(id) = jstr(m, &["from", "user", "id"]) {
            let id = id.to_string();
            return self.name_for_id(&id).unwrap_or(id);
        }
        if let Some(n) = jstr(m, &["from", "application", "displayName"]) {
            return n.to_string();
        }
        "System".to_string()
    }

    fn identity_name(&mut self, ident: &Value) -> String {
        let u = jget(ident, &["user"]).unwrap_or(ident);
        if let Some(n) = jstr(u, &["displayName"]) {
            return n.to_string();
        }
        let id = jstr(u, &["id"]).unwrap_or("").to_string();
        self.name_for_id(&id).unwrap_or_else(|| "someone".into())
    }

    fn event_text(&mut self, m: &Value) -> String {
        let Some(d) = jget(m, &["eventDetail"]) else {
            return "System event".into();
        };
        let t = jstr(d, &["@odata.type"]).unwrap_or("");
        let t = t
            .trim_start_matches("#microsoft.graph.")
            .trim_end_matches("EventMessageDetail")
            .to_string();
        let who = jget(d, &["initiator"])
            .map(|i| self.identity_name(i))
            .unwrap_or_else(|| "Someone".into());
        let members: Vec<Value> = jarr(d, &["members"]).to_vec();
        let names = members
            .iter()
            .map(|x| self.identity_name(x))
            .collect::<Vec<_>>()
            .join(", ");
        match t.as_str() {
            "callEnded" => match jstr(d, &["callDuration"]).and_then(iso_duration) {
                Some(dur) => format!("Call ended (duration {dur})"),
                None => "Call ended".into(),
            },
            "callStarted" => "Call started".into(),
            "callRecording" => "Call recorded".into(),
            "callTranscript" => "Call transcript available".into(),
            "membersAdded" => format!("{who} added {names}"),
            "membersDeleted" => format!("{who} removed {names}"),
            "membersJoined" => format!("{names} joined"),
            "membersLeft" => format!("{names} left"),
            "chatRenamed" => format!(
                "{who} renamed the chat to '{}'",
                jstr(d, &["chatDisplayName"]).unwrap_or("")
            ),
            "" => "System event".into(),
            other => other.to_string(),
        }
    }

    /// Export one chat. `progress` receives human-readable status lines.
    pub fn export_chat(
        &mut self,
        chat: &ChatInfo,
        opts: &ExportOptions,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(String),
    ) -> ExportResult {
        let mut result = ExportResult {
            label: chat.label.clone(),
            kind: chat.kind,
            messages: 0,
            images: 0,
            folder: None,
            html: None,
            error: None,
        };
        match self.export_inner(chat, opts, cancel, progress, &mut result) {
            Ok(()) => {}
            Err(e) => result.error = Some(e),
        }
        result
    }

    fn export_inner(
        &mut self,
        chat: &ChatInfo,
        opts: &ExportOptions,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(String),
        result: &mut ExportResult,
    ) -> Result<(), String> {
        for m in &chat.members {
            if !m.user_id.is_empty() && !m.name.is_empty() {
                self.names.insert(m.user_id.clone(), Some(m.name.clone()));
            }
        }

        // --- download messages
        let url = self.g.url(&format!("/chats/{}/messages?$top=50", chat.id));
        let mut msgs = self
            .g
            .get_paged(&url, |n| {
                progress(format!("{n} messages downloaded"));
                !cancel.load(Ordering::Relaxed)
            })
            .map_err(|e| e.to_string())?;
        msgs.sort_by(|a, b| {
            let ta = jstr(a, &["createdDateTime"]).and_then(parse_time);
            let tb = jstr(b, &["createdDateTime"]).and_then(parse_time);
            ta.cmp(&tb)
        });
        if !opts.include_system {
            msgs.retain(|m| jstr(m, &["messageType"]) != Some("systemEventMessage"));
        }

        // --- output folder
        let base = file_base(chat);
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let mut folder = opts.output_dir.join(format!("TeamsChat_{base}_{stamp}"));
        let mut n = 2;
        while folder.exists() {
            folder = opts
                .output_dir
                .join(format!("TeamsChat_{base}_{stamp}_{n}"));
            n += 1;
        }
        std::fs::create_dir_all(&folder)
            .map_err(|e| format!("Could not create {}: {e}", folder.display()))?;
        let img_dir = folder.join("images");
        if opts.include_images {
            std::fs::create_dir_all(&img_dir).map_err(|e| e.to_string())?;
        }
        result.folder = Some(folder.clone());

        // --- build transcript
        let mut records: Vec<Record> = Vec::with_capacity(msgs.len());
        let mut bubbles = String::new();
        let mut last_day = String::new();
        let total = msgs.len();

        for (i, m) in msgs.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".into());
            }
            if i % 25 == 0 {
                progress(format!("Writing transcript {}/{}", i + 1, total));
            }
            let created = jstr(m, &["createdDateTime"]).and_then(parse_time);
            let ts = created
                .map(|d| d.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default();
            let hhmm = created
                .map(|d| d.format("%H:%M").to_string())
                .unwrap_or_default();
            let day = created
                .map(|d| d.format("%A, %d %B %Y").to_string())
                .unwrap_or_default();
            let id = jstr(m, &["id"]).unwrap_or("").to_string();

            if day != last_day {
                let _ = writeln!(
                    bubbles,
                    "<div class='day'><span>{}</span></div>",
                    html_escape(&day)
                );
                last_day = day;
            }

            if jstr(m, &["messageType"]) == Some("systemEventMessage") {
                let evt = self.event_text(m);
                let _ = writeln!(
                    bubbles,
                    "<div class='sys'>{} &middot; {}</div>",
                    html_escape(&hhmm),
                    html_escape(&evt)
                );
                records.push(Record {
                    timestamp: ts,
                    sender: "System".into(),
                    kind: "systemEventMessage".into(),
                    message: evt,
                    attachments: String::new(),
                    reactions: String::new(),
                    edited: false,
                    deleted: false,
                    id,
                });
                continue;
            }

            let sender = self.sender_name(m);
            let is_me = jstr(m, &["from", "user", "id"]) == Some(self.me.id.as_str());
            let deleted = jget(m, &["deletedDateTime"]).is_some();
            let edited = jget(m, &["lastEditedDateTime"]).is_some();

            // Body
            let raw = jstr(m, &["body", "content"]).unwrap_or("");
            let (mut body_html, mut plain) = if jstr(m, &["body", "contentType"]) == Some("html") {
                (raw.to_string(), html_to_text(raw))
            } else {
                (
                    html_escape(raw)
                        .replace("\r\n", "<br>")
                        .replace('\n', "<br>"),
                    raw.to_string(),
                )
            };
            let [script_re, attach_re] = cleanup_re();
            body_html = script_re.replace_all(&body_html, "").into_owned();
            body_html = attach_re.replace_all(&body_html, "").into_owned();

            // Inline images
            if opts.include_images {
                let urls: Vec<String> = graph_img_re()
                    .find_iter(&body_html)
                    .map(|x| x.as_str().to_string())
                    .collect();
                for (k, u) in urls.iter().enumerate() {
                    let tmp = img_dir.join(format!("{id}_{}.tmp", k + 1));
                    match self.g.download(u, &tmp) {
                        Ok(ct) => {
                            let file = format!("{id}_{}.{}", k + 1, ext_for(&ct));
                            let _ = std::fs::rename(&tmp, img_dir.join(&file));
                            body_html = body_html.replace(u.as_str(), &format!("images/{file}"));
                            result.images += 1;
                        }
                        Err(e) => {
                            let _ = std::fs::remove_file(&tmp);
                            progress(format!("Could not download an image: {e}"));
                        }
                    }
                }
            }
            body_html = leftover_img_re()
                .replace_all(&body_html, "<span class='ph'>[image not exported]</span>")
                .into_owned();

            // Attachments
            let mut quote_html = String::new();
            let mut attach_html = String::new();
            let mut attach_text: Vec<String> = Vec::new();
            for a in jarr(m, &["attachments"]) {
                let ct = jstr(a, &["contentType"]).unwrap_or("");
                let url = jstr(a, &["contentUrl"]).unwrap_or("");
                if ct == "reference" {
                    let name = jstr(a, &["name"]).unwrap_or(url);
                    attach_text.push(format!("File: {name} ({url})"));
                    let _ = write!(
                        attach_html,
                        "<div class='att'>&#128206; <a href='{}' target='_blank'>{}</a></div>",
                        html_escape(url),
                        html_escape(name)
                    );
                } else if ct == "messageReference" {
                    let q: Value = jstr(a, &["content"])
                        .and_then(|c| serde_json::from_str(c).ok())
                        .unwrap_or(Value::Null);
                    let qs = jstr(&q, &["messageSender", "user", "displayName"]).unwrap_or("");
                    let qp = jstr(&q, &["messagePreview"]).unwrap_or("");
                    attach_text.push(format!("Reply to {qs}: {qp}"));
                    let _ = write!(
                        quote_html,
                        "<div class='quote'><b>{}</b><br>{}</div>",
                        html_escape(qs),
                        html_escape(qp)
                    );
                } else if ct.starts_with("application/vnd.microsoft.card") {
                    attach_text.push("Card".into());
                    attach_html.push_str("<div class='att'>[Card / app content]</div>");
                } else {
                    let name = jstr(a, &["name"]).unwrap_or(ct);
                    attach_text.push(name.to_string());
                    let link = if url.is_empty() {
                        html_escape(name)
                    } else {
                        format!(
                            "<a href='{}' target='_blank'>{}</a>",
                            html_escape(url),
                            html_escape(name)
                        )
                    };
                    let _ = write!(attach_html, "<div class='att'>&#128206; {link}</div>");
                }
            }

            // Reactions (grouped, in first-seen order)
            let mut rx: Vec<(String, usize)> = Vec::new();
            for r in jarr(m, &["reactions"]) {
                let t = jstr(r, &["reactionType"]).unwrap_or("?").to_string();
                match rx.iter_mut().find(|(k, _)| *k == t) {
                    Some((_, c)) => *c += 1,
                    None => rx.push((t, 1)),
                }
            }
            let rx: Vec<String> = rx.into_iter().map(|(k, c)| format!("{k} x{c}")).collect();

            if deleted {
                body_html = "<i class='muted'>This message was deleted.</i>".into();
                plain = "[deleted]".into();
            }

            let mut meta = html_escape(&hhmm);
            if edited && !deleted {
                meta.push_str(" &middot; edited");
            }
            let cls = if is_me { "msg me" } else { "msg them" };
            let name_style = if is_me {
                String::new()
            } else {
                format!(" style='color:{}'", name_color(&sender))
            };
            let rx_html = if rx.is_empty() {
                String::new()
            } else {
                format!("<div class='rx'>{}</div>", html_escape(&rx.join("   ")))
            };
            let _ = writeln!(
                bubbles,
                "<div class='{cls}'>\n  <div class='hdr'><b{name_style}>{}</b> <span class='muted'>{meta}</span></div>\n  {quote_html}<div class='body'>{body_html}</div>{attach_html}{rx_html}\n</div>",
                html_escape(&sender)
            );

            records.push(Record {
                timestamp: ts,
                sender,
                kind: jstr(m, &["messageType"]).unwrap_or("message").to_string(),
                message: plain,
                attachments: attach_text.join("; "),
                reactions: rx.join(", "),
                edited,
                deleted,
                id,
            });
        }

        // --- header
        let mut participants = vec![if self.me.upn.is_empty() {
            self.me.name.clone()
        } else {
            format!("{} <{}>", self.me.name, self.me.upn)
        }];
        participants.extend(
            chat.members
                .iter()
                .filter(|m| m.user_id != self.me.id)
                .map(|m| {
                    if m.email.is_empty() {
                        m.display().to_string()
                    } else {
                        format!("{} <{}>", m.display(), m.email)
                    }
                }),
        );
        let subtitle = format!(
            "{} chat · {} participant{}: {}",
            chat.kind.label(),
            participants.len(),
            if participants.len() == 1 { "" } else { "s" },
            participants.join(" · ")
        );
        let first = records.first().map(|r| r.timestamp.as_str()).unwrap_or("-");
        let last = records.last().map(|r| r.timestamp.as_str()).unwrap_or("-");
        let html = format!(
            r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8">
<title>Teams chat - {title}</title>
<style>
 body{{font-family:'Segoe UI',Arial,sans-serif;background:#f5f5f5;margin:0;color:#242424}}
 header{{background:#464775;color:#fff;padding:16px 24px}}
 header h1{{margin:0;font-size:20px}} header p{{margin:4px 0 0;font-size:13px;opacity:.85}}
 main{{max-width:900px;margin:0 auto;padding:16px}}
 .day{{text-align:center;margin:20px 0 8px}} .day span{{background:#e1e1e1;border-radius:10px;padding:2px 10px;font-size:12px}}
 .sys{{text-align:center;color:#616161;font-size:12px;margin:6px 0}}
 .msg{{max-width:75%;margin:6px 0;padding:8px 12px;border-radius:8px;background:#fff;box-shadow:0 1px 2px rgba(0,0,0,.1);word-wrap:break-word}}
 .msg.me{{margin-left:auto;background:#e8ebfa}}
 .hdr{{font-size:12px;margin-bottom:4px}} .muted{{color:#707070;font-weight:normal}}
 .body img{{max-width:100%;height:auto}}
 .body at{{font-weight:600;color:#6264a7}}
 .quote{{border-left:3px solid #6264a7;background:rgba(0,0,0,.04);padding:4px 8px;margin-bottom:6px;font-size:12px}}
 .att{{font-size:13px;margin-top:4px}} .rx{{font-size:12px;color:#616161;margin-top:4px}}
 .ph{{color:#a0a0a0;font-style:italic}}
</style></head><body>
<header><h1>{title}</h1>
<p>{subtitle}</p>
<p>{count} messages &middot; {first} to {last} &middot; exported {now} by {me}</p></header>
<main>
{bubbles}
</main></body></html>
"#,
            title = html_escape(&chat.label),
            subtitle = html_escape(&subtitle),
            count = records.len(),
            now = chrono::Local::now().format("%Y-%m-%d %H:%M"),
            me = html_escape(&self.me.name),
        );

        // --- write files
        let html_path = folder.join(format!("{base}.html"));
        std::fs::write(&html_path, html).map_err(|e| e.to_string())?;
        write_csv(&folder.join(format!("{base}.csv")), &records)?;
        let json = serde_json::to_string_pretty(&msgs).map_err(|e| e.to_string())?;
        std::fs::write(folder.join(format!("{base}.json")), json).map_err(|e| e.to_string())?;

        result.messages = records.len();
        result.html = Some(html_path);
        Ok(())
    }
}

fn write_csv(path: &Path, records: &[Record]) -> Result<(), String> {
    let mut f = std::fs::File::create(path).map_err(|e| e.to_string())?;
    // UTF-8 BOM so Excel shows non-ASCII text correctly
    std::io::Write::write_all(&mut f, b"\xEF\xBB\xBF").map_err(|e| e.to_string())?;
    let mut w = csv::Writer::from_writer(f);
    w.write_record([
        "Timestamp",
        "Sender",
        "Type",
        "Message",
        "Attachments",
        "Reactions",
        "Edited",
        "Deleted",
        "MessageId",
    ])
    .map_err(|e| e.to_string())?;
    for r in records {
        w.write_record([
            r.timestamp.as_str(),
            r.sender.as_str(),
            r.kind.as_str(),
            r.message.as_str(),
            r.attachments.as_str(),
            r.reactions.as_str(),
            if r.edited { "True" } else { "False" },
            if r.deleted { "True" } else { "False" },
            r.id.as_str(),
        ])
        .map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    //! End-to-end test against a local mock of Microsoft Graph.
    use super::*;
    use crate::auth::Token;
    use crate::graph::{http_client, Graph};
    use crate::model::{get_me, list_chats};
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn mock_graph() -> (String, Arc<AtomicUsize>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let throttled = Arc::new(AtomicUsize::new(0));
        let th = Arc::clone(&throttled);
        std::thread::spawn(move || {
            let g = "https://graph.microsoft.com/v1.0";
            for req in server.incoming_requests() {
                let url = req.url().to_string();
                assert!(
                    req.headers().iter().any(|h| h.field.equiv("Authorization")
                        && h.value.as_str() == "Bearer test-token"),
                    "missing bearer token"
                );
                let ok = |v: Value| {
                    tiny_http::Response::from_string(v.to_string()).with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    )
                };
                if url.starts_with("/v1.0/me?") {
                    let _ = req.respond(ok(
                        json!({"id":"aaa","displayName":"Chandra","userPrincipalName":"me@x.com"}),
                    ));
                } else if url.starts_with("/v1.0/me/chats") && url.contains("page2") {
                    let _ = req.respond(ok(json!({"value":[
                        {"id":"19:g1@thread.v2","chatType":"group","topic":"Project: Phoenix",
                         "lastMessagePreview":{"createdDateTime":"2026-10-05T10:00:00Z"},
                         "members":[{"userId":"aaa","displayName":"Chandra"},
                                    {"userId":"bbb","displayName":"John Doe","email":"john@x.com"},
                                    {"userId":"ccc","displayName":"Mary Jane","email":"mary@x.com"}]}
                    ]})));
                } else if url.starts_with("/v1.0/me/chats") {
                    let _ = req.respond(ok(json!({"value":[
                        {"id":"19:aaa_bbb@unq.gbl.spaces","chatType":"oneOnOne",
                         "lastMessagePreview":{"createdDateTime":"2026-10-01T09:01:00Z"},
                         "members":[{"userId":"aaa","displayName":"Chandra","email":"me@x.com"},
                                    {"userId":"bbb","displayName":"John Doe","email":"John@X.com"}]}
                    ], "@odata.nextLink": format!("{g}/me/chats?page2")})));
                } else if url.contains("/hostedContents/") {
                    let _ = req.respond(
                        tiny_http::Response::from_data(vec![0x89, b'P', b'N', b'G']).with_header(
                            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"image/jpeg"[..])
                                .unwrap(),
                        ),
                    );
                } else if url.starts_with("/v1.0/chats/") && url.contains("/messages") {
                    if th.fetch_add(1, Ordering::SeqCst) == 0 {
                        let _ = req.respond(tiny_http::Response::empty(429).with_header(
                            tiny_http::Header::from_bytes(&b"Retry-After"[..], &b"1"[..]).unwrap(),
                        ));
                        continue;
                    }
                    let chat = url
                        .trim_start_matches("/v1.0/chats/")
                        .split('/')
                        .next()
                        .unwrap()
                        .to_string();
                    let _ = req.respond(ok(json!({"value":[
                        {"id":"2","messageType":"message","createdDateTime":"2026-10-01T09:05:00Z",
                         "lastEditedDateTime":"2026-10-01T09:06:00Z",
                         "from":{"user":{"id":"bbb","displayName":"John Doe"}},
                         "body":{"contentType":"html","content":format!("<p>Sure &amp; see <attachment id=\"x\"></attachment><img src=\"{g}/chats/{chat}/messages/2/hostedContents/abc/$value\"></p>")},
                         "attachments":[
                            {"id":"x","contentType":"reference","name":"Spec.docx","contentUrl":"https://x.sharepoint.com/Spec.docx"},
                            {"contentType":"messageReference","content":"{\"messagePreview\":\"Can you send the spec?\",\"messageSender\":{\"user\":{\"displayName\":\"Chandra\"}}}"}],
                         "reactions":[{"reactionType":"like"},{"reactionType":"like"},{"reactionType":"heart"}]},
                        {"id":"1","messageType":"message","createdDateTime":"2026-10-01T09:00:00Z",
                         "from":{"user":{"id":"aaa","displayName":"Chandra"}},
                         "body":{"contentType":"text","content":"Can you send the spec?\n<b>Thanks</b>"},"attachments":[],"reactions":[]},
                        {"id":"3","messageType":"systemEventMessage","createdDateTime":"2026-10-01T08:00:00Z",
                         "eventDetail":{"@odata.type":"#microsoft.graph.membersAddedEventMessageDetail",
                                        "initiator":{"user":{"id":"aaa"}},"members":[{"id":"ccc"},{"id":"zzz"}]}},
                        {"id":"4","messageType":"systemEventMessage","createdDateTime":"2026-10-02T08:00:00Z",
                         "eventDetail":{"@odata.type":"#microsoft.graph.callEndedEventMessageDetail","callDuration":"PT5M12S"}},
                        {"id":"5","messageType":"message","createdDateTime":"2026-10-02T09:00:00Z","deletedDateTime":"2026-10-02T09:01:00Z",
                         "from":{"user":{"id":"bbb","displayName":"John Doe"}},"body":{"contentType":"html","content":""}}
                    ]})));
                } else if url.starts_with("/v1.0/users/zzz") {
                    let _ = req.respond(ok(json!({"displayName":"Ex Colleague"})));
                } else if url.starts_with("/v1.0/users/ccc") {
                    let _ = req.respond(ok(json!({"displayName":"Mary Jane"})));
                } else {
                    let _ = req.respond(
                        tiny_http::Response::from_string(r#"{"error":{"message":"not found"}}"#)
                            .with_status_code(404),
                    );
                }
            }
        });
        (format!("http://127.0.0.1:{port}/v1.0"), throttled)
    }

    #[test]
    fn end_to_end_export() {
        let (base, throttled) = mock_graph();
        let token = Token {
            access_token: "test-token".into(),
            refresh_token: None,
            expires_at: Instant::now() + Duration::from_secs(3600),
        };
        let g = Graph::new(http_client(), token, None).with_base(base);
        let me = get_me(&g).unwrap();
        assert_eq!(me.name, "Chandra");

        let chats = list_chats(&g, &me, |_| true).unwrap();
        assert_eq!(chats.len(), 2, "paging should collect both pages");
        assert_eq!(chats[0].label, "Project: Phoenix", "newest first");
        assert_eq!(chats[1].label, "John Doe");
        assert_eq!(chats[1].other_email.as_deref(), Some("john@x.com"));
        assert!(chats[0].search_text.contains("mary@x.com"));

        let out = std::env::temp_dir().join(format!("tce-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        let opts = ExportOptions {
            output_dir: out.clone(),
            include_images: true,
            include_system: true,
        };
        let cancel = AtomicBool::new(false);
        let mut ex = Exporter::new(&g, &me);
        let mut log = Vec::new();

        let r1 = ex.export_chat(&chats[1], &opts, &cancel, &mut |s| log.push(s));
        assert!(r1.error.is_none(), "{:?}", r1.error);
        assert!(
            throttled.load(Ordering::SeqCst) >= 2,
            "429 should have been retried"
        );
        assert_eq!(r1.messages, 5);
        assert_eq!(r1.images, 1);
        let folder = r1.folder.clone().unwrap();
        assert!(folder
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("TeamsChat_john@x.com_"));
        assert!(folder.join("john@x.com.csv").exists());
        assert!(folder.join("john@x.com.json").exists());
        assert!(folder.join("images/2_1.jpg").exists());
        let html = std::fs::read_to_string(folder.join("john@x.com.html")).unwrap();
        assert!(html.contains("src=\"images/2_1.jpg\""));
        assert!(html.contains("Spec.docx"));
        assert!(html.contains("like x2   heart x1"));
        assert!(
            html.contains("&lt;b&gt;Thanks&lt;/b&gt;"),
            "plain text must be escaped"
        );
        assert!(html.contains("Chandra added Mary Jane, Ex Colleague"));
        assert!(html.contains("Call ended (duration 00:05:12)"));
        assert!(html.contains("This message was deleted."));
        assert!(html.contains(
            "1:1 chat · 2 participants: Chandra &lt;me@x.com&gt; · John Doe &lt;John@X.com&gt;"
        ));
        let csv = std::fs::read_to_string(folder.join("john@x.com.csv")).unwrap();
        let first_data = csv.lines().nth(1).unwrap();
        assert!(
            first_data.contains("System"),
            "oldest (system event) first: {first_data}"
        );
        assert!(csv.contains("Sure & see [image]"));

        let opts2 = ExportOptions {
            include_images: false,
            include_system: false,
            ..opts
        };
        let r2 = ex.export_chat(&chats[0], &opts2, &cancel, &mut |_| {});
        assert!(r2.error.is_none(), "{:?}", r2.error);
        assert_eq!(r2.messages, 3, "system messages excluded");
        assert_eq!(r2.images, 0);
        let f2 = r2.folder.unwrap();
        assert!(f2.join("Project_ Phoenix.html").exists());
        let html2 = std::fs::read_to_string(f2.join("Project_ Phoenix.html")).unwrap();
        assert!(html2.contains("[image not exported]"));
        assert!(html2.contains("Group chat · 3 participants: Chandra &lt;me@x.com&gt; · John Doe &lt;john@x.com&gt; · Mary Jane &lt;mary@x.com&gt;"));

        // cancel
        cancel.store(true, Ordering::SeqCst);
        let r3 = ex.export_chat(&chats[0], &opts2, &cancel, &mut |_| {});
        assert_eq!(r3.error.as_deref(), Some("Cancelled"));

        let _ = std::fs::remove_dir_all(&out);
    }
}
