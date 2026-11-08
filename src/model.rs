//! Chat list: who am I, and what chats do I have.

use crate::graph::{GResult, Graph};
use crate::util::{jarr, jget, jstr, parse_time};
use chrono::{DateTime, Local};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Me {
    pub id: String,
    pub name: String,
    pub upn: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatKind {
    OneOnOne,
    SelfChat,
    Group,
    Meeting,
    Other,
}

impl ChatKind {
    pub fn from_graph(s: &str) -> Self {
        match s {
            "oneOnOne" => Self::OneOnOne,
            "selfChat" => Self::SelfChat,
            "group" => Self::Group,
            "meeting" => Self::Meeting,
            _ => Self::Other,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::OneOnOne => "1:1",
            Self::SelfChat => "Self chat",
            Self::Group => "Group",
            Self::Meeting => "Meeting",
            Self::Other => "Other",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Member {
    pub user_id: String,
    pub name: String,
    pub email: String,
}

impl Member {
    pub fn display(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else if !self.email.is_empty() {
            &self.email
        } else {
            "Unknown"
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChatInfo {
    pub id: String,
    pub kind: ChatKind,
    pub label: String,
    pub members: Vec<Member>,
    pub last_activity: Option<DateTime<Local>>,
    /// For 1:1 chats: the other person's email (used as file name).
    pub other_email: Option<String>,
    /// Lower-cased text used by the search box.
    pub search_text: String,
}

impl ChatInfo {
    pub fn from_graph(v: &Value, me_id: &str) -> Self {
        let id = jstr(v, &["id"]).unwrap_or_default().to_string();
        let graph_kind = ChatKind::from_graph(jstr(v, &["chatType"]).unwrap_or(""));
        let topic = jstr(v, &["topic"]).map(String::from);
        let members: Vec<Member> = jarr(v, &["members"])
            .iter()
            .map(|m| Member {
                user_id: jstr(m, &["userId"]).unwrap_or_default().to_string(),
                name: jstr(m, &["displayName"]).unwrap_or_default().to_string(),
                email: jstr(m, &["email"]).unwrap_or_default().to_string(),
            })
            .collect();
        let is_self_chat = graph_kind == ChatKind::OneOnOne
            && !members.is_empty()
            && members.iter().all(|member| member.user_id == me_id);
        let kind = if is_self_chat {
            ChatKind::SelfChat
        } else {
            graph_kind
        };
        let others: Vec<&Member> = members.iter().filter(|m| m.user_id != me_id).collect();

        let label = if let Some(t) = &topic {
            t.clone()
        } else if is_self_chat {
            "Notes to self".to_string()
        } else if others.is_empty() {
            "Chat".to_string()
        } else {
            let names: Vec<&str> = others.iter().map(|m| m.display()).collect();
            if names.len() > 4 {
                format!("{} +{}", names[..4].join(", "), names.len() - 4)
            } else {
                names.join(", ")
            }
        };

        let other_email = if kind == ChatKind::OneOnOne {
            others
                .first()
                .map(|m| m.email.to_lowercase())
                .filter(|e| !e.is_empty())
        } else {
            None
        };

        let last_activity = jstr(v, &["lastMessagePreview", "createdDateTime"])
            .or_else(|| jstr(v, &["lastUpdatedDateTime"]))
            .or_else(|| jstr(v, &["createdDateTime"]))
            .and_then(parse_time);

        let mut search_text = label.to_lowercase();
        for m in &members {
            search_text.push(' ');
            search_text.push_str(&m.name.to_lowercase());
            search_text.push(' ');
            search_text.push_str(&m.email.to_lowercase());
        }

        Self {
            id,
            kind,
            label,
            members,
            last_activity,
            other_email,
            search_text,
        }
    }

    pub fn member_names(&self) -> String {
        self.members
            .iter()
            .map(|m| {
                if m.email.is_empty() {
                    m.display().to_string()
                } else {
                    format!("{} <{}>", m.display(), m.email)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub fn get_me(g: &Graph) -> GResult<Me> {
    let v = g.get_json(&g.url("/me?$select=id,displayName,userPrincipalName,mail"))?;
    Ok(Me {
        id: jstr(&v, &["id"]).unwrap_or_default().to_string(),
        name: jstr(&v, &["displayName"]).unwrap_or("Me").to_string(),
        upn: jstr(&v, &["userPrincipalName"])
            .or_else(|| jstr(&v, &["mail"]))
            .unwrap_or_default()
            .to_string(),
    })
}

/// All chats (1:1, group, meeting) with members, newest activity first.
#[cfg(test)]
pub fn list_chats(
    g: &Graph,
    me: &Me,
    mut progress: impl FnMut(usize) -> bool,
) -> GResult<Vec<ChatInfo>> {
    let mut chats = Vec::new();
    list_chats_stream(g, me, |page, count| {
        chats.extend(page);
        progress(count)
    })?;
    chats.sort_by_key(|c| std::cmp::Reverse(c.last_activity));
    Ok(chats)
}

/// Load chats page by page, publishing each converted batch to the caller.
pub fn list_chats_stream(
    g: &Graph,
    me: &Me,
    mut on_page: impl FnMut(Vec<ChatInfo>, usize) -> bool,
) -> GResult<()> {
    let url = g.url("/me/chats?$expand=members,lastMessagePreview&$top=50");
    match g.get_paged_with_items(&url, |items, count| {
        let batch = items
            .iter()
            .filter(|v| jget(v, &["id"]).is_some())
            .map(|v| ChatInfo::from_graph(v, &me.id))
            .collect();
        on_page(batch, count)
    }) {
        Ok(_) => {}
        Err(e) if e.message == "Cancelled" => return Err(e),
        // Some tenants reject the double expand - fall back to members only.
        Err(_) => {
            let fallback = g.url("/me/chats?$expand=members&$top=50");
            g.get_paged_with_items(&fallback, |items, count| {
                let batch = items
                    .iter()
                    .filter(|v| jget(v, &["id"]).is_some())
                    .map(|v| ChatInfo::from_graph(v, &me.id))
                    .collect();
                on_page(batch, count)
            })?;
        }
    }
    Ok(())
}
