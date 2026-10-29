//! OBS window-helpers.c window_rating, without native handles.
use skylive_core::{WindowMatchPriority, WindowTarget};
/// OBS 32.2.2 chooses Auto from the class in the saved selector.
pub(crate) fn auto_uses_wgc(class: &str) -> bool {
    let class=class.to_ascii_lowercase();
    class.contains("chrome") || class.contains("mozilla") || [
        "ApplicationFrameWindow", "Windows.UI.Core.CoreWindow", "WinUIDesktopWin32WindowClass",
        "GAMINGSERVICESUI_HOSTING_WINDOW_CLASS", "XLMAIN", "PPTFrameClass", "screenClass",
        "PodiumParent", "OpusApp", "OMain", "Framework::CFrame", "rctrl_renwnd32",
        "MSWinPub", "OfficeApp-Frame", "WindowsWebview2Wrapper", "SDL_app", "WINDOWSCLIENT",
    ].iter().any(|expected|class.eq_ignore_ascii_case(expected))
}
fn compare(a: &str, b: &str) -> i32 {
    let mut a = a.bytes().map(|b| b.to_ascii_lowercase());
    let mut b = b.bytes().map(|b| b.to_ascii_lowercase());
    // OBS astrcmpi returns only -1/0/1, not the libc byte difference. Every
    // nonexact title therefore has the same distance; enumeration order wins.
    loop {
        let x = a.next().unwrap_or(0);
        let y = b.next().unwrap_or(0);
        if x != y || x == 0 {
            return match x.cmp(&y) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
        }
    }
}
pub(crate) fn windows_rating(
    target: &WindowTarget,
    candidate: &WindowTarget,
    mut priority: WindowMatchPriority,
) -> Option<u32> {
    let (
        WindowTarget::Windows {
            title,
            class,
            executable,
        },
        WindowTarget::Windows {
            title: ct,
            class: cc,
            executable: ce,
        },
    ) = (target, candidate)
    else {
        return None;
    };
    let generic = {
        let s = class.to_ascii_lowercase();
        s.contains("chrome") || s.contains("sdl_app")
    };
    if generic && priority == WindowMatchPriority::Class {
        priority = WindowMatchPriority::Title;
    }
    let title_diff = compare(ct, title).unsigned_abs();
    let exe = compare(ce, executable) == 0;
    let class_match = compare(cc, class) == 0;
    if matches!(
        class.as_str(),
        "Windows.UI.Core.CoreWindow" | "WinUIDesktopWin32WindowClass"
    ) {
        return (title_diff == 0 && (priority != WindowMatchPriority::Executable || exe))
            .then_some(0);
    }
    match priority {
        WindowMatchPriority::Title => (title_diff == 0).then_some(0),
        WindowMatchPriority::Class => {
            class_match.then_some(title_diff + if exe { 0 } else { 0x1000 })
        }
        WindowMatchPriority::Executable => exe.then_some(title_diff),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn target(t: &str, c: &str, e: &str) -> WindowTarget {
        WindowTarget::Windows {
            title: t.into(),
            class: c.into(),
            executable: e.into(),
        }
    }
    #[test]
    fn obs_priority_generic_and_uwp_rules_are_literal() {
        let a = target("Project A", "Editor", "editor.exe");
        assert_eq!(
            windows_rating(
                &a,
                &target("project a", "Other", "other.exe"),
                WindowMatchPriority::Title
            ),
            Some(0)
        );
        assert_eq!(
            windows_rating(
                &a,
                &target("Project B", "Editor", "editor.exe"),
                WindowMatchPriority::Class
            ),
            Some(1)
        );
        assert_eq!(
            windows_rating(
                &a,
                &target("Project A", "Editor", "other.exe"),
                WindowMatchPriority::Class
            ),
            Some(4096)
        );
        assert_eq!(
            windows_rating(
                &a,
                &target("Project A", "Other", "editor.exe"),
                WindowMatchPriority::Class
            ),
            None
        );
        assert_eq!(
            windows_rating(
                &target("A", "Chrome_WidgetWin_1", "x"),
                &target("B", "Chrome_WidgetWin_1", "x"),
                WindowMatchPriority::Class
            ),
            None
        );
        assert_eq!(
            windows_rating(
                &target("A", "WinUIDesktopWin32WindowClass", "x"),
                &target("B", "WinUIDesktopWin32WindowClass", "x"),
                WindowMatchPriority::Executable
            ),
            None
        );
        assert_eq!(
            windows_rating(
                &a,
                &target("B", "Other", "EDITOR.EXE"),
                WindowMatchPriority::Executable
            ),
            Some(1)
        );
    }
}
