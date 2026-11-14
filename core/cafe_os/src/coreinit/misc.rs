//! Small process-level exports with no further machinery behind them.
use wiiu_hle_macros::hle_export;

use crate::hle::HleContext;

/// Threads are torn down with the process; nothing to block.
#[hle_export(lib = "coreinit", name = "OSBlockThreadsOnExit")]
fn os_block_threads_on_exit(_ctx: &mut HleContext<'_>) {}

/// Returns `true` (the setting was applied).
#[hle_export(lib = "coreinit", name = "OSEnableHomeButtonMenu")]
fn os_enable_home_button_menu(ctx: &mut HleContext<'_>, enable: bool) -> bool {
    ctx.os.coreinit.home_button_menu = enable;
    true
}

#[hle_export(lib = "coreinit", name = "OSIsHomeButtonMenuEnabled")]
fn os_is_home_button_menu_enabled(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.coreinit.home_button_menu
}

#[cfg(test)]
mod tests {
    use crate::hle::test_env::TestEnv;

    /// Intent: the HOME button menu switch is remembered for the title to read back.
    /// Pass: after disabling it, `OSIsHomeButtonMenuEnabled` says false; after enabling,
    /// true.
    #[test]
    fn home_button_menu_switch_is_remembered() {
        let mut env = TestEnv::new();
        env.call("coreinit", "OSEnableHomeButtonMenu", &[0]);
        assert_eq!(env.call("coreinit", "OSIsHomeButtonMenuEnabled", &[]), 0);
        env.call("coreinit", "OSEnableHomeButtonMenu", &[1]);
        assert_eq!(env.call("coreinit", "OSIsHomeButtonMenuEnabled", &[]), 1);
    }
}
