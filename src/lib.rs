use std::ffi::c_void;
use std::fs::File;
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use hudhook::hooks::dx12::ImguiDx12Hooks;
use hudhook::tracing::{error, info, warn};
use hudhook::{Hudhook, ImguiRenderLoop, imgui};
use imgui::Condition;
use retour::static_detour;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};
use windows::Win32::{
    Foundation::HINSTANCE,
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemServices::DLL_PROCESS_ATTACH,
    },
};
use windows::core::w;

mod offsets;
mod minimap;
mod packet_handler;

use minimap::render_minimap;
use offsets::{
    CHAT_UI_SUBMIT_RVA, CHAT_UI_UPDATE_RVA, IL2CPP_STRING_NEW_LEN_RVA, RECEIVE_RVA, SEND_RVA,
    SUBMIT_WORLD_CHAT_RVA,
};
use packet_handler::{AppState, app_state, process_packet};

static FIRST_RENDER: AtomicBool = AtomicBool::new(false);
static PACKET_LOGGING_ENABLED: AtomicBool = AtomicBool::new(false);
static PACKET_HOOKS_READY: AtomicBool = AtomicBool::new(false);
static CHAT_HOOK_READY: AtomicBool = AtomicBool::new(false);
const MAX_PACKET_LEN: i32 = 50_000;

static CHAT_FNS: OnceLock<ChatFns> = OnceLock::new();
static PENDING_WORLD_CHAT: OnceLock<Mutex<Option<String>>> = OnceLock::new();

struct ChatFns {
    chat_ui_submit: unsafe extern "system" fn(*mut c_void, *mut c_void, *const c_void) -> bool,
    submit_world_chat: unsafe extern "system" fn(*mut c_void, *const c_void),
    il2cpp_string_new_len: unsafe extern "system" fn(*const u8, i32) -> *mut c_void,
}

#[repr(C)]
struct Il2CppByteArray {
    _object_header: [usize; 2],
    _bounds: usize,
    len: i32,
    _padding: i32,
    data: [u8; 0],
}

static_detour! {
    static SendDetour: unsafe extern "system" fn(*const c_void) -> *mut c_void;
}

static_detour! {
    static ReceiveDetour: unsafe extern "system" fn(*mut c_void, *const c_void) -> *mut c_void;
}

static_detour! {
    static SubmitWorldChatDetour: unsafe extern "system" fn(*mut c_void, *const c_void);
}

static_detour! {
    static ChatUiUpdateDetour: unsafe extern "system" fn(*mut c_void, *const c_void);
}

#[derive(Default)]
pub struct RenderLoop {
    chat_input: String,
    chat_status: String,
}

impl ImguiRenderLoop for RenderLoop {
    fn render(&mut self, ui: &mut imgui::Ui) {
        if !FIRST_RENDER.swap(true, Ordering::SeqCst) {
            info!("imgui render loop reached");
        }

        let display_size = ui.io().display_size;
        let mut packet_logging = PACKET_LOGGING_ENABLED.load(Ordering::Relaxed);
        let state = app_state().lock().unwrap();

        ui.window("Furina")
            .position([20.0, 20.0], Condition::FirstUseEver)
            .size([560.0, 520.0], Condition::FirstUseEver)
            .build(|| {
                if let Some(_tabs) = ui.tab_bar("##furina-tabs") {
                    if let Some(_tab) = ui.tab_item("Basic") {
                        ui.text("Hook loaded.");
                        ui.separator();

                        if ui.checkbox("Preview send/receive packet logs", &mut packet_logging) {
                            PACKET_LOGGING_ENABLED.store(packet_logging, Ordering::SeqCst);
                            info!(
                                "packet preview logging {}",
                                if packet_logging { "enabled" } else { "disabled" }
                            );
                        }

                        ui.text(if PACKET_HOOKS_READY.load(Ordering::Relaxed) {
                            "Packet hooks: ready"
                        } else {
                            "Packet hooks: unavailable"
                        });
                        ui.text(format!(
                            "World: {}",
                            state.current_world.as_deref().unwrap_or("(none)")
                        ));
                        ui.text(format!(
                            "Self: {}",
                            state.self_user_name.as_deref().unwrap_or("(unknown)")
                        ));
                        ui.text(if CHAT_HOOK_READY.load(Ordering::Relaxed) {
                            "World chat: ready"
                        } else {
                            "World chat: unavailable"
                        });
                        ui.text(format!(
                            "Display size: {:.0} x {:.0}",
                            display_size[0], display_size[1]
                        ));
                    }

                    if let Some(_tab) = ui.tab_item("World") {
                        if state.current_world.is_some() {
                            let avail = ui.content_region_avail();
                            let spacing = ui.clone_style().item_spacing[0];
                            let right_width = 190.0_f32.min((avail[0] * 0.38).max(150.0));
                            let left_width = (avail[0] - right_width - spacing).max(120.0);
                            let pane_height = (avail[1] - 8.0).max(120.0);

                            ui.child_window("##world-map")
                                .size([left_width, pane_height])
                                .border(true)
                                .build(|| {
                                    render_minimap(
                                        ui,
                                        &state.players,
                                        state.self_user_id,
                                        state.minimap.as_ref(),
                                    );
                                });

                            ui.same_line();

                            ui.child_window("##world-players")
                                .size([right_width, pane_height])
                                .border(true)
                                .build(|| {
                                    render_chat_controls(ui, self);
                                    ui.separator();
                                    render_player_list(ui, &state);
                                });
                        } else {
                            ui.text("Minimap and player list appear after world packets arrive.");
                        }
                    }
                }
            });
    }
}

fn render_player_list(ui: &imgui::Ui, state: &AppState) {
    ui.text(format!("Players ({})", state.players.len()));

    for player in state.sorted_players() {
        let pos = match (player.x, player.y) {
            (Some(x), Some(y)) => format!(" ({x:.1}, {y:.1})"),
            _ => String::new(),
        };

        let label = if player.user_id == state.self_user_id {
            format!("{} (you){pos}", player.name)
        } else {
            format!("{}{pos}", player.name)
        };
        ui.bullet_text(label);
    }
}

fn render_chat_controls(ui: &imgui::Ui, render_loop: &mut RenderLoop) {
    ui.text("World Chat");

    let pressed_enter = ui
        .input_text("##world-chat-input", &mut render_loop.chat_input)
        .enter_returns_true(true)
        .build();

    let can_send = CHAT_HOOK_READY.load(Ordering::Relaxed);
    let send_clicked = ui.button("Send");
    if (pressed_enter || send_clicked) && can_send {
        match queue_world_chat(&render_loop.chat_input) {
            Ok(()) => {
                render_loop.chat_status = "Queued world chat.".to_owned();
                render_loop.chat_input.clear();
            },
            Err(err) => {
                render_loop.chat_status = err;
            },
        }
    } else if (pressed_enter || send_clicked) && !can_send {
        render_loop.chat_status = "World chat hook is not ready.".to_owned();
    }

    if !render_loop.chat_status.is_empty() {
        ui.text_wrapped(&render_loop.chat_status);
    }
}

fn setup_tracing() {
    hudhook::alloc_console().ok();
    hudhook::enable_console_colors();

    let log_file = hudhook::util::get_dll_path()
        .map(|mut path| {
            path.set_extension("log");
            path
        })
        .and_then(|path| File::create(path).ok());

    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new("info"))
        .with(
            fmt::layer().event_format(
                fmt::format()
                    .with_level(true)
                    .with_thread_ids(true)
                    .with_file(true)
                    .with_line_number(true)
                    .with_thread_names(true),
            ),
        );

    if let Some(log_file) = log_file {
        subscriber
            .with(
                fmt::layer()
                    .with_thread_ids(true)
                    .with_file(true)
                    .with_line_number(true)
                    .with_thread_names(true)
                    .with_writer(Mutex::new(log_file))
                    .with_ansi(false),
            )
            .init();
    } else {
        subscriber.init();
    }
}

fn format_packet_dump(direction: &str, bytes: &[u8]) -> String {
    let mut out = String::new();
    out.push_str(&format!("[{direction}] {} bytes\n", bytes.len()));

    for (row, chunk) in bytes.chunks(16).enumerate() {
        let offset = row * 16;
        out.push_str(&format!("{offset:08X}  "));

        for i in 0..16 {
            if let Some(byte) = chunk.get(i) {
                out.push_str(&format!("{byte:02X} "));
            } else {
                out.push_str("   ");
            }
            if i == 7 {
                out.push(' ');
            }
        }

        out.push(' ');

        for byte in chunk {
            let ch = if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push('\n');
    }

    out
}

unsafe fn packet_bytes<'a>(packet: *mut Il2CppByteArray) -> Option<&'a [u8]> {
    if packet.is_null() {
        return None;
    }

    let len = unsafe { (*packet).len };
    if !(1..MAX_PACKET_LEN).contains(&len) {
        return None;
    }

    Some(unsafe { slice::from_raw_parts((*packet).data.as_ptr(), len as usize) })
}

fn maybe_preview_packet(direction: &str, bytes: &[u8]) {
    if !PACKET_LOGGING_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    println!("{}", format_packet_dump(direction, bytes));
}

unsafe extern "system" fn send_detour(method: *const c_void) -> *mut c_void {
    let packet = unsafe { SendDetour.call(method) };
    if let Some(bytes) = unsafe { packet_bytes(packet as *mut Il2CppByteArray) } {
        process_packet(bytes, false);
        maybe_preview_packet(">>>> SEND", bytes);
    }
    packet
}

unsafe extern "system" fn receive_detour(this: *mut c_void, method: *const c_void) -> *mut c_void {
    let packet = unsafe { ReceiveDetour.call(this, method) };
    if let Some(bytes) = unsafe { packet_bytes(packet as *mut Il2CppByteArray) } {
        process_packet(bytes, true);
        maybe_preview_packet("<<<< RECEIVE", bytes);
    }
    packet
}

unsafe extern "system" fn submit_world_chat_detour(message: *mut c_void, method: *const c_void) {
    if !message.is_null() {
        info!("SubmitWorldChatMessage invoked");
    }

    unsafe { SubmitWorldChatDetour.call(message, method) };
}

unsafe extern "system" fn chat_ui_update_detour(this: *mut c_void, method: *const c_void) {
    let Some(chat_fns) = CHAT_FNS.get() else {
        unsafe {
            ChatUiUpdateDetour.call(this, method);
        }
        return;
    };

    if !this.is_null() {
        let pending = PENDING_WORLD_CHAT
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap()
            .take();

        if let Some(message) = pending {
            let managed_string =
                unsafe { (chat_fns.il2cpp_string_new_len)(message.as_ptr(), message.len() as i32) };

            if !managed_string.is_null() {
                let submitted =
                    unsafe { (chat_fns.chat_ui_submit)(this, managed_string, std::ptr::null()) };
                if !submitted {
                    unsafe { (chat_fns.submit_world_chat)(managed_string, std::ptr::null()) };
                }
            }
        }
    }

    unsafe {
        ChatUiUpdateDetour.call(this, method);
    }
}

fn queue_world_chat(message: &str) -> Result<(), String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return Err("Chat message cannot be empty.".to_owned());
    }

    if CHAT_FNS.get().is_none() {
        return Err("Game chat function not initialized.".to_owned());
    }

    *PENDING_WORLD_CHAT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some(trimmed.to_owned());
    Ok(())
}

fn install_packet_hooks() {
    let module = match unsafe { GetModuleHandleW(w!("GameAssembly.dll")) } {
        Ok(handle) => handle,
        Err(e) => {
            warn!("GameAssembly.dll not found: {e}");
            return;
        },
    };

    let base = module.0 as usize;
    let send_addr = (base + SEND_RVA) as *const ();
    let receive_addr = (base + RECEIVE_RVA) as *const ();

    unsafe {
        let send_fn: unsafe extern "system" fn(*const c_void) -> *mut c_void =
            std::mem::transmute(send_addr);
        let receive_fn: unsafe extern "system" fn(*mut c_void, *const c_void) -> *mut c_void =
            std::mem::transmute(receive_addr);

        if let Err(e) = SendDetour.initialize(send_fn, |method| send_detour(method)) {
            error!("failed to initialize send detour: {e}");
            return;
        }
        if let Err(e) =
            ReceiveDetour.initialize(receive_fn, |this, method| receive_detour(this, method))
        {
            error!("failed to initialize receive detour: {e}");
            return;
        }
        if let Err(e) = SendDetour.enable() {
            error!("failed to enable send detour: {e}");
            return;
        }
        if let Err(e) = ReceiveDetour.enable() {
            error!("failed to enable receive detour: {e}");
            return;
        }
    }

    PACKET_HOOKS_READY.store(true, Ordering::SeqCst);
    info!("packet hooks ready");
}

fn install_chat_hook() {
    let module = match unsafe { GetModuleHandleW(w!("GameAssembly.dll")) } {
        Ok(handle) => handle,
        Err(e) => {
            warn!("GameAssembly.dll not found for chat hook: {e}");
            return;
        },
    };

    let base = module.0 as usize;
    let chat_ui_update_addr = (base + CHAT_UI_UPDATE_RVA) as *const ();
    let chat_ui_submit_addr = (base + CHAT_UI_SUBMIT_RVA) as *const ();
    let submit_addr = (base + SUBMIT_WORLD_CHAT_RVA) as *const ();
    let string_new_len_addr = (base + IL2CPP_STRING_NEW_LEN_RVA) as *const ();

    let chat_ui_update_fn: unsafe extern "system" fn(*mut c_void, *const c_void) =
        unsafe { std::mem::transmute(chat_ui_update_addr) };
    let chat_ui_submit_fn: unsafe extern "system" fn(*mut c_void, *mut c_void, *const c_void) -> bool =
        unsafe { std::mem::transmute(chat_ui_submit_addr) };
    let submit_fn: unsafe extern "system" fn(*mut c_void, *const c_void) =
        unsafe { std::mem::transmute(submit_addr) };
    let string_new_len_fn: unsafe extern "system" fn(*const u8, i32) -> *mut c_void =
        unsafe { std::mem::transmute(string_new_len_addr) };

    let _ = CHAT_FNS.set(ChatFns {
        chat_ui_submit: chat_ui_submit_fn,
        submit_world_chat: submit_fn,
        il2cpp_string_new_len: string_new_len_fn,
    });

    unsafe {
        if let Err(e) =
            SubmitWorldChatDetour.initialize(submit_fn, |message, method| submit_world_chat_detour(message, method))
        {
            error!("failed to initialize world chat detour: {e}");
            return;
        }
        if let Err(e) = SubmitWorldChatDetour.enable() {
            error!("failed to enable world chat detour: {e}");
            return;
        }
        if let Err(e) =
            ChatUiUpdateDetour.initialize(chat_ui_update_fn, |this, method| chat_ui_update_detour(this, method))
        {
            error!("failed to initialize ChatUI update detour: {e}");
            return;
        }
        if let Err(e) = ChatUiUpdateDetour.enable() {
            error!("failed to enable ChatUI update detour: {e}");
            return;
        }
    }

    CHAT_HOOK_READY.store(true, Ordering::SeqCst);
    info!("world chat hook ready");
}

fn start(hmodule: HINSTANCE) {
    setup_tracing();
    install_packet_hooks();
    install_chat_hook();

    if let Err(e) = Hudhook::builder()
        .with::<ImguiDx12Hooks>(RenderLoop::default())
        .with_hmodule(hmodule)
        .build()
        .apply()
    {
        error!("failed to apply hooks: {e:?}");
        hudhook::eject();
        return;
    }

    info!("hooks applied successfully");
}

#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(hmodule: HINSTANCE, reason: u32, _: *mut c_void) {
    if reason == DLL_PROCESS_ATTACH {
        let hmodule_raw = hmodule.0 as usize;
        std::thread::spawn(move || {
            start(HINSTANCE(hmodule_raw as *mut c_void));
        });
    }
}
