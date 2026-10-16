//! Put the user back where they were: remember the frontmost app
//! before the editor opens, bring it back when the edit ends.

#[cfg(target_os = "macos")]
pub(super) async fn frontmost() -> Option<String> {
    super::macos::osascript(
        "tell application \"System Events\" to bundle identifier of \
         first process whose frontmost is true",
    ).await.ok().filter(|id| !id.is_empty())
}

#[cfg(target_os = "macos")]
pub(super) async fn restore(bundle_id: &str) {
    let _ = tokio::process::Command::new("open").arg("-b").arg(bundle_id)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status().await;
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn frontmost() -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn restore(_bundle_id: &str) {}
