//! Pure portion of OBS 32.2.2 window rating; native enumeration is a separate
//! boundary because HWNDs and process leases must never enter persistence.
use skylive_core::{WindowsAudioWindowPriority as Priority,WindowsAudioWindowSelector};

pub(super) struct Candidate<'a,T>{pub handle:T,pub title:&'a str,pub class:&'a str,pub executable:Option<&'a str>}
pub(super) enum Best<T>{Found(T),Unavailable,None}

/// Enumeration order is significant: equal scores retain the earlier window.
/// OBS astrcmpi returns only -1/0/1, so title mismatch scores 0 or 1.
/// Unknown executables participate with their best possible score: if they
/// could win, refuse rather than silently capture a different process.
/// Matching is ASCII-insensitive; full OBS locale/Unicode parity is not claimed.
pub(super) fn best<'a,T>(selector:&WindowsAudioWindowSelector,candidates:impl IntoIterator<Item=Candidate<'a,T>>)->Best<T>{
    if selector.title().is_empty()&&selector.class().is_empty()&&selector.executable().is_empty(){return Best::None;}
    let requested_class=selector.class();
    let uwp=matches!(requested_class,"Windows.UI.Core.CoreWindow"|"WinUIDesktopWin32WindowClass");
    let generic=requested_class.to_ascii_lowercase().contains("chrome")||requested_class.to_ascii_lowercase().contains("sdl_app");
    let priority=if generic&&selector.priority()==Priority::Class{Priority::Title}else{selector.priority()};
    let mut result=None;let mut unavailable=false;let mut best_score=i32::MAX;
    for candidate in candidates {
        let title=if candidate.title.eq_ignore_ascii_case(selector.title()){0}else{1};
        let class=candidate.class.eq_ignore_ascii_case(requested_class);
        let exe=candidate.executable.is_none_or(|value|value.eq_ignore_ascii_case(selector.executable()));
        let score=if uwp {if (priority==Priority::Executable&&!exe)||title!=0{i32::MAX}else{0}}
            else {match priority {
                Priority::Title=>if title==0{0}else{i32::MAX},
                Priority::Class=>if class{title+if exe{0}else{0x1000}}else{i32::MAX},
                Priority::Executable=>if exe{title}else{i32::MAX},
            }};
        if score<best_score {
            best_score=score;
            if candidate.executable.is_none(){unavailable=true;result=None;}
            else{unavailable=false;result=Some(candidate.handle);}
            if score==0{break;}
        }
    }
    if unavailable{Best::Unavailable}else if let Some(result)=result{Best::Found(result)}else{Best::None}
}
