//! `proc_ui.rpl`: process lifecycle (foreground/background/exit) messages.
//!
//! The title never leaves the foreground, so callbacks are recorded but
//! never invoked and message processing always reports "foreground".
use wiiu_hle_macros::hle_export;

use crate::hle::{HleContext, HleExport};

/// `PROCUI_STATUS_IN_FOREGROUND`.
const STATUS_FOREGROUND: u32 = 0;
/// Memory ProcUI asks the title to provide for its pool.
const POOL_SIZE: u32 = 0x1000;

/// ProcUI state.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct ProcUiState {
    pub running: bool,
    /// Save callback set by `ProcUIInit`/`ProcUIInitEx`/`ProcUISetSaveCallback`.
    pub save_callback: (u32, u32),
}

#[hle_export(lib = "proc_ui", name = "ProcUIInit")]
fn proc_ui_init(ctx: &mut HleContext<'_>, save_callback: u32) {
    ctx.os.proc_ui = ProcUiState { running: true, save_callback: (save_callback, 0) };
}

#[hle_export(lib = "proc_ui", name = "ProcUIInitEx")]
fn proc_ui_init_ex(ctx: &mut HleContext<'_>, save_callback: u32, argument: u32) {
    ctx.os.proc_ui = ProcUiState { running: true, save_callback: (save_callback, argument) };
}

#[hle_export(lib = "proc_ui", name = "ProcUIShutdown")]
fn proc_ui_shutdown(ctx: &mut HleContext<'_>) {
    ctx.os.proc_ui.running = false;
}

#[hle_export(lib = "proc_ui", name = "ProcUIIsRunning")]
fn proc_ui_is_running(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.proc_ui.running
}

#[hle_export(lib = "proc_ui", name = "ProcUIInForeground")]
fn proc_ui_in_foreground(_ctx: &mut HleContext<'_>) -> bool {
    true
}

#[hle_export(lib = "proc_ui", name = "ProcUIInShutdown")]
fn proc_ui_in_shutdown(_ctx: &mut HleContext<'_>) -> bool {
    false
}

#[hle_export(lib = "proc_ui", name = "ProcUIRegisterCallback")]
fn proc_ui_register_callback(
    _ctx: &mut HleContext<'_>,
    _message: u32,
    _callback: u32,
    _param: u32,
    _priority: u32,
) {
}

#[hle_export(lib = "proc_ui", name = "ProcUIRegisterCallbackCore")]
fn proc_ui_register_callback_core(
    _ctx: &mut HleContext<'_>,
    _message: u32,
    _callback: u32,
    _param: u32,
    _priority: u32,
    _core: u32,
) {
}

#[hle_export(lib = "proc_ui", name = "ProcUIRegisterBackgroundCallback")]
fn proc_ui_register_background_callback(
    _ctx: &mut HleContext<'_>,
    _callback: u32,
    _param: u32,
    _ticks: u64,
) {
}

#[hle_export(lib = "proc_ui", name = "ProcUIClearCallbacks")]
fn proc_ui_clear_callbacks(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "proc_ui", name = "ProcUISetSaveCallback")]
fn proc_ui_set_save_callback(ctx: &mut HleContext<'_>, callback: u32, argument: u32) {
    ctx.os.proc_ui.save_callback = (callback, argument);
}

#[hle_export(lib = "proc_ui", name = "ProcUISetCallbackStackSize")]
fn proc_ui_set_callback_stack_size(_ctx: &mut HleContext<'_>, _size: u32) {}

#[hle_export(lib = "proc_ui", name = "ProcUICalcMemorySize")]
fn proc_ui_calc_memory_size(_ctx: &mut HleContext<'_>, _count: u32) -> u32 {
    POOL_SIZE
}

#[hle_export(lib = "proc_ui", name = "ProcUISetMemoryPool")]
fn proc_ui_set_memory_pool(_ctx: &mut HleContext<'_>, _pool: u32, _size: u32) -> i32 {
    0
}

#[hle_export(lib = "proc_ui", name = "ProcUISetBucketStorage")]
fn proc_ui_set_bucket_storage(_ctx: &mut HleContext<'_>, _buffer: u32, _size: u32) {}

#[hle_export(lib = "proc_ui", name = "ProcUISetMEM1Storage")]
fn proc_ui_set_mem1_storage(_ctx: &mut HleContext<'_>, _buffer: u32, _size: u32) {}

#[hle_export(lib = "proc_ui", name = "ProcUIDrawDoneRelease")]
fn proc_ui_draw_done_release(_ctx: &mut HleContext<'_>) {}

#[hle_export(lib = "proc_ui", name = "ProcUIProcessMessages")]
fn proc_ui_process_messages(_ctx: &mut HleContext<'_>, _block: bool) -> u32 {
    STATUS_FOREGROUND
}

#[hle_export(lib = "proc_ui", name = "ProcUISubProcessMessages")]
fn proc_ui_sub_process_messages(_ctx: &mut HleContext<'_>, _block: bool) -> u32 {
    STATUS_FOREGROUND
}

pub const EXPORTS: &[HleExport] = &[
    PROC_UI_INIT,
    PROC_UI_INIT_EX,
    PROC_UI_SHUTDOWN,
    PROC_UI_IS_RUNNING,
    PROC_UI_IN_FOREGROUND,
    PROC_UI_IN_SHUTDOWN,
    PROC_UI_REGISTER_CALLBACK,
    PROC_UI_REGISTER_CALLBACK_CORE,
    PROC_UI_REGISTER_BACKGROUND_CALLBACK,
    PROC_UI_CLEAR_CALLBACKS,
    PROC_UI_SET_SAVE_CALLBACK,
    PROC_UI_SET_CALLBACK_STACK_SIZE,
    PROC_UI_CALC_MEMORY_SIZE,
    PROC_UI_SET_MEMORY_POOL,
    PROC_UI_SET_BUCKET_STORAGE,
    PROC_UI_SET_MEM1_STORAGE,
    PROC_UI_DRAW_DONE_RELEASE,
    PROC_UI_PROCESS_MESSAGES,
    PROC_UI_SUB_PROCESS_MESSAGES,
];
