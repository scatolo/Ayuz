// Ayuz - Unofficial Control Center for Asus Laptops
// Copyright (C) 2026 Guido Philipp
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see https://www.gnu.org/licenses/.

use evdev::{AbsoluteAxisCode, EventSummary, KeyCode};
use rust_i18n::t;
use std::sync::OnceLock;
use tokio::sync::watch;

use crate::services::commands::{is_gnome_desktop, is_kde_desktop};
use crate::services::evdev_runner::{find_touchpad, open_event_stream, touchpad_abs_bounds};
use crate::services::gnome_brightness;
use crate::services::kde_brightness;

/// Fraction of touchpad width/height that counts as an edge zone (4%)
const EDGE_PERCENT: f64 = 0.04;
/// Minimum movement in touchpad units required to trigger an action
const STEP_THRESHOLD: i32 = 300;

/// Tracks the current touch gesture as events arrive from the input device.
enum GestureState {
    /// No finger is currently touching the pad.
    Idle,
    /// Finger down - waiting for the first X and Y position to classify the gesture.
    Classifying { x: Option<i32>, y: Option<i32> },
    /// Touch started in the left edge zone; tracks volume via vertical movement.
    LeftEdge { last_y: i32 },
    /// Touch started in the right edge zone; tracks brightness via vertical movement.
    RightEdge { last_y: i32 },
    /// Touch started in the top edge zone; triggers media prev/next on horizontal movement.
    TopEdge { start_x: i32, done: bool },
    /// Touch started outside any edge zone - no action will be taken.
    Other,
}

/// Transitions a [`GestureState::Classifying`] state to the appropriate edge state once both
/// X and Y coordinates have been received for the initial touch position.
fn try_classify(state: &mut GestureState, left: i32, right: i32, top: i32) {
    if let GestureState::Classifying {
        x: Some(x),
        y: Some(y),
    } = *state
    {
        *state = if x < left {
            GestureState::LeftEdge { last_y: y }
        } else if x > right {
            GestureState::RightEdge { last_y: y }
        } else if y < top {
            GestureState::TopEdge {
                start_x: x,
                done: false,
            }
        } else {
            GestureState::Other
        };
    }
}

/// Spawns an external program asynchronously to perform a gesture action.
///
/// Failures are logged as warnings but do not propagate - this is a fire-and-forget call.
async fn run_action(program: &str, args: &[&str]) {
    let result = tokio::process::Command::new(program)
        .args(args)
        .status()
        .await;
    if let Err(e) = result {
        tracing::warn!(
            "{}",
            t!(
                "error_gesture_action",
                program = program,
                error = e.to_string()
            )
        );
    }
}

/// Returns true if `wpctl` is available on the system, caching the result.
fn has_wpctl() -> bool {
    static HAS_WPCTL: OnceLock<bool> = OnceLock::new();
    *HAS_WPCTL.get_or_init(|| {
        if std::path::Path::new("/usr/bin/wpctl").is_file() {
            return true;
        }
        std::process::Command::new("which")
            .arg("wpctl")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Returns the program and argument lists needed to adjust volume.
pub(crate) fn volume_commands(has_wpctl: bool, increase: bool) -> [(&'static str, &'static [&'static str]); 2] {
    match (has_wpctl, increase) {
        (true, true) => [
            ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]),
            ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+"]),
        ],
        (true, false) => [
            ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"]),
            ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"]),
        ],
        (false, true) => [
            ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"]),
            ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "+5%"]),
        ],
        (false, false) => [
            ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"]),
            ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "-5%"]),
        ],
    }
}

async fn adjust_volume(increase: bool) {
    for (prog, args) in volume_commands(has_wpctl(), increase) {
        run_action(prog, args).await;
    }
}

/// Main event loop for touchpad edge gesture detection.
///
/// Finds the touchpad device, reads its absolute-axis bounds, then processes `evdev` events
/// to recognise three gesture zones:
/// - **Left edge** (vertical swipe) → volume up/down via `pactl`
/// - **Right edge** (vertical swipe) → brightness up/down via `brightnessctl`
/// - **Top edge** (horizontal swipe) → media previous/next via `playerctl`
///
/// The loop exits cleanly when `shutdown` fires (the sender's value changes).
pub async fn run_gesture_loop(mut shutdown: watch::Receiver<bool>) {
    let device = match find_touchpad() {
        Some(d) => d,
        None => {
            tracing::warn!("{}", t!("error_no_touchpad"));
            return;
        }
    };

    let (x_max, y_max) = match touchpad_abs_bounds(&device) {
        Some(b) => b,
        None => return,
    };
    let left_bound = (x_max as f64 * EDGE_PERCENT) as i32;
    let right_bound = (x_max as f64 * (1.0 - EDGE_PERCENT)) as i32;
    let top_bound = (y_max as f64 * EDGE_PERCENT) as i32;

    let Some(mut stream) = open_event_stream(device) else {
        return;
    };

    let mut state = GestureState::Idle;

    loop {
        let event = tokio::select! {
            _ = shutdown.changed() => break,
            result = stream.next_event() => {
                match result {
                    Ok(ev) => ev,
                    Err(e) => {
                        tracing::warn!("{}", t!("error_event_read", error = e.to_string()));
                        break;
                    }
                }
            }
        };

        match event.destructure() {
            EventSummary::Key(_, KeyCode::BTN_TOUCH, value) => {
                if value == 1 {
                    // Palm rejection: a touch that starts while the user is
                    // typing never becomes a gesture.
                    state = if crate::services::typing_watch::gestures_suppressed() {
                        GestureState::Other
                    } else {
                        GestureState::Classifying { x: None, y: None }
                    };
                } else {
                    state = GestureState::Idle;
                }
            }
            EventSummary::AbsoluteAxis(
                _,
                AbsoluteAxisCode::ABS_X | AbsoluteAxisCode::ABS_MT_POSITION_X,
                value,
            ) => {
                if let GestureState::Classifying { x, .. } = &mut state {
                    *x = Some(value);
                    try_classify(&mut state, left_bound, right_bound, top_bound);
                } else if let GestureState::TopEdge { start_x, done } = &mut state {
                    if !*done {
                        let dx = value - *start_x;
                        if dx.abs() >= STEP_THRESHOLD {
                            *done = true;
                            if dx < 0 {
                                run_action("playerctl", &["previous"]).await;
                            } else {
                                run_action("playerctl", &["next"]).await;
                            }
                        }
                    }
                }
            }
            EventSummary::AbsoluteAxis(
                _,
                AbsoluteAxisCode::ABS_Y | AbsoluteAxisCode::ABS_MT_POSITION_Y,
                value,
            ) => {
                if let GestureState::Classifying { y, .. } = &mut state {
                    *y = Some(value);
                    try_classify(&mut state, left_bound, right_bound, top_bound);
                } else {
                    match &mut state {
                        GestureState::LeftEdge { last_y } => {
                            let dy = value - *last_y;
                            if dy.abs() >= STEP_THRESHOLD {
                                *last_y = value;
                                adjust_volume(dy < 0).await;
                            }
                        }
                        GestureState::RightEdge { last_y } => {
                            let dy = value - *last_y;
                            if dy.abs() >= STEP_THRESHOLD {
                                *last_y = value;
                                let delta = if dy < 0 { 5 } else { -5 };
                                let arg = if delta > 0 { "5%+" } else { "5%-" };
                                if is_kde_desktop() {
                                    if let Err(e) =
                                        kde_brightness::adjust_brightness_relative(delta).await
                                    {
                                        tracing::warn!(
                                            "PowerDevil setBrightness failed, falling back to brightnessctl: {e}"
                                        );
                                        run_action("brightnessctl", &["set", arg]).await;
                                    }
                                } else if is_gnome_desktop() {
                                    if let Err(e) =
                                        gnome_brightness::adjust_brightness_relative(delta).await
                                    {
                                        tracing::warn!(
                                            "GNOME brightness adjustment failed, falling back to brightnessctl: {e}"
                                        );
                                        run_action("brightnessctl", &["set", arg]).await;
                                    }
                                } else {
                                    run_action("brightnessctl", &["set", arg]).await;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            EventSummary::Key(
                _,
                KeyCode::BTN_TOOL_DOUBLETAP | KeyCode::BTN_TOOL_TRIPLETAP,
                1,
            ) => {
                state = GestureState::Other;
            }
            EventSummary::AbsoluteAxis(_, AbsoluteAxisCode::ABS_MT_SLOT, value) if value > 0 => {
                state = GestureState::Other;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_volume_commands_wpctl() {
        let up = volume_commands(true, true);
        assert_eq!(up[0], ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"][..]));
        assert_eq!(up[1], ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%+" ][..]));

        let down = volume_commands(true, false);
        assert_eq!(down[0], ("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "0"][..]));
        assert_eq!(down[1], ("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-" ][..]));
    }

    #[test]
    fn test_volume_commands_pactl() {
        let up = volume_commands(false, true);
        assert_eq!(up[0], ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"][..]));
        assert_eq!(up[1], ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "+5%"][..]));

        let down = volume_commands(false, false);
        assert_eq!(down[0], ("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "0"][..]));
        assert_eq!(down[1], ("pactl", &["set-sink-volume", "@DEFAULT_SINK@", "-5%"][..]));
    }

    #[test]
    fn test_try_classify() {
        let mut state = GestureState::Classifying { x: Some(30), y: Some(500) };
        try_classify(&mut state, 100, 900, 100);
        match state {
            GestureState::LeftEdge { last_y } => assert_eq!(last_y, 500),
            _ => panic!("Expected LeftEdge"),
        }

        let mut state = GestureState::Classifying { x: Some(950), y: Some(500) };
        try_classify(&mut state, 100, 900, 100);
        match state {
            GestureState::RightEdge { last_y } => assert_eq!(last_y, 500),
            _ => panic!("Expected RightEdge"),
        }

        let mut state = GestureState::Classifying { x: Some(500), y: Some(50) };
        try_classify(&mut state, 100, 900, 100);
        match state {
            GestureState::TopEdge { start_x, done } => {
                assert_eq!(start_x, 500);
                assert!(!done);
            }
            _ => panic!("Expected TopEdge"),
        }

        let mut state = GestureState::Classifying { x: Some(500), y: Some(500) };
        try_classify(&mut state, 100, 900, 100);
        match state {
            GestureState::Other => {}
            _ => panic!("Expected Other"),
        }
    }
}
