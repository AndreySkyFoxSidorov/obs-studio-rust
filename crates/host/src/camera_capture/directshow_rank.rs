//! OBS DirectShow capability ranking for the default video configuration.
//! All values are bounded before arithmetic; the native caller owns media types.

#[derive(Clone, Copy)]
pub(super) struct Request { pub width: i32, pub height: i32, pub flipped: bool, pub interval: i64 }

#[derive(Clone, Copy)]
pub(super) struct Capability {
    pub min_width: i32, pub max_width: i32, pub min_height: i32, pub max_height: i32,
    pub step_width: i32, pub step_height: i32, pub min_interval: i64, pub max_interval: i64,
    pub format_rating: i64,
}

#[derive(Clone, Copy)]
pub(super) struct Choice { pub score: i64, pub width: i32, pub height: i32, pub interval: i64 }

/// Mirrors `ClosestVideoMTCallback`: distance to each supported range plus
/// the format preference, with the first minimum winning. The caller retains
/// capability enumeration order, as OBS does. Invalid driver data is rejected.
pub(super) fn choose(request: Request, cap: Capability, current_interval: i64) -> Option<Choice> {
    if request.width <= 0 || request.height <= 0 || request.interval <= 0
        || cap.min_width <= 0 || cap.min_height == 0 || cap.max_height == 0 || cap.max_width < cap.min_width
        || cap.step_width <= 0 || cap.step_height <= 0
        || cap.min_interval <= 0 || cap.max_interval < cap.min_interval || cap.format_rating < 0 {
        return None;
    }
    let distance = |v:i64, low:i64, high:i64| if v<low {low-v} else if v>high {v-high} else {0};
    let x = distance(i64::from(request.width),i64::from(cap.min_width),i64::from(cap.max_width));
    // OBS compares absolute endpoints but aligns using the original signed
    // minCY. Preserve its branch order even for reversed absolute endpoints.
    let min_height=i64::from(cap.min_height).abs();
    let max_height=i64::from(cap.max_height).abs();
    let requested_height=i64::from(request.height);
    let y = if requested_height<min_height {min_height-requested_height}
        else if requested_height>max_height {requested_height-max_height} else {0};
    let f = distance(request.interval,cap.min_interval,cap.max_interval);
    let score = x.checked_add(y)?.checked_add(f)?.checked_add(cap.format_rating)?;
    // If outside a capability range OBS leaves the candidate's own values in
    // place; these sentinels tell the caller which fields must remain intact.
    let width = if x==0 {request.width - (request.width-cap.min_width)%cap.step_width} else {0};
    let height = if y==0 {
        let h=requested_height-(requested_height-i64::from(cap.min_height))%i64::from(cap.step_height);
        let h=if request.flipped {-h} else {h};
        i32::try_from(h).ok()?
    } else {0};
    let interval = if f==0 && current_interval.abs_diff(request.interval)>1 {request.interval} else {current_interval};
    Some(Choice {score,width,height,interval})
}
