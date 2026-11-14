//! Process termination.
use wiiu_hle_macros::hle_export;
use wiiu_memory::GuestPtr;

use crate::hle::{HleContext, HleError};

fn terminate(ctx: &mut HleContext<'_>, code: i32) {
    log::info!("guest exited with status {code}");
    ctx.os.exit_code = Some(code);
    ctx.os.frame_done = true;
}

#[hle_export(lib = "coreinit", name = "exit")]
fn exit(ctx: &mut HleContext<'_>, code: i32) {
    terminate(ctx, code);
}

#[hle_export(lib = "coreinit", name = "_Exit")]
fn underscore_exit(ctx: &mut HleContext<'_>, code: i32) {
    terminate(ctx, code);
}

#[hle_export(lib = "coreinit", name = "OSFatal")]
fn os_fatal(ctx: &mut HleContext<'_>, message: GuestPtr<u8>) -> Result<(), HleError> {
    let message = ctx.memory.read_c_string(message.address(), 0x1000)?;
    Err(HleError::Fatal(message))
}

#[cfg(test)]
mod tests {
    use crate::hle::{
        dispatch,
        test_env::{SCRATCH, TestEnv},
    };

    /// Intent: `exit` ends the title with its status and ends the frame, so the front end
    /// can show that the title quit.
    /// Pass: the exit code is kept and the frame is marked done.
    #[test]
    fn exit_keeps_the_status() {
        let mut env = TestEnv::new();
        env.call("coreinit", "exit", &[3]);
        assert_eq!(env.os.exit_code, Some(3));
        assert!(env.os.frame_done);
    }

    /// Intent: `OSFatal` stops emulation with the title's own message.
    /// Pass: the call fails with a fatal error that carries the guest string.
    #[test]
    fn fatal_carries_the_guest_message() {
        let mut env = TestEnv::new();
        env.memory.write_c_string(SCRATCH, "disc read error").unwrap();
        env.cpu.gpr[3] = SCRATCH;
        let index = env.os.registry.function_index("coreinit", "OSFatal").unwrap();
        let error = dispatch(&mut env.ctx(), index).unwrap_err();
        assert!(error.to_string().contains("disc read error"));
    }
}
