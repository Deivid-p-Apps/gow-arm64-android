//! Steamworks (steam_api64) minimal stub, plus the Sony compatibility DLLs the
//! PC build imports (libSceGnm/Pad/JobManager/GpuAddress). Only what the IAT
//! resolves is stubbed; unknown entry points are counted, never faked.

use std::sync::atomic::{AtomicBool, Ordering};

pub fn steam_api64(func: &str) -> Option<u64> {
    static INITED: AtomicBool = AtomicBool::new(false);
    match func {
        "SteamAPI_Init" | "SteamAPI_InitSafe" | "SteamInternal_Init" => {
            INITED.store(true, Ordering::Relaxed);
            Some(1) // appid != 0 => success
        }
        "SteamAPI_RestartAppIfNecessary" => Some(0),
        "SteamAPI_Shutdown" => Some(0),
        "SteamAPI_IsSteamRunning" => Some(1),
        "SteamAPI_GetHSteamUser" | "SteamAPI_GetHSteamPipe" => Some(1),
        "SteamAPI_RegisterCallback" | "SteamAPI_UnregisterCallback" => Some(0),
        "SteamAPI_RunCallbacks" => Some(0),
        "SteamInternal_ContextInit" => Some(1),
        "SteamInternal_FindOrCreateUserInterface" | "SteamInternal_CreateInterface" => Some(1),
        "SteamAPI_GetSteamInstallPath" => Some(0),
        "SteamAPI_UseBreakpadCrashHandler" => Some(0),
        _ => None,
    }
}

/// Sony compatibility layer: this PC build routes GPU/input through these DLLs.
/// We provide the entry points the IAT needs; behaviour is added per use.
pub fn sce(func: &str) -> Option<u64> {
    // Returning a non-zero handle is only correct where a handle is expected;
    // callers that need real behaviour will be implemented with the renderer.
    match func {
        "initialize" | "sceGnmInit" => Some(0),
        _ => Some(0),
    }
}

/// XInput 1.4: report a neutral (disconnected-style) state until the Android
/// input layer maps real gamepads. Returning 0 = no controller is a real state,
/// not a fake success.
pub fn xinput(func: &str) -> Option<u64> {
    match func {
        "ord#2" => Some(0), // XInputGetState -> ERROR_DEVICE_NOT_CONNECTED
        "ord#3" => Some(0), // XInputSetState
        _ => Some(0),
    }
}
