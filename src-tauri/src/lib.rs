pub fn run() {
    // WebKitGTK's DMA-BUF renderer crashes the window on some Wayland + NVIDIA
    // setups ("Error 71 (Protocol error) dispatching to Wayland display").
    // Respect an explicit setting from the user's environment.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running Linux_Util Desktop");
}
