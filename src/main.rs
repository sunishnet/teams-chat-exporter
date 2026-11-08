// Hide the console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod auth;
mod export;
mod graph;
mod model;
mod util;

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use export::{ExportOptions, ExportResult, Exporter};
use graph::Graph;
use model::{ChatInfo, ChatKind, Me, Member};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x46, 0x47, 0x75);
const ACCENT_LIGHT: egui::Color32 = egui::Color32::from_rgb(0xe9, 0xea, 0xfe);

// ---------------------------------------------------------------- settings

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Settings {
    tenant: String,
    client_id: String,
    output_dir: String,
    include_images: bool,
    include_system: bool,
    cache_chat_list: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let out = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Teams Chat Exports");
        Self {
            tenant: auth::DEFAULT_TENANT.into(),
            client_id: auth::DEFAULT_CLIENT_ID.into(),
            output_dir: out.to_string_lossy().into_owned(),
            include_images: true,
            include_system: true,
            cache_chat_list: true,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct CacheScope {
    tenant: String,
    client_id: String,
    me_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedMember {
    user_id: String,
    name: String,
    email: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedChat {
    id: String,
    kind: String,
    label: String,
    members: Vec<CachedMember>,
    last_activity: Option<i64>,
    other_email: Option<String>,
    search_text: String,
}

impl From<&ChatInfo> for CachedChat {
    fn from(chat: &ChatInfo) -> Self {
        let kind = match chat.kind {
            ChatKind::OneOnOne => "oneOnOne",
            ChatKind::SelfChat => "selfChat",
            ChatKind::Group => "group",
            ChatKind::Meeting => "meeting",
            ChatKind::Other => "other",
        };
        Self {
            id: chat.id.clone(),
            kind: kind.into(),
            label: chat.label.clone(),
            members: chat
                .members
                .iter()
                .map(|member| CachedMember {
                    user_id: member.user_id.clone(),
                    name: member.name.clone(),
                    email: member.email.clone(),
                })
                .collect(),
            last_activity: chat.last_activity.map(|time| time.timestamp()),
            other_email: chat.other_email.clone(),
            search_text: chat.search_text.clone(),
        }
    }
}

impl CachedChat {
    fn into_chat(self) -> ChatInfo {
        let last_activity = self.last_activity.and_then(|timestamp| {
            chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
                .map(|time| time.with_timezone(&chrono::Local))
        });
        ChatInfo {
            id: self.id,
            kind: ChatKind::from_graph(&self.kind),
            label: self.label,
            members: self
                .members
                .into_iter()
                .map(|member| Member {
                    user_id: member.user_id,
                    name: member.name,
                    email: member.email,
                })
                .collect(),
            last_activity,
            other_email: self.other_email,
            search_text: self.search_text,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct ChatCache {
    scope: CacheScope,
    cached_at: i64,
    chats: Vec<CachedChat>,
}

// ---------------------------------------------------------------- worker messages

enum Msg {
    Log(String),
    SignedIn(Arc<Graph>, Me),
    SignInFailed(String),
    ChatBatch(Vec<ChatInfo>, usize),
    ChatsLoaded,
    ChatsFailed(String),
    Progress {
        index: usize,
        total: usize,
        label: String,
        detail: String,
    },
    ExportDone(Vec<ExportResult>),
}

#[derive(PartialEq, Clone, Copy)]
enum Busy {
    SigningIn,
    LoadingChats,
    Exporting,
}

// ---------------------------------------------------------------- app

struct App {
    settings: Settings,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    graph: Option<Arc<Graph>>,
    me: Option<Me>,
    chats: Vec<ChatInfo>,
    pending_chats: Vec<ChatInfo>,
    chat_cache: Option<ChatCache>,
    selected: HashSet<String>,
    search: String,
    email_autoselect_query: String,
    show_one: bool,
    show_self: bool,
    show_group: bool,
    show_meeting: bool,
    busy: Option<Busy>,
    status: String,
    log: Vec<String>,
    progress: Option<(usize, usize, String, String)>,
    results: Option<Vec<ExportResult>>,
    cancel: Arc<AtomicBool>,
    show_connection: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();
        let chat_cache: Option<ChatCache> = cc
            .storage
            .and_then(|s| eframe::get_value(s, "teams-chat-exporter.chat-cache"))
            .filter(|_| settings.cache_chat_list);
        style(&cc.egui_ctx);
        let (tx, rx) = channel();
        #[allow(unused_mut)]
        let mut app = Self {
            settings,
            tx,
            rx,
            graph: None,
            me: None,
            chats: Vec::new(),
            pending_chats: Vec::new(),
            chat_cache,
            selected: HashSet::new(),
            search: String::new(),
            email_autoselect_query: String::new(),
            show_one: true,
            show_self: true,
            show_group: true,
            show_meeting: true,
            busy: None,
            status: "Not signed in".into(),
            log: Vec::new(),
            progress: None,
            results: None,
            cancel: Arc::new(AtomicBool::new(false)),
            show_connection: false,
        };
        // Debug builds only: TCE_DEMO=1 shows sample chats without signing in (UI preview).
        #[cfg(debug_assertions)]
        if std::env::var_os("TCE_DEMO").is_some() {
            app.load_demo();
        }
        app
    }

    #[cfg(debug_assertions)]
    fn load_demo(&mut self) {
        use serde_json::json;
        let me = Me {
            id: "me".into(),
            name: "Chandra".into(),
            upn: "chandra@contoso.com".into(),
        };
        let m = |id: &str, n: &str, e: &str| json!({"userId": id, "displayName": n, "email": e});
        let raw = [
            json!({"id":"1","chatType":"oneOnOne","lastMessagePreview":{"createdDateTime":"2026-11-04T05:10:00Z"},
                   "members":[m("me","Chandra","chandra@contoso.com"), m("b","John Doe","john.doe@contoso.com")]}),
            json!({"id":"2","chatType":"group","topic":"Project Phoenix - delivery","lastMessagePreview":{"createdDateTime":"2026-11-03T12:44:00Z"},
                   "members":[m("me","Chandra",""), m("b","John Doe","john.doe@contoso.com"), m("c","Mary Jane","mary@contoso.com"), m("d","Raj Kumar","raj@contoso.com")]}),
            json!({"id":"3","chatType":"meeting","topic":"Weekly sync: Budget Q3","lastMessagePreview":{"createdDateTime":"2026-11-02T09:30:00Z"},
                   "members":[m("me","Chandra",""), m("c","Mary Jane","mary@contoso.com"), m("e","Priya Nair","priya@contoso.com")]}),
            json!({"id":"4","chatType":"group","lastMessagePreview":{"createdDateTime":"2026-10-28T16:02:00Z"},
                   "members":[m("me","Chandra",""), m("c","Mary Jane","mary@contoso.com"), m("d","Raj Kumar","raj@contoso.com")]}),
            json!({"id":"5","chatType":"oneOnOne","lastMessagePreview":{"createdDateTime":"2026-10-21T11:15:00Z"},
                   "members":[m("me","Chandra",""), m("e","Priya Nair","priya@contoso.com")]}),
            json!({"id":"6","chatType":"meeting","topic":"SharePoint migration kickoff","lastMessagePreview":{"createdDateTime":"2026-10-14T08:00:00Z"},
                   "members":[m("me","Chandra",""), m("b","John Doe",""), m("f","Alex Wong","alex@fabrikam.com"), m("g","Sam Lee","sam@fabrikam.com"), m("h","Ana Ruiz","ana@fabrikam.com"), m("i","Tom Hall","tom@fabrikam.com")]}),
        ];
        self.chats = raw
            .iter()
            .map(|v| ChatInfo::from_graph(v, &me.id))
            .collect();
        self.selected.insert("2".into());
        self.selected.insert("3".into());
        self.status = format!("{} chats loaded", self.chats.len());
        self.me = Some(me);
    }

    fn push_log(&mut self, s: String) {
        let stamp = chrono::Local::now().format("%H:%M:%S");
        for line in s.lines() {
            self.log.push(format!("{stamp}  {line}"));
        }
        if self.log.len() > 500 {
            let cut = self.log.len() - 500;
            self.log.drain(..cut);
        }
    }

    fn cache_scope(&self, me: &Me) -> CacheScope {
        CacheScope {
            tenant: non_empty(&self.settings.tenant, auth::DEFAULT_TENANT),
            client_id: non_empty(&self.settings.client_id, auth::DEFAULT_CLIENT_ID),
            me_id: me.id.clone(),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Log(s) => self.push_log(s),
                Msg::SignedIn(g, me) => {
                    self.push_log(format!("Signed in as {} ({})", me.name, me.upn));
                    let scope = self.cache_scope(&me);
                    let cached = self
                        .settings
                        .cache_chat_list
                        .then(|| self.chat_cache.clone())
                        .flatten()
                        .filter(|cache| cache.scope == scope);
                    if let Some(cache) = cached {
                        self.chats = cache.chats.into_iter().map(CachedChat::into_chat).collect();
                        self.chats
                            .sort_by_key(|chat| std::cmp::Reverse(chat.last_activity));
                        let cached_at =
                            chrono::DateTime::<chrono::Utc>::from_timestamp(cache.cached_at, 0)
                                .map(|time| {
                                    time.with_timezone(&chrono::Local)
                                        .format("%Y-%m-%d %H:%M")
                                        .to_string()
                                })
                                .unwrap_or_else(|| "an earlier session".into());
                        self.status = format!(
                            "Showing {} saved chats from {cached_at}; refreshing…",
                            self.chats.len()
                        );
                    } else {
                        self.chats.clear();
                        self.selected.clear();
                    }
                    self.graph = Some(g);
                    self.me = Some(me);
                    self.busy = None;
                    self.load_chats(ctx);
                }
                Msg::SignInFailed(e) => {
                    self.push_log(format!("Sign-in failed: {e}"));
                    self.status = format!("Sign-in failed: {e}");
                    self.busy = None;
                }
                Msg::ChatBatch(batch, count) => {
                    let mut fresh_ids: HashSet<String> = self
                        .pending_chats
                        .iter()
                        .map(|chat| chat.id.clone())
                        .collect();
                    let mut visible_ids: HashSet<String> =
                        self.chats.iter().map(|chat| chat.id.clone()).collect();
                    for chat in batch {
                        if fresh_ids.insert(chat.id.clone()) {
                            self.pending_chats.push(chat.clone());
                        }
                        if visible_ids.insert(chat.id.clone()) {
                            self.chats.push(chat);
                        }
                    }
                    self.status = format!("Loading chats… {} found", count);
                    self.email_autoselect_query.clear();
                    self.select_email_matches();
                }
                Msg::ChatsLoaded => {
                    self.pending_chats
                        .sort_by_key(|chat| std::cmp::Reverse(chat.last_activity));
                    self.chats = std::mem::take(&mut self.pending_chats);
                    let valid_ids: HashSet<&str> =
                        self.chats.iter().map(|chat| chat.id.as_str()).collect();
                    self.selected.retain(|id| valid_ids.contains(id.as_str()));
                    self.status = format!("{} chats loaded", self.chats.len());
                    self.push_log(self.status.clone());
                    if self.settings.cache_chat_list {
                        if let Some(me) = self.me.clone() {
                            self.chat_cache = Some(ChatCache {
                                scope: self.cache_scope(&me),
                                cached_at: chrono::Utc::now().timestamp(),
                                chats: self.chats.iter().map(CachedChat::from).collect(),
                            });
                        }
                    } else {
                        self.chat_cache = None;
                    }
                    self.busy = None;
                }
                Msg::ChatsFailed(e) => {
                    self.push_log(format!("Could not load chats: {e}"));
                    self.status = format!("Could not load chats: {e}");
                    self.busy = None;
                }
                Msg::Progress {
                    index,
                    total,
                    label,
                    detail,
                } => self.progress = Some((index, total, label, detail)),
                Msg::ExportDone(r) => {
                    let ok = r.iter().filter(|x| x.error.is_none()).count();
                    let msgs: usize = r.iter().map(|x| x.messages).sum();
                    self.status = format!(
                        "Export finished: {ok} of {} chats, {msgs} messages",
                        r.len()
                    );
                    self.push_log(self.status.clone());
                    for x in &r {
                        if let Some(e) = &x.error {
                            self.push_log(format!("  {} - FAILED: {e}", x.label));
                        }
                    }
                    self.results = Some(r);
                    self.progress = None;
                    self.busy = None;
                }
            }
        }
    }

    // ------------------------------------------------------------ actions

    fn sign_in(&mut self, ctx: &egui::Context) {
        let cfg = auth::AuthConfig {
            tenant: non_empty(&self.settings.tenant, auth::DEFAULT_TENANT),
            client_id: non_empty(&self.settings.client_id, auth::DEFAULT_CLIENT_ID),
        };
        self.busy = Some(Busy::SigningIn);
        self.status = "Waiting for you to sign in in the browser...".into();
        self.cancel.store(false, Ordering::Relaxed);
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let cancel = Arc::clone(&self.cancel);
        std::thread::spawn(move || {
            let http = graph::http_client();
            let log = |s: String| {
                let _ = tx.send(Msg::Log(s));
                ctx.request_repaint();
            };
            let res = auth::sign_in_interactive(&cfg, &http, &cancel, &log).and_then(|tok| {
                let g = Graph::new(http.clone(), tok, Some(cfg.clone()));
                let me = model::get_me(&g).map_err(|e| e.to_string())?;
                Ok((g, me))
            });
            let _ = match res {
                Ok((g, me)) => tx.send(Msg::SignedIn(Arc::new(g), me)),
                Err(e) => tx.send(Msg::SignInFailed(e)),
            };
            ctx.request_repaint();
        });
    }

    fn sign_out(&mut self) {
        self.graph = None;
        self.me = None;
        self.chats.clear();
        self.pending_chats.clear();
        self.selected.clear();
        self.status = "Signed out".into();
        self.push_log("Signed out.".into());
    }

    fn load_chats(&mut self, ctx: &egui::Context) {
        let (Some(g), Some(me)) = (self.graph.clone(), self.me.clone()) else {
            return;
        };
        self.pending_chats.clear();
        self.busy = Some(Busy::LoadingChats);
        self.status = if self.chats.is_empty() {
            "Loading your chats...".into()
        } else {
            format!(
                "Refreshing your chats… showing {} last-known chats",
                self.chats.len()
            )
        };
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let res = model::list_chats_stream(&g, &me, |batch, n| {
                let _ = tx.send(Msg::ChatBatch(batch, n));
                ctx.request_repaint();
                true
            });
            let _ = match res {
                Ok(()) => tx.send(Msg::ChatsLoaded),
                Err(e) => tx.send(Msg::ChatsFailed(e.to_string())),
            };
            ctx.request_repaint();
        });
    }

    fn start_export(&mut self, ctx: &egui::Context) {
        let (Some(g), Some(me)) = (self.graph.clone(), self.me.clone()) else {
            return;
        };
        let chats: Vec<ChatInfo> = self
            .chats
            .iter()
            .filter(|c| self.selected.contains(&c.id))
            .cloned()
            .collect();
        if chats.is_empty() {
            return;
        }
        let opts = ExportOptions {
            output_dir: PathBuf::from(&self.settings.output_dir),
            include_images: self.settings.include_images,
            include_system: self.settings.include_system,
        };
        if let Err(e) = std::fs::create_dir_all(&opts.output_dir) {
            self.status = format!("Can't use output folder: {e}");
            return;
        }
        self.busy = Some(Busy::Exporting);
        self.results = None;
        self.cancel.store(false, Ordering::Relaxed);
        self.push_log(format!(
            "Exporting {} chat(s) to {}",
            chats.len(),
            opts.output_dir.display()
        ));
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let cancel = Arc::clone(&self.cancel);
        std::thread::spawn(move || {
            let mut ex = Exporter::new(&g, &me);
            let total = chats.len();
            let mut results = Vec::new();
            for (i, chat) in chats.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send(Msg::Log(format!(
                    "[{}/{total}] {} ({})",
                    i + 1,
                    chat.label,
                    chat.kind.label()
                )));
                let r = ex.export_chat(chat, &opts, &cancel, &mut |detail| {
                    let _ = tx.send(Msg::Progress {
                        index: i,
                        total,
                        label: chat.label.clone(),
                        detail,
                    });
                    ctx.request_repaint();
                });
                if r.error.is_none() {
                    let _ = tx.send(Msg::Log(format!(
                        "      {} messages, {} images",
                        r.messages, r.images
                    )));
                }
                results.push(r);
            }
            let _ = tx.send(Msg::ExportDone(results));
            ctx.request_repaint();
        });
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.search.trim().to_lowercase();
        self.chats
            .iter()
            .enumerate()
            .filter(|(_, c)| match c.kind {
                ChatKind::OneOnOne => self.show_one,
                ChatKind::SelfChat => self.show_self,
                ChatKind::Group => self.show_group,
                ChatKind::Meeting => self.show_meeting,
                ChatKind::Other => true,
            })
            .filter(|(_, c)| {
                q.is_empty() || q.split_whitespace().all(|w| c.search_text.contains(w))
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn select_email_matches(&mut self) {
        let email = self.search.trim().to_lowercase();
        if !email.contains('@') {
            self.email_autoselect_query.clear();
            return;
        }
        if email == self.email_autoselect_query {
            return;
        }
        self.email_autoselect_query = email.clone();
        let matches: Vec<String> = self
            .chats
            .iter()
            .filter(|c| {
                c.members
                    .iter()
                    .any(|m| m.email.eq_ignore_ascii_case(&email))
            })
            .map(|c| c.id.clone())
            .collect();
        self.selected.extend(matches);
    }

    // ------------------------------------------------------------ UI parts

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::WHITE)
            .show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading(
                egui::RichText::new("Teams Chat Exporter")
                    .strong()
                    .color(ACCENT),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.busy.is_some();
                if let Some(me) = self.me.clone() {
                    if ui
                        .add_enabled(!busy, egui::Button::new("Sign out"))
                        .clicked()
                    {
                        self.sign_out();
                        return;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("⟳ Refresh chats"))
                        .clicked()
                    {
                        self.load_chats(ui.ctx());
                    }
                    ui.label(egui::RichText::new(format!("{}  ·  {}", me.name, me.upn)).weak());
                } else if self.busy == Some(Busy::SigningIn) {
                    if ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                    ui.spinner();
                } else {
                    ui.toggle_value(&mut self.show_connection, "⚙ Connection");
                }
            });
        });
        if self.show_connection && self.me.is_none() {
            ui.add_space(4.0);
            egui::Grid::new("conn")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Tenant");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.tenant)
                            .hint_text("organizations, contoso.onmicrosoft.com or tenant GUID")
                            .desired_width(360.0),
                    );
                    ui.end_row();
                    ui.label("Client ID");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.settings.client_id)
                                .desired_width(360.0),
                        );
                        if ui.small_button("Reset").clicked() {
                            self.settings.client_id = auth::DEFAULT_CLIENT_ID.into();
                            self.settings.tenant = auth::DEFAULT_TENANT.into();
                        }
                    });
                    ui.end_row();
                });
            ui.label(
                egui::RichText::new(
                    "Default client is Microsoft's public Graph Command Line Tools client. Your organization controls consent. \
                     Set a tenant to sign in to a specific client tenant, or enter your own app registration's client ID.",
                )
                .small()
                .weak(),
            );
        }
        ui.add_space(6.0);
            });
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(6.0);
        let busy = self.busy.is_some();

        ui.horizontal(|ui| {
            ui.label("Save to");
            let w = (ui.available_width() - 190.0).max(120.0);
            ui.add_enabled(
                !busy,
                egui::TextEdit::singleline(&mut self.settings.output_dir).desired_width(w),
            );
            if ui
                .add_enabled(!busy, egui::Button::new("Browse…"))
                .clicked()
            {
                let mut d = rfd::FileDialog::new().set_title("Choose export folder");
                if std::path::Path::new(&self.settings.output_dir).is_dir() {
                    d = d.set_directory(&self.settings.output_dir);
                }
                if let Some(p) = d.pick_folder() {
                    self.settings.output_dir = p.to_string_lossy().into_owned();
                }
            }
            if ui.button("Open").clicked() {
                let _ = std::fs::create_dir_all(&self.settings.output_dir);
                let _ = open::that(&self.settings.output_dir);
            }
        });

        ui.horizontal(|ui| {
            ui.add_enabled(
                !busy,
                egui::Checkbox::new(&mut self.settings.include_images, "Download images"),
            );
            ui.add_enabled(
                !busy,
                egui::Checkbox::new(
                    &mut self.settings.include_system,
                    "Include system events (calls, members added, renames)",
                ),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.busy == Some(Busy::Exporting) {
                    if ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = "Cancelling...".into();
                    }
                } else {
                    let n = self.selected.len();
                    let text = egui::RichText::new(format!(
                        "Export {n} chat{}",
                        if n == 1 { "" } else { "s" }
                    ))
                    .strong()
                    .color(egui::Color32::WHITE);
                    let enabled = !busy && n > 0 && self.graph.is_some();
                    let btn = egui::Button::new(text)
                        .fill(if enabled { ACCENT } else { egui::Color32::GRAY })
                        .min_size(egui::vec2(140.0, 30.0));
                    if ui.add_enabled(enabled, btn).clicked() {
                        self.start_export(&ctx);
                    }
                }
            });
        });

        if let Some((i, total, label, detail)) = &self.progress {
            let frac = (*i as f32) / (*total).max(1) as f32;
            ui.add(
                egui::ProgressBar::new(frac)
                    .text(format!("Chat {}/{total}: {label} - {detail}", i + 1))
                    .animate(true),
            );
        }

        ui.horizontal(|ui| {
            if matches!(self.busy, Some(Busy::LoadingChats) | Some(Busy::SigningIn)) {
                ui.spinner();
            }
            ui.label(egui::RichText::new(&self.status).weak());
        });

        egui::CollapsingHeader::new("Local chat cache & privacy")
            .default_open(false)
            .show(ui, |ui| {
                let changed = ui
                    .checkbox(
                        &mut self.settings.cache_chat_list,
                        "Cache chat list on this device for faster loading",
                    )
                    .changed();
                if changed && !self.settings.cache_chat_list {
                    self.chat_cache = None;
                }
                ui.label(
                    egui::RichText::new(
                        "Stores chat IDs, names, email addresses, and last activity in local app settings. Message content, files, and sign-in tokens are never cached.",
                    )
                    .small()
                    .weak(),
                );
                ui.horizontal(|ui| {
                    let can_clear = self.chat_cache.is_some();
                    if ui
                        .add_enabled(can_clear, egui::Button::new("Clear saved chat list"))
                        .clicked()
                    {
                        self.chat_cache = None;
                        self.status = "Saved chat list cleared.".into();
                    }
                    if let Some(cache) = &self.chat_cache {
                        ui.label(
                            egui::RichText::new(format!("{} chats saved", cache.chats.len()))
                                .weak(),
                        );
                    }
                });
            });

        egui::CollapsingHeader::new("Activity log")
            .default_open(false)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for l in &self.log {
                            ui.label(egui::RichText::new(l).monospace().small());
                        }
                    });
            });
        ui.horizontal(|ui| {
            let mut version_parts = env!("CARGO_PKG_VERSION").split('.');
            let version_label = match (
                version_parts.next(),
                version_parts.next(),
                version_parts.next(),
            ) {
                (Some(year), Some(month), Some(day)) => {
                    format!("{}{:0>2}{:0>2}", year, month, day)
                }
                _ => env!("CARGO_PKG_VERSION").to_string(),
            };
            ui.label(
                egui::RichText::new(format!(
                    "© 2026 Sunish Issac · Version {version_label} · Vibe coded with Codex"
                ))
                .small()
                .weak(),
            );
            ui.hyperlink_to("sunish.net", "https://sunish.net");
        });
        ui.add_space(4.0);
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.25).max(20.0));
            ui.label(
                egui::RichText::new("Export your Microsoft Teams chats")
                    .size(22.0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label("1:1, group and meeting chats → HTML transcript, CSV and JSON, with images.");
            ui.label(
                egui::RichText::new(
                    "Everything stays on this computer. Only your own chats are visible.",
                )
                .weak(),
            );
            ui.add_space(20.0);
            if self.busy == Some(Busy::SigningIn) {
                ui.spinner();
                ui.label("Finish signing in in your browser...");
            } else {
                let btn = egui::Button::new(
                    egui::RichText::new("Sign in with Microsoft")
                        .size(16.0)
                        .strong()
                        .color(egui::Color32::WHITE),
                )
                .fill(ACCENT)
                .min_size(egui::vec2(240.0, 40.0));
                if ui.add(btn).clicked() {
                    self.sign_in(&ctx);
                }
            }
        });
    }

    fn chat_list(&mut self, ui: &mut egui::Ui) {
        let visible = self.visible();

        egui::Frame::group(ui.style())
            .fill(egui::Color32::WHITE)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.search)
                            .hint_text("🔍 Search by chat name, person or email")
                            .desired_width(320.0),
                    );
                    if !self.search.is_empty() && ui.small_button("✕").clicked() {
                        self.search.clear();
                        self.email_autoselect_query.clear();
                    }
                    ui.separator();
                    ui.toggle_value(&mut self.show_one, "1:1");
                    ui.toggle_value(&mut self.show_self, "Self");
                    ui.toggle_value(&mut self.show_group, "Group");
                    ui.toggle_value(&mut self.show_meeting, "Meeting");
                    ui.separator();
                    if ui.button("Select shown").clicked() {
                        for &i in &visible {
                            self.selected.insert(self.chats[i].id.clone());
                        }
                    }
                    if ui.button("Clear").clicked() {
                        self.selected.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} shown · {} selected",
                                visible.len(),
                                self.selected.len()
                            ))
                            .weak(),
                        );
                    });
                })
            });
        self.select_email_matches();
        if self.search.trim().contains('@') {
            ui.label(
                egui::RichText::new("Exact email matches are selected for export.")
                    .small()
                    .weak(),
            );
        }
        ui.add_space(4.0);

        if self.chats.is_empty() {
            ui.centered_and_justified(|ui| {
                if self.busy == Some(Busy::LoadingChats) {
                    ui.label("Loading your chats...");
                } else {
                    ui.label("No chats found.");
                }
            });
            return;
        }

        let chats = &self.chats;
        let selected = &mut self.selected;
        let row_h = 26.0;
        egui::Frame::group(ui.style())
            .fill(egui::Color32::WHITE)
            .show(ui, |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .sense(egui::Sense::click())
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(28.0))
                    .column(Column::exact(72.0))
                    .column(Column::remainder().at_least(220.0).clip(true))
                    .column(Column::exact(80.0))
                    .column(Column::exact(130.0))
                    .min_scrolled_height(0.0)
                    .header(22.0, |mut h| {
                        h.col(|_| {});
                        h.col(|ui| {
                            ui.strong("Type");
                        });
                        h.col(|ui| {
                            ui.strong("Chat");
                        });
                        h.col(|ui| {
                            ui.strong("Members");
                        });
                        h.col(|ui| {
                            ui.strong("Last activity");
                        });
                    })
                    .body(|body| {
                        body.rows(row_h, visible.len(), |mut row| {
                            let c = &chats[visible[row.index()]];
                            let mut on = selected.contains(&c.id);
                            row.set_selected(on);
                            let mut toggled = false;
                            row.col(|ui| {
                                if ui.checkbox(&mut on, "").changed() {
                                    toggled = true;
                                }
                            });
                            row.col(|ui| {
                                let (txt, col) = match c.kind {
                                    ChatKind::OneOnOne => {
                                        ("1:1", egui::Color32::from_rgb(0x2b, 0x7a, 0x78))
                                    }
                                    ChatKind::SelfChat => {
                                        ("Self", egui::Color32::from_rgb(0x9b, 0x59, 0xb6))
                                    }
                                    ChatKind::Group => {
                                        ("Group", egui::Color32::from_rgb(0x5b, 0x5f, 0xc7))
                                    }
                                    ChatKind::Meeting => {
                                        ("Meeting", egui::Color32::from_rgb(0xb0, 0x6a, 0x00))
                                    }
                                    ChatKind::Other => ("Other", egui::Color32::GRAY),
                                };
                                ui.label(egui::RichText::new(txt).color(col).strong());
                            });
                            row.col(|ui| {
                                let mut t = c.label.clone();
                                if let Some(e) = &c.other_email {
                                    t = format!("{t}  ·  {e}");
                                }
                                ui.label(t);
                            });
                            row.col(|ui| {
                                ui.label(c.members.len().to_string())
                                    .on_hover_text(c.member_names());
                            });
                            row.col(|ui| {
                                ui.label(
                                    c.last_activity
                                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                                        .unwrap_or_default(),
                                );
                            });
                            if row.response().clicked() {
                                on = !on;
                                toggled = true;
                            }
                            if toggled {
                                if on {
                                    selected.insert(c.id.clone());
                                } else {
                                    selected.remove(&c.id);
                                }
                            }
                        });
                    })
            });
    }

    fn results_window(&mut self, ctx: &egui::Context) {
        let Some(results) = &self.results else {
            return;
        };
        let mut keep_open = true;
        let mut close = false;
        let out_dir = self.settings.output_dir.clone();
        egui::Window::new("Export complete")
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut keep_open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        egui::Grid::new("results")
                            .striped(true)
                            .num_columns(5)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.strong("Chat");
                                ui.strong("Type");
                                ui.strong("Messages");
                                ui.strong("Images");
                                ui.strong("");
                                ui.end_row();
                                for r in results {
                                    ui.label(&r.label);
                                    ui.label(r.kind.label());
                                    match &r.error {
                                        None => {
                                            ui.label(r.messages.to_string());
                                            ui.label(r.images.to_string());
                                            ui.horizontal(|ui| {
                                                if let Some(h) = &r.html {
                                                    if ui.small_button("View").clicked() {
                                                        let _ = open::that(h);
                                                    }
                                                }
                                                if let Some(f) = &r.folder {
                                                    if ui.small_button("Folder").clicked() {
                                                        let _ = open::that(f);
                                                    }
                                                }
                                            });
                                        }
                                        Some(e) => {
                                            ui.colored_label(
                                                egui::Color32::from_rgb(0xc4, 0x31, 0x4b),
                                                "Failed",
                                            );
                                            ui.label("");
                                            ui.label(egui::RichText::new(e).small())
                                                .on_hover_text(e);
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Open export folder").clicked() {
                        let _ = open::that(&out_dir);
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
        if !keep_open || close {
            self.results = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        if self.busy.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("bottom").show(ui, |ui| self.bottom_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            if self.me.is_none() {
                self.welcome(ui);
            } else {
                self.chat_list(ui);
            }
        });
        self.results_window(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
        eframe::set_value(storage, "teams-chat-exporter.chat-cache", &self.chat_cache);
    }
}

fn non_empty(s: &str, default: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        default.to_string()
    } else {
        t.to_string()
    }
}

fn style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(10.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 7.0);
        s.visuals.panel_fill = egui::Color32::from_rgb(0xf4, 0xf6, 0xfb);
        s.visuals.window_fill = egui::Color32::WHITE;
        s.visuals.extreme_bg_color = egui::Color32::from_rgb(0xf8, 0xf9, 0xfc);
        s.visuals.faint_bg_color = egui::Color32::from_rgb(0xf0, 0xf2, 0xf8);
        s.visuals.selection.bg_fill = ACCENT_LIGHT;
        s.visuals.selection.stroke.color = ACCENT;
        s.visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(0xf7, 0xf8, 0xfc);
        s.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0xed, 0xef, 0xf6);
        s.visuals.widgets.inactive.fg_stroke.color = egui::Color32::from_rgb(0x32, 0x35, 0x45);
        s.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(0xe3, 0xe8, 0xf8);
        s.visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0xd7, 0xde, 0xf3);
    });
}

fn main() -> eframe::Result {
    let use_gl = std::env::args().any(|a| a == "--opengl")
        || std::env::var("TCE_RENDERER")
            .map(|v| v == "glow")
            .unwrap_or(false);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Teams Chat Exporter")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([720.0, 460.0]),
        renderer: if use_gl {
            eframe::Renderer::Glow
        } else {
            eframe::Renderer::Wgpu
        },
        ..Default::default()
    };
    eframe::run_native(
        "Teams Chat Exporter",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
