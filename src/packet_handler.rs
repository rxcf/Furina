use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{Mutex, OnceLock};

use bson::{Bson, Document};
use hudhook::tracing::{debug, warn};

use crate::minimap::{MinimapData, decode_minimap_from_gwc};

static APP_STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

#[derive(Clone, Default)]
pub struct PlayerInfo {
    pub user_id: Option<String>,
    pub marker_color: [f32; 4],
    pub name: String,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub dir: Option<i32>,
    pub anim: Option<i32>,
    pub xp_level: Option<i32>,
    pub gem_amount: Option<i32>,
    pub in_portal: bool,
    pub status_icon: Option<i32>,
}

#[derive(Default)]
pub struct AppState {
    pub current_world: Option<String>,
    pub self_user_id: Option<String>,
    pub self_user_name: Option<String>,
    pub players: HashMap<String, PlayerInfo>,
    pub minimap: Option<MinimapData>,
}

impl AppState {
    pub fn sorted_players(&self) -> Vec<PlayerInfo> {
        let mut players: Vec<_> = self.players.values().cloned().collect();
        players.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.user_id.cmp(&b.user_id)));
        players
    }
}

pub fn app_state() -> &'static Mutex<AppState> {
    APP_STATE.get_or_init(|| Mutex::new(AppState::default()))
}

pub fn process_packet(bytes: &[u8], is_receive: bool) {
    let Some(doc) = try_parse_outer_document(bytes) else {
        return;
    };

    let mut handled_any = false;
    collect_messages(&doc, &mut |msg| {
        handled_any = true;
        process_message(msg, is_receive);
    });

    if is_receive && !handled_any {
        debug!("decoded BSON packet without message envelope keys");
    }
}

pub fn try_parse_outer_document(bytes: &[u8]) -> Option<Document> {
    if let Ok(doc) = Document::from_reader(Cursor::new(bytes)) {
        return Some(doc);
    }

    if bytes.len() > 4 {
        let total_len = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        if total_len == bytes.len() {
            return Document::from_reader(Cursor::new(&bytes[4..])).ok();
        }
    }

    None
}

fn collect_messages<'a, F>(doc: &'a Document, visitor: &mut F)
where
    F: FnMut(&'a Document),
{
    if doc.contains_key("ID") {
        visitor(doc);
        return;
    }

    let mut handled_any = false;
    let count = get_i32(doc, "mc").unwrap_or(0).max(0);
    for idx in 0..count {
        let key = format!("m{idx}");
        let Some(Bson::Document(msg)) = doc.get(&key) else {
            continue;
        };
        handled_any = true;
        visitor(msg);
    }

    if handled_any {
        return;
    }

    let mut numbered_keys: Vec<_> = doc
        .iter()
        .filter_map(|(key, value)| {
            let suffix = key.strip_prefix('m')?;
            let idx = suffix.parse::<usize>().ok()?;
            let Bson::Document(msg) = value else {
                return None;
            };
            Some((idx, msg))
        })
        .collect();
    numbered_keys.sort_by_key(|(idx, _)| *idx);

    if !numbered_keys.is_empty() {
        for (_, msg) in numbered_keys {
            visitor(msg);
        }
        return;
    }

    for value in doc.values() {
        let Bson::Document(msg) = value else {
            continue;
        };
        if msg.contains_key("ID") {
            visitor(msg);
        }
    }
}

fn process_message(msg: &Document, is_receive: bool) {
    let msg_id = msg.get_str("ID").unwrap_or_default();
    let mut state = app_state().lock().unwrap();

    match msg_id {
        "GPd" if is_receive => {
            state.self_user_id = get_id_string(msg, "U");
            state.self_user_name = msg.get_str("UN").ok().map(str::to_owned);

            if let Some(user_id) = state.self_user_id.clone() {
                let self_name = state
                    .self_user_name
                    .clone()
                    .unwrap_or_else(|| user_id.clone());
                let player = state.players.entry(user_id.clone()).or_default();
                player.user_id = Some(user_id);
                player.name = self_name;
            }
        },
        "TTjW" if is_receive => {
            let join_result = get_i32(msg, "JR");
            let has_error = msg.contains_key("E") || msg.contains_key("ER");
            if join_result.is_some_and(|value| value != 0) || has_error {
                return;
            }

            if let Some(world) = msg.get_str("W").ok().or_else(|| msg.get_str("WN").ok()) {
                state.current_world = Some(world.to_owned());
                state.players.clear();
                state.minimap = None;

                if let Some(self_user_id) = state.self_user_id.clone() {
                    let self_name = state
                        .self_user_name
                        .clone()
                        .unwrap_or_else(|| self_user_id.clone());
                    let player = state.players.entry(self_user_id.clone()).or_default();
                    player.user_id = Some(self_user_id);
                    player.name = self_name;
                }
            }
        },
        "LW" if is_receive => {
            state.current_world = None;
            state.players.clear();
            state.minimap = None;
        },
        "GWC" if is_receive => {
            if let Some(Bson::Binary(binary)) = msg.get("W") {
                match decode_minimap_from_gwc(&binary.bytes) {
                    Ok(minimap) => state.minimap = Some(minimap),
                    Err(e) => warn!("failed to decode GWC minimap: {e}"),
                }
            }
        },
        "AnP" | "U" | "mP" | "PSicU" | "PPA" if is_receive => {
            if let Some(user_id) = get_id_string(msg, "U") {
                upsert_player(&mut state, msg, user_id);
            }
        },
        "PL" if is_receive => {
            if let Some(user_id) = get_id_string(msg, "U") {
                state.players.remove(&user_id);
            }
        },
        _ => {},
    }
}

fn upsert_player(state: &mut AppState, msg: &Document, user_id: String) {
    let player = state.players.entry(user_id.clone()).or_default();
    player.user_id = Some(user_id.clone());

    if let Ok(u_id_str) = msg.get_str("U") {
        player.marker_color = generate_color_from_id(u_id_str);
    }
    if let Ok(name) = msg.get_str("UN") {
        player.name = name.to_owned();
    } else if Some(&user_id) == state.self_user_id.as_ref() {
        if let Some(self_name) = &state.self_user_name {
            player.name = self_name.clone();
        }
    } else if player.name.is_empty() {
        player.name = user_id;
    }

    if let Some(x) = get_f32(msg, "x") {
        player.x = Some(x);
    }
    if let Some(y) = get_f32(msg, "y") {
        player.y = Some(y);
    }
    if let Some(dir) = get_i32(msg, "d") {
        player.dir = Some(dir);
    }
    if let Some(anim) = get_i32(msg, "a") {
        player.anim = Some(anim);
    }
    if let Some(xp_level) = get_i32(msg, "xpLvL") {
        player.xp_level = Some(xp_level);
    }
    if let Some(gem_amount) = get_i32(msg, "GAmt") {
        player.gem_amount = Some(gem_amount);
    }
    if let Some(in_portal) = get_bool(msg, "inPortal") {
        player.in_portal = in_portal;
    }
    if let Some(status_icon) = get_i32(msg, "SIc") {
        player.status_icon = Some(status_icon);
    }
}

fn generate_color_from_id(id: &str) -> [f32; 4] {
    let r_part = u8::from_str_radix(&id[id.len()-2..], 16).unwrap_or(255);
    let g_part = u8::from_str_radix(&id[id.len()-4..id.len()-2], 16).unwrap_or(255);
    let b_part = u8::from_str_radix(&id[id.len()-6..id.len()-4], 16).unwrap_or(255);

    [
        (r_part as f32 / 255.0).max(0.4),
        (g_part as f32 / 255.0).max(0.4),
        (b_part as f32 / 255.0).max(0.4),
        1.0
    ]
}

fn get_id_string(doc: &Document, key: &str) -> Option<String> {
    match doc.get(key) {
        Some(Bson::String(v)) => Some(v.clone()),
        Some(Bson::Int32(v)) => Some(v.to_string()),
        Some(Bson::Int64(v)) => Some(v.to_string()),
        Some(Bson::ObjectId(v)) => Some(v.to_hex()),
        _ => None,
    }
}

// Not sure if we need this in the future again, i'll just leave it here for now
/*fn get_i64(doc: &Document, key: &str) -> Option<i64> {
    match doc.get(key) {
        Some(Bson::Int32(v)) => Some(*v as i64),
        Some(Bson::Int64(v)) => Some(*v),
        Some(Bson::Double(v)) => Some(*v as i64),
        _ => None,
    }
}*/

fn get_i32(doc: &Document, key: &str) -> Option<i32> {
    match doc.get(key) {
        Some(Bson::Int32(v)) => Some(*v),
        Some(Bson::Int64(v)) => Some(*v as i32),
        Some(Bson::Double(v)) => Some(*v as i32),
        Some(Bson::String(v)) => v.parse().ok(),
        _ => None,
    }
}

fn get_f32(doc: &Document, key: &str) -> Option<f32> {
    match doc.get(key) {
        Some(Bson::Int32(v)) => Some(*v as f32),
        Some(Bson::Int64(v)) => Some(*v as f32),
        Some(Bson::Double(v)) => Some(*v as f32),
        Some(Bson::String(v)) => v.parse().ok(),
        _ => None,
    }
}

fn get_bool(doc: &Document, key: &str) -> Option<bool> {
    match doc.get(key) {
        Some(Bson::Boolean(v)) => Some(*v),
        Some(Bson::Int32(v)) => Some(*v != 0),
        Some(Bson::Int64(v)) => Some(*v != 0),
        _ => None,
    }
}
