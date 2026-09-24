use super::{PrivacyMode, PrivacyModeState, INVALID_PRIVACY_MODE_CONN_ID, NO_PHYSICAL_DISPLAYS};
use crate::{platform::windows::reg_display_settings, virtual_display_manager};
use hbb_common::{allow_err, bail, config::Config, log, ResultType};
use std::{
    io::Error,
    ops::{Deref, DerefMut},
    thread,
    time::Duration,
};
use virtual_display::MonitorMode;
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE},
        ntdef::{NULL, WCHAR},
    },
    um::{
        wingdi::{
            DEVMODEW, DISPLAY_DEVICEW, DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICE_ATTACHED_TO_DESKTOP,
            DISPLAY_DEVICE_MIRRORING_DRIVER, DISPLAY_DEVICE_PRIMARY_DEVICE, DM_POSITION,
        },
        winuser::{
            ChangeDisplaySettingsExW, EnumDisplayDevicesW, EnumDisplaySettingsExW,
            EnumDisplaySettingsW, CDS_NORESET, CDS_RESET, CDS_SET_PRIMARY, CDS_UPDATEREGISTRY,
            DISP_CHANGE_FAILED, DISP_CHANGE_SUCCESSFUL, EDD_GET_DEVICE_INTERFACE_NAME,
            ENUM_CURRENT_SETTINGS, ENUM_REGISTRY_SETTINGS,
        },
    },
};

pub(super) const PRIVACY_MODE_IMPL: &str = super::PRIVACY_MODE_IMPL_WIN_VIRTUAL_DISPLAY;

const CONFIG_KEY_REG_RECOVERY: &str = "reg_recovery";

// How long to keep waiting for a plugged-in monitor to reach the console desktop after the
// first window expires. The attach was measured at ~9.4s on a dGPU-direct laptop, while the
// first window is 5s: bailing there rolls the plug-in back and destroys the pending attach.
const VIRTUAL_DISPLAY_RETRY_WAIT_MILLIS: u64 = 10_000;
// Budget for converging to a single virtual display before privacy mode starts. Only spent when
// there is a surplus, in which case `ensure_virtual_display()` has nothing to wait for. Kept short
// because a removal takes effect in well under a second, and the whole turn-on has to fit in the
// 20s the caller waits for.
const VIRTUAL_DISPLAY_PLUG_OUT_WAIT_MILLIS: u64 = 5_000;
const VIRTUAL_DISPLAY_PLUG_OUT_SETTLE_MILLIS: u64 = 2_500;

struct Display {
    dm: DEVMODEW,
    name: [WCHAR; 32],
    primary: bool,
}

pub struct PrivacyModeImpl {
    impl_key: String,
    conn_id: i32,
    displays: Vec<Display>,
    virtual_displays: Vec<Display>,
    virtual_displays_added: Vec<u32>,
}

struct TurnOnGuard<'a> {
    privacy_mode: &'a mut PrivacyModeImpl,
    succeeded: bool,
}

impl<'a> Deref for TurnOnGuard<'a> {
    type Target = PrivacyModeImpl;

    fn deref(&self) -> &Self::Target {
        self.privacy_mode
    }
}

impl<'a> DerefMut for TurnOnGuard<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.privacy_mode
    }
}

impl<'a> Drop for TurnOnGuard<'a> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.privacy_mode
                .turn_off_privacy(INVALID_PRIVACY_MODE_CONN_ID, None)
                .ok();
        }
    }
}

impl PrivacyModeImpl {
    pub fn new(impl_key: &str) -> Self {
        Self {
            impl_key: impl_key.to_owned(),
            conn_id: INVALID_PRIVACY_MODE_CONN_ID,
            displays: Vec::new(),
            virtual_displays: Vec::new(),
            virtual_displays_added: Vec::new(),
        }
    }

    // mainly from https://github.com/rustdesk-org/rustdesk/blob/44c3a52ca8502cf53b58b59db130611778d34dbe/libs/scrap/src/dxgi/mod.rs#L365
    fn set_displays(&mut self) {
        self.displays.clear();
        self.virtual_displays.clear();

        let mut i: DWORD = 0;
        loop {
            #[allow(invalid_value)]
            let mut dd: DISPLAY_DEVICEW = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
            dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as _;
            let ok = unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut dd as _, 0) };
            if ok == FALSE {
                break;
            }
            i += 1;
            if 0 == (dd.StateFlags & DISPLAY_DEVICE_ACTIVE)
                || (dd.StateFlags & DISPLAY_DEVICE_MIRRORING_DRIVER) > 0
            {
                continue;
            }
            #[allow(invalid_value)]
            let mut dm: DEVMODEW = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
            dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
            dm.dmDriverExtra = 0;
            unsafe {
                if FALSE
                    == EnumDisplaySettingsExW(
                        dd.DeviceName.as_ptr(),
                        ENUM_CURRENT_SETTINGS,
                        &mut dm as _,
                        0,
                    )
                {
                    if FALSE
                        == EnumDisplaySettingsExW(
                            dd.DeviceName.as_ptr(),
                            ENUM_REGISTRY_SETTINGS,
                            &mut dm as _,
                            0,
                        )
                    {
                        continue;
                    }
                }
            }

            let primary = (dd.StateFlags & DISPLAY_DEVICE_PRIMARY_DEVICE) > 0;
            let display = Display {
                dm,
                name: dd.DeviceName,
                primary,
            };

            if Self::is_virtual_display_device(&dd) {
                self.virtual_displays.push(display);
                continue;
            }
            self.displays.push(display);
        }
    }

    fn is_virtual_display_device(dd: &DISPLAY_DEVICEW) -> bool {
        match String::from_utf16(&dd.DeviceString) {
            Ok(s) => [
                virtual_display_manager::AMYUNI_IDD_DEVICE_STRING,
                virtual_display_manager::RUSTDESK_IDD_DEVICE_STRING,
            ]
            .iter()
            .any(|ds| s.len() >= ds.len() && &s[..ds.len()] == *ds),
            Err(_) => false,
        }
    }

    // An empty `virtual_displays` says nothing about *why*: the driver may have created no monitor,
    // or created one Windows never brought onto the desktop, or the adapter may be gone entirely.
    // Those need different fixes, and the second one is invisible at adapter level -- a pending
    // monitor only shows up when the adapter's own targets are enumerated. Log-only.
    fn log_virtual_display_targets() {
        let mut i: DWORD = 0;
        loop {
            #[allow(invalid_value)]
            let mut dd: DISPLAY_DEVICEW = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
            dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as _;
            if FALSE == unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut dd as _, 0) } {
                break;
            }
            i += 1;
            if !Self::is_virtual_display_device(&dd) {
                continue;
            }
            let adapter = std::string::String::from_utf16_lossy(&dd.DeviceName);
            log::info!(
                "Privacy mode: virtual display adapter {:?} state flags 0x{:x}",
                &adapter,
                dd.StateFlags
            );
            let mut j: DWORD = 0;
            loop {
                #[allow(invalid_value)]
                let mut target: DISPLAY_DEVICEW =
                    unsafe { std::mem::MaybeUninit::uninit().assume_init() };
                target.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as _;
                if FALSE == unsafe {
                    EnumDisplayDevicesW(dd.DeviceName.as_ptr(), j, &mut target as _, 0)
                } {
                    break;
                }
                j += 1;
                log::info!(
                    "Privacy mode:   target {:?} {:?} state flags 0x{:x}",
                    std::string::String::from_utf16_lossy(&target.DeviceName),
                    std::string::String::from_utf16_lossy(&target.DeviceString),
                    target.StateFlags
                );
            }
        }
    }

    // A panel left at 0x0 by an earlier privacy mode is off the desktop, so `set_displays()` sees
    // no physical display at all and the mode can never be entered again. Re-enabling from the
    // registry mode is what Windows itself does on the next logon. Callers must gate this on the
    // desktop holding only virtual displays, otherwise a monitor the user switched off by hand
    // would come back too.
    fn recover_off_desktop_displays(&mut self) {
        let mut recovered = 0;
        let mut i: DWORD = 0;
        loop {
            #[allow(invalid_value)]
            let mut dd: DISPLAY_DEVICEW = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
            dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as _;
            if FALSE == unsafe { EnumDisplayDevicesW(std::ptr::null(), i, &mut dd as _, 0) } {
                break;
            }
            i += 1;
            if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) > 0
                || (dd.StateFlags & DISPLAY_DEVICE_MIRRORING_DRIVER) > 0
                || Self::is_virtual_display_device(&dd)
            {
                continue;
            }
            let name = std::string::String::from_utf16_lossy(&dd.DeviceName);
            #[allow(invalid_value)]
            let mut dm: DEVMODEW = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
            dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
            dm.dmDriverExtra = 0;
            unsafe {
                if FALSE
                    == EnumDisplaySettingsExW(
                        dd.DeviceName.as_ptr(),
                        ENUM_REGISTRY_SETTINGS,
                        &mut dm as _,
                        0,
                    )
                {
                    continue;
                }
                let rc = ChangeDisplaySettingsExW(
                    dd.DeviceName.as_ptr(),
                    &mut dm,
                    NULL as _,
                    CDS_UPDATEREGISTRY | CDS_NORESET,
                    NULL as _,
                );
                if rc != DISP_CHANGE_SUCCESSFUL {
                    log::error!(
                        "Privacy mode: failed to re-enable display {:?}, {}",
                        &name,
                        Self::change_display_settings_ex_err_msg(rc)
                    );
                    continue;
                }
            }
            log::info!(
                "Privacy mode: re-enabled the off-desktop display {:?}",
                &name
            );
            recovered += 1;
        }

        if recovered > 0 {
            // Flush the queue built above. `CDS_RESET` would ignore it and re-apply every display's
            // persisted registry mode instead, which is a different (and staler) configuration.
            allow_err!(Self::commit_change_display(0));
            self.set_displays();
        }
    }

    fn restore_plug_out_monitor(&mut self) {
        let _ = virtual_display_manager::plug_out_monitor_indices(
            &self.virtual_displays_added,
            true,
            false,
        );
        self.virtual_displays_added.clear();
    }

    #[inline]
    fn change_display_settings_ex_err_msg(rc: i32) -> String {
        if rc != DISP_CHANGE_FAILED {
            format!("ret: {}", rc)
        } else {
            format!(
                "ret: {}, last error: {:?}",
                rc,
                std::io::Error::last_os_error()
            )
        }
    }

    fn set_primary_display(&mut self) -> ResultType<String> {
        // Multiple virtual displays with different origins are tested.
        let display = &self.virtual_displays[0];
        let display_name = std::string::String::from_utf16(&display.name)?;

        #[allow(invalid_value)]
        let mut new_primary_dm: DEVMODEW = unsafe { std::mem::MaybeUninit::uninit().assume_init() };
        new_primary_dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
        new_primary_dm.dmDriverExtra = 0;
        unsafe {
            if FALSE
                == EnumDisplaySettingsW(
                    display.name.as_ptr(),
                    ENUM_CURRENT_SETTINGS,
                    &mut new_primary_dm,
                )
            {
                bail!(
                    "Failed EnumDisplaySettingsW, device name: {:?}, error: {}",
                    std::string::String::from_utf16(&display.name),
                    Error::last_os_error()
                );
            }

            // Windows 24H2 requires the virtual display to be set first.
            // No idea why, maybe the same issue: https://developercommunity.visualstudio.com/t/Windows-11-Enterprise-24H2-using-WinApi/10851936?sort=newest
            let flags = CDS_UPDATEREGISTRY | CDS_NORESET;
            let offx = new_primary_dm.u1.s2().dmPosition.x;
            let offy = new_primary_dm.u1.s2().dmPosition.y;
            new_primary_dm.u1.s2_mut().dmPosition.x = 0;
            new_primary_dm.u1.s2_mut().dmPosition.y = 0;
            new_primary_dm.dmFields |= DM_POSITION;
            let rc = ChangeDisplaySettingsExW(
                display.name.as_ptr(),
                &mut new_primary_dm,
                NULL as _,
                flags | CDS_SET_PRIMARY,
                NULL,
            );
            if rc != DISP_CHANGE_SUCCESSFUL {
                let err = Self::change_display_settings_ex_err_msg(rc);
                log::error!(
                    "Failed ChangeDisplaySettingsEx, the virtual display, {}",
                    &err
                );
                bail!("Failed ChangeDisplaySettingsEx, {}", err);
            }

            let mut i: DWORD = 0;
            loop {
                #[allow(invalid_value)]
                let mut dd: DISPLAY_DEVICEW = std::mem::MaybeUninit::uninit().assume_init();
                dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as _;
                if FALSE
                    == EnumDisplayDevicesW(NULL as _, i, &mut dd, EDD_GET_DEVICE_INTERFACE_NAME)
                {
                    break;
                }
                i += 1;
                if (dd.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP) == 0 {
                    continue;
                }
                // Skip the virtual display.
                if dd.DeviceName == display.name {
                    continue;
                }

                #[allow(invalid_value)]
                let mut dm: DEVMODEW = std::mem::MaybeUninit::uninit().assume_init();
                dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
                dm.dmDriverExtra = 0;
                if FALSE
                    == EnumDisplaySettingsW(dd.DeviceName.as_ptr(), ENUM_CURRENT_SETTINGS, &mut dm)
                {
                    bail!(
                        "Failed EnumDisplaySettingsW, device name: {:?}, error: {}",
                        std::string::String::from_utf16(&dd.DeviceName),
                        Error::last_os_error()
                    );
                }

                dm.u1.s2_mut().dmPosition.x -= offx;
                dm.u1.s2_mut().dmPosition.y -= offy;
                dm.dmFields |= DM_POSITION;
                let rc = ChangeDisplaySettingsExW(
                    dd.DeviceName.as_ptr(),
                    &mut dm,
                    NULL as _,
                    flags,
                    NULL,
                );
                if rc != DISP_CHANGE_SUCCESSFUL {
                    let err = Self::change_display_settings_ex_err_msg(rc);
                    log::error!(
                        "Failed ChangeDisplaySettingsEx, device name: {:?}, flags: {}, {}",
                        std::string::String::from_utf16(&dd.DeviceName),
                        flags,
                        &err
                    );
                    bail!("Failed ChangeDisplaySettingsEx, {}", err);
                }

                // If we want to set dpi, the following references may be helpful.
                // And setting dpi should be called after changing the display settings.
                // https://stackoverflow.com/questions/35233182/how-can-i-change-windows-10-display-scaling-programmatically-using-c-sharp
                // https://github.com/lihas/windows-DPI-scaling-sample/blob/master/DPIHelper/DpiHelper.cpp
                //
                // But the official API does not provide a way to get/set dpi.
                // https://learn.microsoft.com/en-us/windows/win32/api/wingdi/ne-wingdi-displayconfig_device_info_type
                // https://github.com/lihas/windows-DPI-scaling-sample/blob/738ac18b7a7ce2d8fdc157eb825de9cb5eee0448/DPIHelper/DpiHelper.h#L37
            }
        }

        Ok(display_name)
    }

    // While two usbmmidd displays share the console desktop, DWM presents the whole desktop at
    // 5-8 fps even with nothing capturing it, so privacy mode has to leave exactly one online.
    // The surplus are unplugged rather than disabled: a disabled display keeps a 0x0 mode in the
    // registry and then vanishes from every display count, which is how the physical panel ends up
    // unaccounted for. If this overshoots to none, `ensure_virtual_display()` plugs a new one.
    fn plug_out_surplus_virtual_displays(&mut self) {
        if self.virtual_displays.len() <= 1 {
            return;
        }
        log::info!(
            "Privacy mode: plugging out {} extra virtual displays to keep a single display on the desktop",
            self.virtual_displays.len() - 1
        );
        let now = std::time::Instant::now();
        while self.virtual_displays.len() > 1
            && now.elapsed() < Duration::from_millis(VIRTUAL_DISPLAY_PLUG_OUT_WAIT_MILLIS)
        {
            let before = self.virtual_displays.len();
            // index 0 + force_one asks the driver for exactly one removal, whatever its own count.
            if let Err(e) = virtual_display_manager::plug_out_monitor(0, false, true) {
                log::error!("Privacy mode: failed to plug out a virtual display, {}", e);
                break;
            }
            let settle = std::time::Instant::now();
            while self.virtual_displays.len() >= before
                && settle.elapsed() < Duration::from_millis(VIRTUAL_DISPLAY_PLUG_OUT_SETTLE_MILLIS)
            {
                thread::sleep(Duration::from_millis(500));
                self.set_displays();
            }
            if self.virtual_displays.len() >= before {
                log::warn!("Privacy mode: the virtual display count did not drop, giving up");
                break;
            }
        }
    }

    // NOTE: We can't detect if the other virtual displays are physical displays or not.
    // We can only use `DeviceString` == `virtual_display_manager::get_cur_device_string()` to detect if the display is a virtual display.
    // The other virtual displays can't be restored after exiting the privacy mode on Windows 24H2.
    fn disable_physical_displays(&self) -> ResultType<()> {
        for display in &self.displays {
            let mut dm = display.dm.clone();
            unsafe {
                dm.u1.s2_mut().dmPosition.x = 10000;
                dm.u1.s2_mut().dmPosition.y = 10000;
                dm.dmPelsHeight = 0;
                dm.dmPelsWidth = 0;
                let flags = CDS_UPDATEREGISTRY | CDS_NORESET;
                let rc = ChangeDisplaySettingsExW(
                    display.name.as_ptr(),
                    &mut dm,
                    NULL as _,
                    flags,
                    NULL as _,
                );
                if rc != DISP_CHANGE_SUCCESSFUL {
                    let err = Self::change_display_settings_ex_err_msg(rc);
                    log::error!(
                        "Failed ChangeDisplaySettingsEx, device name: {:?}, flags: {}, {}",
                        std::string::String::from_utf16(&display.name),
                        flags,
                        &err
                    );
                    bail!("Failed ChangeDisplaySettingsEx, {}", err);
                }
            }
        }
        Ok(())
    }

    #[inline]
    fn default_display_modes() -> Vec<MonitorMode> {
        vec![MonitorMode {
            width: 1920,
            height: 1080,
            sync: 60,
        }]
    }

    // This function will wait at most 16 seconds for the virtual displays to be ready.
    // It's ok to wait, because:
    // 1. A new thread is created to handle the async privacy mode.
    // 2. The user is usually not in a hurry to turn on the privacy mode.
    pub fn ensure_virtual_display(&mut self, is_async_mode: bool) -> ResultType<()> {
        if self.virtual_displays.is_empty() {
            let displays =
                virtual_display_manager::plug_in_peer_request(vec![Self::default_display_modes()])?;
            if is_async_mode {
                thread::sleep(Duration::from_secs(1));
            }
            self.set_displays();
            // No physical displays, no need to use the privacy mode.
            if self.displays.is_empty() {
                self.recover_off_desktop_displays();
            }
            if self.displays.is_empty() {
                virtual_display_manager::plug_out_monitor_indices(&displays, false, false)?;
                bail!(NO_PHYSICAL_DISPLAYS);
            }

            if is_async_mode {
                let now = std::time::Instant::now();
                while self.virtual_displays.is_empty()
                    && now.elapsed() < Duration::from_millis(5000)
                {
                    thread::sleep(Duration::from_millis(500));
                    self.set_displays();
                }
            }

            // The driver can create the monitor long before the desktop takes it online, and it
            // stays invisible until something makes Windows re-poll the display devices.
            if is_async_mode && self.virtual_displays.is_empty() {
                allow_err!(Self::commit_change_display(CDS_RESET));
                let now = std::time::Instant::now();
                while self.virtual_displays.is_empty()
                    && now.elapsed() < Duration::from_millis(VIRTUAL_DISPLAY_RETRY_WAIT_MILLIS)
                {
                    thread::sleep(Duration::from_millis(500));
                    self.set_displays();
                }
            }

            self.virtual_displays_added.extend(displays);
        }

        Ok(())
    }

    #[inline]
    fn commit_change_display(flags: DWORD) -> ResultType<()> {
        unsafe {
            // use winapi::{
            //     shared::windef::HDESK,
            //     um::{
            //         processthreadsapi::GetCurrentThreadId,
            //         winnt::MAXIMUM_ALLOWED,
            //         winuser::{CloseDesktop, GetThreadDesktop, OpenInputDesktop, SetThreadDesktop},
            //     },
            // };
            // let mut desk_input: HDESK = NULL as _;
            // let desk_current: HDESK = GetThreadDesktop(GetCurrentThreadId());
            // if !desk_current.is_null() {
            //     desk_input = OpenInputDesktop(0, FALSE, MAXIMUM_ALLOWED);
            //     if desk_input.is_null() {
            //         SetThreadDesktop(desk_input);
            //     }
            // }

            let rc = ChangeDisplaySettingsExW(NULL as _, NULL as _, NULL as _, flags, NULL as _);
            if rc != DISP_CHANGE_SUCCESSFUL {
                let err = Self::change_display_settings_ex_err_msg(rc);
                bail!("Failed ChangeDisplaySettingsEx, {}", err);
            }

            // if !desk_current.is_null() {
            //     SetThreadDesktop(desk_current);
            // }
            // if !desk_input.is_null() {
            //     CloseDesktop(desk_input);
            // }
        }
        Ok(())
    }

    fn restore(&mut self) {
        // `set_primary_display()` moves the primary flag onto a virtual display, so a second
        // privacy mode captures that as the original state and the panel would never get the flag
        // back. Only corrected when no physical display claims it, to leave a normal multi-display
        // setup alone.
        if !self.displays.iter().any(|d| d.primary)
            && self.virtual_displays.iter().any(|d| d.primary)
        {
            if let Some(display) = self.displays.first_mut() {
                display.primary = true;
            }
        }
        Self::restore_displays(&self.displays);
        Self::restore_displays(&self.virtual_displays);
        allow_err!(Self::commit_change_display(0));
        self.displays.clear();
        self.virtual_displays.clear();
        let is_virtual_display_added = self.virtual_displays_added.len() > 0;
        if is_virtual_display_added {
            self.restore_plug_out_monitor();
        } else {
            // https://github.com/rustdesk/rustdesk/pull/12114#issuecomment-2983054370
            // No virtual displays added, we need to change the display combination to force the display settings to be reloaded.
            // Plugging every monitor out instead was how the privacy mode ended up taking away
            // virtual displays the user made by hand, and it left one behind that no control in the
            // UI can remove: with the physical panel disabled, "plug out" resolves to all-but-one.
            //
            // Nothing is committed here: `restore_displays()` above queued the real modes and
            // `commit_change_display(0)` applied them. `CDS_RESET` would throw that away and put
            // every display back at its persisted registry mode, which is still the 0x0 one privacy
            // mode wrote when it disabled the panel -- the panel then blacks just as we exit.

            // We can't replug the virtual dislays here.
            // TODO: plug out + plug in the virtual displays (`IDD_IMPL_AMYUNI`) in a short time makes the server side crash.
        }
    }

    fn restore_displays(displays: &[Display]) {
        for display in displays {
            let flags = if display.primary {
                CDS_NORESET | CDS_UPDATEREGISTRY | CDS_SET_PRIMARY
            } else {
                CDS_NORESET | CDS_UPDATEREGISTRY
            };
            let name = std::string::String::from_utf16_lossy(&display.name);
            unsafe {
                let mut dm = display.dm.clone();
                let rc = ChangeDisplaySettingsExW(
                    display.name.as_ptr(),
                    &mut dm,
                    std::ptr::null_mut(),
                    flags,
                    std::ptr::null_mut(),
                );
                if rc == DISP_CHANGE_SUCCESSFUL {
                    continue;
                }
                log::error!(
                    "Failed to restore display settings, device name: {:?}, {}",
                    &name,
                    Self::change_display_settings_ex_err_msg(rc)
                );

                // The mode captured before privacy mode may no longer be acceptable to the driver.
                // The registry mode is what Windows itself re-enables a display with, so the local
                // screen does not stay black just because privacy mode is over.
                #[allow(invalid_value)]
                let mut dm: DEVMODEW = std::mem::MaybeUninit::uninit().assume_init();
                dm.dmSize = std::mem::size_of::<DEVMODEW>() as _;
                dm.dmDriverExtra = 0;
                if FALSE
                    == EnumDisplaySettingsExW(
                        display.name.as_ptr(),
                        ENUM_REGISTRY_SETTINGS,
                        &mut dm,
                        0,
                    )
                {
                    log::error!(
                        "Failed to read the registry display settings, device name: {:?}, error: {}",
                        &name,
                        Error::last_os_error()
                    );
                    continue;
                }
                let rc = ChangeDisplaySettingsExW(
                    display.name.as_ptr(),
                    &mut dm,
                    std::ptr::null_mut(),
                    flags,
                    std::ptr::null_mut(),
                );
                if rc != DISP_CHANGE_SUCCESSFUL {
                    log::error!(
                        "Failed to restore display settings from the registry, device name: {:?}, {}",
                        &name,
                        Self::change_display_settings_ex_err_msg(rc)
                    );
                }
            }
        }
    }
}

impl PrivacyMode for PrivacyModeImpl {
    fn is_async_privacy_mode(&self) -> bool {
        virtual_display_manager::is_amyuni_idd()
    }

    fn init(&self) -> ResultType<()> {
        Ok(())
    }

    fn clear(&mut self) {
        allow_err!(self.turn_off_privacy(self.conn_id, None));
    }

    fn turn_on_privacy(&mut self, conn_id: i32) -> ResultType<bool> {
        if !virtual_display_manager::is_virtual_display_supported() {
            bail!("idd_not_support_under_win10_2004_tip");
        }

        if self.check_on_conn_id(conn_id)? {
            log::debug!("Privacy mode of conn {} is already on", conn_id);
            return Ok(true);
        }
        self.set_displays();
        if self.displays.is_empty() && !self.virtual_displays.is_empty() {
            // Nothing but virtual displays on the desktop means an earlier privacy mode never
            // brought the panel back. Without this the mode stays unenterable for good.
            self.recover_off_desktop_displays();
        }
        if self.displays.is_empty() {
            log::debug!("{}", NO_PHYSICAL_DISPLAYS);
            bail!(NO_PHYSICAL_DISPLAYS);
        }

        let is_async_mode = self.is_async_privacy_mode();
        let mut guard = TurnOnGuard {
            privacy_mode: self,
            succeeded: false,
        };

        guard.plug_out_surplus_virtual_displays();
        guard.ensure_virtual_display(is_async_mode)?;
        if guard.virtual_displays.is_empty() {
            Self::log_virtual_display_targets();
            log::debug!("No virtual displays");
            bail!("No virtual displays.");
        }

        let reg_connectivity_1 = reg_display_settings::read_reg_connectivity()?;
        let primary_display_name = guard.set_primary_display()?;
        guard.disable_physical_displays()?;
        Self::commit_change_display(CDS_RESET)?;
        // Explicitly set the resolution(virtual display) to 1920x1080.
        allow_err!(crate::platform::change_resolution(
            &primary_display_name,
            1920,
            1080
        ));
        let reg_connectivity_2 = reg_display_settings::read_reg_connectivity()?;

        if let Some(reg_recovery) =
            reg_display_settings::diff_recent_connectivity(reg_connectivity_1, reg_connectivity_2)
        {
            Config::set_option(
                CONFIG_KEY_REG_RECOVERY.to_owned(),
                serde_json::to_string(&reg_recovery)?,
            );
        } else {
            reset_config_reg_connectivity();
        };

        // OpenInputDesktop and block the others' input ?
        guard.conn_id = conn_id;
        guard.succeeded = true;

        allow_err!(super::win_input::hook());

        Ok(true)
    }

    fn turn_off_privacy(
        &mut self,
        conn_id: i32,
        state: Option<PrivacyModeState>,
    ) -> ResultType<()> {
        self.check_off_conn_id(conn_id)?;
        // A failed unhook must not skip restore() and the conn_id reset below: that left the panel
        // disabled with the mode still occupied by a connection that no longer existed.
        if let Err(e) = super::win_input::unhook() {
            log::error!("Privacy mode: failed to unhook the input, {}", e);
        }
        let _tmp_ignore_changed_holder = crate::display_service::temp_ignore_displays_changed();
        self.restore();
        // We need to force restore the registry connectivity.
        // This is because the registry connection may be changed by `self.restore()`, but will not be fully restored.
        restore_reg_connectivity(false, true);

        if self.conn_id != INVALID_PRIVACY_MODE_CONN_ID {
            if let Some(state) = state {
                allow_err!(super::set_privacy_mode_state(
                    conn_id,
                    state,
                    PRIVACY_MODE_IMPL.to_string(),
                    1_000
                ));
            }
            self.conn_id = INVALID_PRIVACY_MODE_CONN_ID.to_owned();
        }

        Ok(())
    }

    #[inline]
    fn pre_conn_id(&self) -> i32 {
        self.conn_id
    }

    #[inline]
    fn get_impl_key(&self) -> &str {
        &self.impl_key
    }
}

impl Drop for PrivacyModeImpl {
    fn drop(&mut self) {
        if self.conn_id != INVALID_PRIVACY_MODE_CONN_ID {
            allow_err!(self.turn_off_privacy(self.conn_id, None));
        }
    }
}

#[inline]
fn reset_config_reg_connectivity() {
    Config::set_option(CONFIG_KEY_REG_RECOVERY.to_owned(), "".to_owned());
}

pub fn restore_reg_connectivity(plug_out_monitors: bool, force: bool) {
    let config_recovery_value = Config::get_option(CONFIG_KEY_REG_RECOVERY);
    if config_recovery_value.is_empty() {
        return;
    }
    if plug_out_monitors {
        let _ = virtual_display_manager::plug_out_monitor(-1, true, false);
    }
    if let Ok(reg_recovery) =
        serde_json::from_str::<reg_display_settings::RegRecovery>(&config_recovery_value)
    {
        if let Err(e) = reg_display_settings::restore_reg_connectivity(reg_recovery, force) {
            log::error!("Failed restore_reg_connectivity, error: {}", e);
        }
    }
    reset_config_reg_connectivity();
}
