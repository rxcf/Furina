use std::collections::HashMap;
use std::io::Cursor;

use bson::{Bson, Document};
use hudhook::imgui;
use hudhook::imgui::{DrawListMut, ImColor32};

use crate::packet_handler::PlayerInfo;

#[derive(Default)]
pub struct MinimapData {
    pub width: usize,
    pub height: usize,
    pub colors: Vec<[u8; 3]>,
}

pub fn render_minimap(
    ui: &imgui::Ui,
    players: &HashMap<String, PlayerInfo>,
    self_user_id: Option<String>,
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

    let world_dimensions = format!("{} x {}", minimap.width, minimap.height);
    let style = ui.clone_style();
    let text_size = ui.calc_text_size(&world_dimensions);
    let text_height = text_size[1] + style.item_spacing[1];

    let avail = ui.content_region_avail();
    
    let safe_width = avail[0].max(1.0);
    let safe_height = (avail[1] - text_height).max(1.0);

    let scale_x = safe_width / minimap.width as f32;
    let scale_y = safe_height / minimap.height as f32;
    
    let tile_scale = scale_x.min(scale_y);
    
    let size = [
        minimap.width as f32 * tile_scale, 
        minimap.height as f32 * tile_scale
    ];

    let screen_pos = ui.cursor_screen_pos();
    let draw_list = ui.get_window_draw_list();
    
    ui.invisible_button("##minimap_area", size);

    draw_minimap_tiles(&draw_list, screen_pos, tile_scale, minimap);
    draw_player_markers(&draw_list, screen_pos, tile_scale, minimap, players, self_user_id);

    ui.text(world_dimensions);
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
    players: &HashMap<String, PlayerInfo>,
    self_user_id: Option<String>,
) {
    for player in players.values() {
        let (Some(x), Some(y)) = (player.x, player.y) else { continue };

        let map_w = minimap.width as f32;
        let map_h = minimap.height as f32;

        const GAME_SCALE_X: f32 = 3.07;
        const GAME_SCALE_Y: f32 = 3.15;
        const OFFSET_X: f32 = 0.3;
        const OFFSET_Y: f32 = 0.2;

        let game_range_x = map_w / GAME_SCALE_X;
        let game_range_y = map_h / GAME_SCALE_Y;

        let map_x = ((x + OFFSET_X) / game_range_x) * map_w;
        let map_y = ((y + OFFSET_Y) / game_range_y) * map_h;

        let center = [
            origin[0] + (map_x * tile_scale),
            origin[1] + ((map_h - map_y) * tile_scale),
        ];

        let radius = (tile_scale * 0.8).max(2.5);
        let color = if player.user_id == self_user_id {
            ImColor32::from_rgba(255, 255, 255, 255)
        } else {
            ImColor32::from_rgba(
                (player.marker_color[0] * 255.0) as u8,
                (player.marker_color[1] * 255.0) as u8,
                (player.marker_color[2] * 255.0) as u8,
                (player.marker_color[3] * 255.0) as u8,
            )
        };

        draw_list.add_circle(center, radius, color).filled(true).build();
        draw_list.add_circle(center, radius, ImColor32::from_rgba(0, 0, 0, 255)).thickness(1.5).build();
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
    if block_id == 1 || block_id == 2735 { //SoilBlock, GemSoil
        return [182, 106, 37];
    }
    if block_id == 3 || block_id == 343 || block_id == 344 { //Bedrock, LavaRock, EndLavaRock
        return [86, 76, 66];
    }
    if block_id == 4 { //Granite
        return [69, 69, 73];
    }
    if block_id == 5 || block_id == 6 { //Sand, SandStone
        return [210, 155, 94];
    }
    if block_id == 7 { //Lava
        return [220, 87, 22];
    }
    if block_id == 8 { //Marble
        return [176, 171, 168];
    }
    if block_id == 9 { //Obsidian
        return [67, 41, 58];
    }
    if block_id == 10 || block_id == 1513 || block_id == 4540 { //Grass, GrassTall, GrassExtraTall
        return [47, 117, 7];
    }
    if block_id == 15 { //MetalPlate
        return [160, 175, 202];
    }
    if block_id == 16 { //WoodenPlatform
        return [190, 103, 55];
    }
    if block_id == 19 { //GreyBrick
        return [148, 133, 117];
    }
    if block_id == 20 { //RedBrick
        return [187, 49, 27];
    }
    if block_id == 21 { //YellowBrick
        return [214, 174, 10];
    }
    if block_id == 22 { //WhiteBrick
        return [211, 211, 209];
    }
    if block_id == 23 { //BlackBrick
        return [46, 46, 43];
    }
    if block_id == 24 { //WoodWall
        return [172, 91, 55];
    }
    if block_id == 30 || block_id == 88 { //WoodenTable, WoodenChair
        return [114, 55, 21];
    }
    if block_id == 33 { //GreenJello
        return [0, 217, 0];
    }
    if block_id == 34 { //YellowJello
        return [228, 213, 0];
    }
    if block_id == 35 { //BlueJello
        return [0, 105, 212];
    }
    if block_id == 36 { //RedJello
        return [217, 43, 0];
    }
    if block_id == 37 { //Tree
        return [135, 72, 16];
    }
    if block_id == 75 { //Mushroom
        return [175, 137, 75];
    }
    if block_id == 91 { //Lantern
        return [255, 255, 240];
    }
    if block_id == 95 { //ClearJello
        return [216, 229, 239];
    }
    if block_id == 96 { //Fireplace
        return [162, 136, 84];
    }
    if block_id == 110 { //EntrancePortal
        return [146, 170, 164];
    }
    if block_id == 319 { //Bat
        return [31, 31, 31];
    }
    if block_id == 410 { //LockSmall
        return [39, 159, 250];
    }
    if block_id == 411 { //LockMedium
        return [255, 170, 34];
    }
    if block_id == 412 { //LockLarge
        return [243, 72, 46];
    }
    if block_id == 413 { //LockWorld
        return [255, 216, 0];
    }
    if block_id == 568 { //PinkJello
        return [239, 55, 231];
    }
    if block_id == 569 { //LightBlueJello
        return [0, 190, 223];
    }
    if block_id == 1159 { //GlowBlockBlue
        return [66, 229, 252];
    }
    if block_id == 1160 { //GlowBlockGreen
        return [57, 250, 134];
    }
    if block_id == 1161 { //GlowBlockOrange
        return [254, 166, 74];
    }
    if block_id == 1162 { //GlowBlockRed
        return [254, 109, 71];
    }
    if block_id == 1419 { //NetherExit
        return [240, 105, 255];
    }
    if block_id == 1421 { //NetherCrystal
        return [192, 66, 225];
    }
    if block_id == 1510 || block_id == 1511 || block_id == 1512 { //Bush1, Bush2, Bush3
        return [27, 100, 2];
    }
    if block_id == 1514 { //HangingLeaves
        return [89, 121, 35];
    }
    if block_id == 1515 || block_id == 1516 { //Rocks1, Rocks2
        return [96, 96, 83];
    }
    if block_id == 1517 { //TreeStump
        return [89, 43, 2];
    }
    if block_id == 1518 { //VegetationBlock1
        return [53, 121, 10];
    }
    if block_id == 1519 { //TreeTrunk1
        return [98, 40, 10];
    }
    if block_id == 1538 || block_id == 1542 { //ScifiBlock1, ScifiBlock5
        return [206, 206, 206];
    }
    if block_id == 1539 { //ScifiBlock2
        return [22, 22, 22];
    }
    if block_id == 1540 { //ScifiBlock3
        return [170, 170, 170];
    }
    if block_id == 1541 { //ScifiBlock4
        return [78, 83, 97];
    }
    if block_id == 1669 { //BeigeBrick
        return [189, 127, 76];
    }
    if block_id == 1670 { //BlueBrick
        return [112, 144, 152];
    }
    if block_id == 1767 { //GreenBrick
        return [27, 196, 24];
    }
    if block_id == 2298 { //OrangeJello
        return [255, 113, 0];
    }
    if block_id == 2299 { //BlackJello
        return [64, 64, 64];
    }
    if block_id == 2680 { //PinkBrick
        return [237, 108, 236];
    }
    if block_id == 4761 { //TinyChest
        return [124, 124, 124];
    }
    if block_id == 5035 || block_id == 5036 || block_id == 5037 || block_id == 5038 || block_id == 5039 || block_id == 5040 || block_id == 5041 || block_id == 5042 { //NetherGreystone1and2, NetherGreystone3and4, NetherLavastone1and2, NetherLavastone3and4, NetherRedstone1and2, NetherRedstone3and4, NetherRedstoneGlow1and2, NetherRedstoneGlow3and4
        return [77, 59, 57];
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
