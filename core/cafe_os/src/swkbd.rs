//! `swkbd.rpl`: the software keyboard (port of Cemu's `swkbd.cpp`).
//!
//! The title's own keyboard is not drawn. While the title shows its input form or
//! keyboard, the front end takes text from the host keyboard (IME included) and hands it
//! over with [`Swkbd::set_text`] and [`Swkbd::decide`]; the title reads it back from the
//! form string and from the buffer of its receiver, which is told of every change.
use wiiu_hle_macros::hle_export;
use wiiu_memory::Memory;

use crate::{
    CafeOs,
    coreinit::thread::{Wait, block},
    hle::{HleContext, HleError, HleExport, callback::GuestCall},
};

/// Longest form string, in UTF-16 units.
const FORM_STRING_MAX_LENGTH: usize = 4096;
/// `SWKBD_STATE_BLANK`: not shown.
const STATE_BLANK: u32 = 0;
/// `SWKBD_STATE_DISPLAYED`.
const STATE_DISPLAYED: u32 = 2;
/// Timer ticks per second.
const TIMER_CLOCK: u64 = 62_156_250;

/// `SwkbdKeyboardArg`: offset of its `ReceiverArg`.
const KEYBOARD_ARG_RECEIVER: u32 = 0xA8;
/// `SwkbdAppearArg`: offsets of the initial text and its maximum length.
const APPEAR_ARG_INITIAL_TEXT: u32 = 0xC8;
const APPEAR_ARG_MAX_TEXT_LENGTH: u32 = 0xD0;

/// Learning dictionary (`SwkbdInitLearnDic`).
const LEARN_DIC_SIZE: u32 = 0xA460;
const LEARN_DIC_ENTRY_COUNT: u32 = 1000;
const LEARN_DIC_ENTRY_STRIDE: u32 = 0x20;
const LEARN_DIC_INDEX_COUNT: u32 = LEARN_DIC_ENTRY_COUNT + 1;
/// `"NJDC"`.
const LEARN_DIC_MAGIC: u32 = 0x4E4A_4443;

/// `nn::swkbd::ReceiverArg`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone, Copy)]
struct ReceiverArg {
    event_receiver: u32,
    string_buf: u32,
    /// In UTF-16 units, the terminator included.
    string_buf_size: i32,
}

impl ReceiverArg {
    fn read(memory: &Memory, address: u32) -> Result<Self, HleError> {
        Ok(Self {
            event_receiver: memory.read_u32(address)?,
            string_buf: memory.read_u32(address + 4)?,
            string_buf_size: memory.read_u32(address + 8)?.cast_signed(),
        })
    }
}

/// The keyboard's state.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
pub struct Swkbd {
    /// Set by `SwkbdAppearInputForm`/`SwkbdAppearKeyboard`.
    active: bool,
    /// The keyboard without an input form: the receiver's buffer limits the length.
    keyboard_only: bool,
    /// Set when the text is confirmed; stays set after the keyboard goes away.
    decided: bool,
    /// The text, in UTF-16 units.
    text: Vec<u16>,
    max_text_length: usize,
    receiver: ReceiverArg,
    /// Guest copy of the form string for `SwkbdGetInputFormString`.
    form_string: u32,
    /// Guest `ChangeStringParam` for the receiver's callback.
    change_param: u32,
    /// Work `SwkbdCalcSubThread*` pretend to have.
    font_work: u32,
    predict_work: u32,
}

impl Swkbd {
    /// Whether the title waits for text.
    #[inline]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// The text so far.
    pub fn text(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }

    /// Most UTF-16 units the title takes.
    fn max_length(&self) -> usize {
        if self.keyboard_only {
            usize::try_from(self.receiver.string_buf_size - 1).unwrap_or(0)
        } else {
            self.max_text_length
        }
    }
}

impl CafeOs {
    /// Replaces the keyboard's text with `text` (cut to what the title takes) and tells the
    /// title, as typing does on the GamePad.
    ///
    /// # Errors
    /// Fails if the title's buffer is not in guest memory.
    pub fn swkbd_set_text(&mut self, memory: &mut Memory, text: &str) -> Result<(), HleError> {
        if !self.swkbd.active {
            return Ok(());
        }
        let mut units: Vec<u16> = text.encode_utf16().collect();
        units.truncate(self.swkbd.max_length());
        if units == self.swkbd.text {
            return Ok(());
        }
        self.swkbd.text = units;
        self.swkbd_string_changed(memory)
    }

    /// Confirms the text (the GamePad's OK).
    pub const fn swkbd_decide(&mut self) {
        self.swkbd.decided = true;
    }

    /// Cemu's `swkbd_inputStringChanged`: writes the text to the receiver's buffer and
    /// calls its `changeString`.
    fn swkbd_string_changed(&mut self, memory: &mut Memory) -> Result<(), HleError> {
        let receiver = self.swkbd.receiver;
        if receiver.string_buf_size > 1 && receiver.string_buf != 0 {
            let room = usize::try_from(receiver.string_buf_size - 1).unwrap_or(0);
            let length = room.min(self.swkbd.text.len());
            write_utf16(memory, receiver.string_buf, &self.swkbd.text[..length])?;
        }
        if receiver.event_receiver != 0 {
            let vtable = memory.read_u32(receiver.event_receiver)?;
            let change_string = if vtable == 0 { 0 } else { memory.read_u32(vtable + 0xC)? };
            if change_string != 0 {
                if self.swkbd.change_param == 0 {
                    self.swkbd.change_param = self.registry.allocate_data(8, 8);
                }
                memory.write_u32(self.swkbd.change_param, 0)?;
                memory.write_u32(self.swkbd.change_param + 4, 0)?;
                self.callbacks.push(GuestCall {
                    function: change_string,
                    args: vec![receiver.event_receiver, self.swkbd.change_param],
                });
            }
        }
        Ok(())
    }
}

/// Writes `units` and a terminator as big-endian UTF-16.
fn write_utf16(memory: &mut Memory, address: u32, units: &[u16]) -> Result<(), HleError> {
    let bytes: Vec<u8> = units.iter().chain([&0]).flat_map(|unit| unit.to_be_bytes()).collect();
    memory.write_bytes(address, &bytes)?;
    Ok(())
}

/// Sleeps the calling thread for `ticks`. Returns `true` once it woke: the call runs again
/// then, and must not redo its work.
fn sleep(ctx: &mut HleContext<'_>, ticks: u64) -> bool {
    if let Some(Wait::Sleep(_)) = ctx.os.coreinit.threads.take_woke_from() {
        return true;
    }
    let deadline = ctx.os.timebase.ticks_since_boot().saturating_add(ticks);
    block(ctx, Wait::Sleep(deadline));
    false
}

#[hle_export(lib = "swkbd", name = "SwkbdCreate__3RplFPUcQ3_2nn5swkbd10RegionTypeUiP8FSClient")]
fn swkbd_create(
    ctx: &mut HleContext<'_>,
    _work_memory: u32,
    _region: u32,
    _unknown: u32,
    _fs_client: u32,
) {
    if sleep(ctx, TIMER_CLOCK) {
        return;
    }
    let swkbd = &mut ctx.os.swkbd;
    let (form_string, change_param) = (swkbd.form_string, swkbd.change_param);
    // Takes a while on hardware: MH3U needs that not to lock up.
    *swkbd = Swkbd { form_string, change_param, font_work: 3, predict_work: 3, ..Swkbd::default() };
}

#[hle_export(lib = "swkbd", name = "SwkbdCalc__3RplFRCQ3_2nn5swkbd14ControllerInfo")]
fn swkbd_calc(_ctx: &mut HleContext<'_>, _controller_info: u32) {}

#[hle_export(lib = "swkbd", name = "SwkbdGetStateKeyboard__3RplFv")]
fn swkbd_get_state_keyboard(ctx: &mut HleContext<'_>) -> u32 {
    if ctx.os.swkbd.active { STATE_DISPLAYED } else { STATE_BLANK }
}

#[hle_export(lib = "swkbd", name = "SwkbdGetStateInputForm__3RplFv")]
fn swkbd_get_state_input_form(ctx: &mut HleContext<'_>) -> u32 {
    if ctx.os.swkbd.active { STATE_DISPLAYED } else { STATE_BLANK }
}

#[hle_export(lib = "swkbd", name = "SwkbdSetReceiver__3RplFRCQ3_2nn5swkbd11ReceiverArg")]
fn swkbd_set_receiver(ctx: &mut HleContext<'_>, receiver_arg: u32) -> Result<u32, HleError> {
    ctx.os.swkbd.receiver = ReceiverArg::read(ctx.memory, receiver_arg)?;
    Ok(0)
}

#[hle_export(lib = "swkbd", name = "SwkbdAppearInputForm__3RplFRCQ3_2nn5swkbd9AppearArg")]
fn swkbd_appear_input_form(ctx: &mut HleContext<'_>, appear_arg: u32) -> Result<u32, HleError> {
    let max = ctx.memory.read_u32(appear_arg + APPEAR_ARG_MAX_TEXT_LENGTH)?.cast_signed();
    let max_text_length = usize::try_from(max)
        .ok()
        .filter(|&max| max > 0)
        .map_or(FORM_STRING_MAX_LENGTH - 1, |max| max.min(FORM_STRING_MAX_LENGTH - 1));
    let initial = ctx.memory.read_u32(appear_arg + APPEAR_ARG_INITIAL_TEXT)?;
    let mut text = Vec::new();
    if initial != 0 {
        for i in 0..max_text_length as u32 {
            let mut unit = [0; 2];
            ctx.memory.read_bytes(initial + 2 * i, &mut unit)?;
            match u16::from_be_bytes(unit) {
                0 => break,
                unit => text.push(unit),
            }
        }
    }
    let swkbd = &mut ctx.os.swkbd;
    swkbd.active = true;
    swkbd.decided = false;
    swkbd.keyboard_only = false;
    swkbd.max_text_length = max_text_length;
    swkbd.text = text;
    Ok(1)
}

#[hle_export(lib = "swkbd", name = "SwkbdAppearKeyboard__3RplFRCQ3_2nn5swkbd11KeyboardArg")]
fn swkbd_appear_keyboard(ctx: &mut HleContext<'_>, keyboard_arg: u32) -> Result<u32, HleError> {
    let receiver = ReceiverArg::read(ctx.memory, keyboard_arg + KEYBOARD_ARG_RECEIVER)?;
    let swkbd = &mut ctx.os.swkbd;
    swkbd.active = true;
    swkbd.keyboard_only = true;
    swkbd.decided = false;
    swkbd.text.clear();
    swkbd.receiver = receiver;
    Ok(1)
}

#[hle_export(lib = "swkbd", name = "SwkbdDisappearInputForm__3RplFv")]
fn swkbd_disappear_input_form(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.swkbd.active = false;
    1
}

#[hle_export(lib = "swkbd", name = "SwkbdDisappearKeyboard__3RplFv")]
fn swkbd_disappear_keyboard(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.swkbd.active = false;
    1
}

#[hle_export(lib = "swkbd", name = "SwkbdGetInputFormString__3RplFv")]
fn swkbd_get_input_form_string(ctx: &mut HleContext<'_>) -> Result<u32, HleError> {
    if ctx.os.swkbd.form_string == 0 {
        ctx.os.swkbd.form_string =
            ctx.os.registry.allocate_data(2 * FORM_STRING_MAX_LENGTH as u32, 8);
    }
    let swkbd = &ctx.os.swkbd;
    let length = swkbd.text.len().min(FORM_STRING_MAX_LENGTH - 1);
    write_utf16(ctx.memory, swkbd.form_string, &swkbd.text[..length])?;
    Ok(swkbd.form_string)
}

#[hle_export(lib = "swkbd", name = "SwkbdIsDecideOkButton__3RplFPb")]
fn swkbd_is_decide_ok_button(ctx: &mut HleContext<'_>, _out: u32) -> bool {
    ctx.os.swkbd.decided
}

#[hle_export(lib = "swkbd", name = "SwkbdGetDrawStringInfo__3RplFPQ3_2nn5swkbd14DrawStringInfo")]
fn swkbd_get_draw_string_info(ctx: &mut HleContext<'_>, info: u32) -> Result<u32, HleError> {
    for i in 0..6 {
        ctx.memory.write_u32(info + 4 * i, u32::MAX)?;
    }
    ctx.memory.write_bytes(info + 0x18, &[0])?;
    Ok(0)
}

#[hle_export(lib = "swkbd", name = "SwkbdInitLearnDic__3RplFPv")]
fn swkbd_init_learn_dic(ctx: &mut HleContext<'_>, dictionary: u32) -> Result<u32, HleError> {
    if dictionary == 0 {
        return Ok(0);
    }
    let first_index = 0x48;
    let second_index = first_index + LEARN_DIC_INDEX_COUNT * 2;
    let index_end = second_index + LEARN_DIC_INDEX_COUNT * 2;
    let entry_end = index_end + LEARN_DIC_ENTRY_COUNT * LEARN_DIC_ENTRY_STRIDE;
    ctx.memory.fill(dictionary, LEARN_DIC_SIZE, 0)?;
    let header = [
        LEARN_DIC_MAGIC,
        0x30000,
        0x8002_0000,
        LEARN_DIC_SIZE - 0x48,
        0x2C,
        0x6E,
        0x6E,
        index_end,
        0,
        0,
        LEARN_DIC_ENTRY_COUNT,
        LEARN_DIC_ENTRY_STRIDE,
        0,
        0,
        0,
        first_index,
        second_index,
        entry_end,
    ];
    for (i, value) in (0..).zip(header) {
        ctx.memory.write_u32(dictionary + 4 * i, value)?;
    }
    ctx.memory.write_u32(dictionary + LEARN_DIC_SIZE - 4, LEARN_DIC_MAGIC)?;
    Ok(1)
}

#[hle_export(lib = "swkbd", name = "SwkbdIsNeedCalcSubThreadFont__3RplFv")]
fn swkbd_is_need_calc_sub_thread_font(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.swkbd.font_work > 0
}

#[hle_export(lib = "swkbd", name = "SwkbdIsNeedCalcSubThreadPredict__3RplFv")]
fn swkbd_is_need_calc_sub_thread_predict(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.swkbd.predict_work > 0
}

#[hle_export(lib = "swkbd", name = "SwkbdCalcSubThreadFont__3RplFv")]
fn swkbd_calc_sub_thread_font(ctx: &mut HleContext<'_>) {
    if ctx.os.swkbd.font_work == 0 {
        return;
    }
    // 5 ms of pretended work.
    if sleep(ctx, TIMER_CLOCK / 200) {
        ctx.os.swkbd.font_work -= 1;
    }
}

#[hle_export(lib = "swkbd", name = "SwkbdCalcSubThreadPredict__3RplFv")]
fn swkbd_calc_sub_thread_predict(ctx: &mut HleContext<'_>) {
    if ctx.os.swkbd.predict_work == 0 {
        return;
    }
    if sleep(ctx, TIMER_CLOCK / 200) {
        ctx.os.swkbd.predict_work -= 1;
    }
}

pub const EXPORTS: &[HleExport] = &[
    SWKBD_CREATE,
    SWKBD_CALC,
    SWKBD_GET_STATE_KEYBOARD,
    SWKBD_GET_STATE_INPUT_FORM,
    SWKBD_SET_RECEIVER,
    SWKBD_APPEAR_INPUT_FORM,
    SWKBD_APPEAR_KEYBOARD,
    SWKBD_DISAPPEAR_INPUT_FORM,
    SWKBD_DISAPPEAR_KEYBOARD,
    SWKBD_GET_INPUT_FORM_STRING,
    SWKBD_IS_DECIDE_OK_BUTTON,
    SWKBD_GET_DRAW_STRING_INFO,
    SWKBD_INIT_LEARN_DIC,
    SWKBD_IS_NEED_CALC_SUB_THREAD_FONT,
    SWKBD_IS_NEED_CALC_SUB_THREAD_PREDICT,
    SWKBD_CALC_SUB_THREAD_FONT,
    SWKBD_CALC_SUB_THREAD_PREDICT,
];

#[cfg(test)]
mod tests {
    use crate::hle::test_env::TestEnv;

    /// Intent: text typed on the host reaches the title as on the GamePad, Japanese too.
    /// Pass: after `SwkbdAppearInputForm` the state is "displayed"; set text lands in the
    /// receiver's buffer (cut to its size) and in the form string; OK is reported once
    /// decided; `SwkbdDisappearInputForm` hides the keyboard.
    #[test]
    fn host_text_reaches_the_title() {
        let mut env = TestEnv::new();
        let (appear, receiver, buffer) = (0x1000_0000, 0x1000_1000, 0x1000_2000);
        env.memory.fill(appear, 0x100, 0).unwrap();
        env.memory.write_u32(appear + 0xD0, 6).unwrap();
        env.memory.write_u32(receiver, 0).unwrap();
        env.memory.write_u32(receiver + 4, buffer).unwrap();
        env.memory.write_u32(receiver + 8, 4).unwrap();
        env.call("swkbd", "SwkbdSetReceiver__3RplFRCQ3_2nn5swkbd11ReceiverArg", &[receiver]);
        env.call("swkbd", "SwkbdAppearInputForm__3RplFRCQ3_2nn5swkbd9AppearArg", &[appear]);
        assert_eq!(env.call("swkbd", "SwkbdGetStateInputForm__3RplFv", &[]), 2);

        env.os.swkbd_set_text(&mut env.memory, "シュルクabc").unwrap();
        let mut units = [0; 8];
        env.memory.read_bytes(buffer, &mut units).unwrap();
        let expected: Vec<u8> =
            "シュル".encode_utf16().chain([0]).flat_map(u16::to_be_bytes).collect();
        assert_eq!(units.as_slice(), expected.as_slice());

        let form = env.call("swkbd", "SwkbdGetInputFormString__3RplFv", &[]);
        let mut text = [0; 14];
        env.memory.read_bytes(form, &mut text).unwrap();
        let expected: Vec<u8> =
            "シュルクab".encode_utf16().chain([0]).flat_map(u16::to_be_bytes).collect();
        assert_eq!(text.as_slice(), expected.as_slice());

        assert_eq!(env.call("swkbd", "SwkbdIsDecideOkButton__3RplFPb", &[0]), 0);
        env.os.swkbd_decide();
        assert_eq!(env.call("swkbd", "SwkbdIsDecideOkButton__3RplFPb", &[0]), 1);
        env.call("swkbd", "SwkbdDisappearInputForm__3RplFv", &[]);
        assert_eq!(env.call("swkbd", "SwkbdGetStateInputForm__3RplFv", &[]), 0);
    }
}
