use std::collections::HashMap;
use std::io::Cursor;

use bson::{Bson, Document};
use hudhook::imgui;
use hudhook::imgui::{DrawListMut, ImColor32};

use crate::packet_handler::PlayerInfo;

const MINIMAP_MAX_SIZE: [f32; 2] = [320.0, 220.0];

#[derive(Default)]
pub struct MinimapData {
    pub width: usize,
    pub height: usize,
    pub colors: Vec<[u8; 3]>,
}

pub fn render_minimap(
    ui: &imgui::Ui,
    players: &HashMap<i64, PlayerInfo>,
    self_user_id: Option<i64>,
    minimap: Option<&MinimapData>,
) {
    ui.text("Minimap");

    let Some(minimap) = minimap else {
        ui.text("Waiting for world data...");
        return;
    };

    if minimap.width == 0 || minimap.height == 0 {
        ui.text("World data not ready.");
        return;
    }

    let scale_x = MINIMAP_MAX_SIZE[0] / minimap.width as f32;
    let scale_y = MINIMAP_MAX_SIZE[1] / minimap.height as f32;
    let tile_scale = scale_x.min(scale_y).max(1.0);
    let size = [minimap.width as f32 * tile_scale, minimap.height as f32 * tile_scale];

    let screen_pos = ui.cursor_screen_pos();
    let draw_list = ui.get_window_draw_list();
    ui.invisible_button("##minimap", size);

    draw_minimap_tiles(&draw_list, screen_pos, tile_scale, minimap);
    draw_player_markers(&draw_list, screen_pos, tile_scale, minimap, players, self_user_id);

    ui.text(format!("{} x {}", minimap.width, minimap.height));
}

fn draw_minimap_tiles(
    draw_list: &DrawListMut<'_>,
    origin: [f32; 2],
    tile_scale: f32,
    minimap: &MinimapData,
) {
    for y in 0..minimap.height {
        for x in 0..minimap.width {
            let color = minimap.colors[y * minimap.width + x];
            let min = [
                origin[0] + x as f32 * tile_scale,
                origin[1] + y as f32 * tile_scale,
            ];
            let max = [min[0] + tile_scale, min[1] + tile_scale];
            draw_list.add_rect(min, max, rgb(color)).filled(true).build();
        }
    }
}

fn draw_player_markers(
    draw_list: &DrawListMut<'_>,
    origin: [f32; 2],
    tile_scale: f32,
    minimap: &MinimapData,
    players: &HashMap<i64, PlayerInfo>,
    self_user_id: Option<i64>,
) {
    for player in players.values() {
        let (Some(x), Some(y)) = (player.x, player.y) else {
            continue;
        };

        if x.is_sign_negative() || y.is_sign_negative() {
            continue;
        }

        let px = x.floor() as usize;
        let py = y.floor() as usize;
        if px >= minimap.width || py >= minimap.height {
            continue;
        }

        let center = [
            origin[0] + (px as f32 + 0.5) * tile_scale,
            origin[1] + (minimap.height as f32 - py as f32 - 0.5) * tile_scale,
        ];
        let radius = (tile_scale * 0.5).max(2.0);
        let color = if player.user_id == self_user_id {
            ImColor32::from_rgba(255, 64, 64, 255)
        } else {
            ImColor32::from_rgba(255, 255, 0, 255)
        };

        draw_list.add_circle(center, radius, color).filled(true).build();
    }
}

fn rgb(color: [u8; 3]) -> ImColor32 {
    ImColor32::from_rgba(color[0], color[1], color[2], 255)
}

pub fn decode_minimap_from_gwc(compressed: &[u8]) -> Result<MinimapData, String> {
    let decompressed = zstd::decode_all(Cursor::new(compressed))
        .map_err(|e| format!("zstd decode failed: {e}"))?;
    let world = Document::from_reader(Cursor::new(decompressed))
        .map_err(|e| format!("bson decode failed: {e}"))?;

    let settings = world
        .get_document("WorldSizeSettingsType")
        .map_err(|e| format!("missing world size: {e}"))?;
    let width = settings
        .get_i32("WorldSizeX")
        .map_err(|e| format!("missing width: {e}"))? as usize;
    let height = settings
        .get_i32("WorldSizeY")
        .map_err(|e| format!("missing height: {e}"))? as usize;

    let block_layer = match world.get("BlockLayer") {
        Some(Bson::Binary(binary)) => binary.bytes.as_slice(),
        _ => return Err("missing block layer".to_owned()),
    };

    let mut colors = vec![[135, 206, 235]; width * height];
    for y in 0..height {
        for x in 0..width {
            let idx = (y * width + x) * 2;
            if idx + 1 >= block_layer.len() {
                continue;
            }

            let block_id = u16::from_le_bytes([block_layer[idx], block_layer[idx + 1]]);
            let draw_y = height - 1 - y;
            colors[draw_y * width + x] = generate_block_color(block_id);
        }
    }

    Ok(MinimapData { width, height, colors })
}

fn generate_block_color(block_id: u16) -> [u8; 3] {
    if block_id == 0 {
        return [135, 206, 235];
    }

    let mut r = ((block_id as u32 * 85) % 256) as u8;
    let mut g = ((block_id as u32 * 153) % 256) as u8;
    let mut b = ((block_id as u32 * 211) % 256) as u8;

    let distance = (r as i32 - 135).abs() + (g as i32 - 206).abs() + (b as i32 - 235).abs();
    if distance < 50 {
        r = r.wrapping_add(128);
        g = g.wrapping_add(64);
        b = b.wrapping_add(32);
    }

    [r, g, b]
}
