use playit_api_client::api::Platform;

pub fn get_platform() -> Platform {
    #[cfg(target_os = "windows")]
    return Platform::Windows;

    #[cfg(target_os = "linux")]
    return Platform::Linux;

    #[cfg(target_os = "freebsd")]
    return Platform::Freebsd;

    #[cfg(target_os = "macos")]
    return Platform::Macos;

    #[cfg(target_os = "android")]
    return Platform::Android;

    #[cfg(target_os = "ios")]
    return Platform::Ios;

    #[allow(unreachable_code)]
    Platform::Unknown
}
