use crate::supervisor::{BridgeState, BridgeStatus};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tauri::{image::Image, menu::MenuItem, AppHandle, Runtime, Wry};

pub const TRAY_ID: &str = "main";
const PULSE_INTERVAL: Duration = Duration::from_millis(200);
const PULSE_LEVELS: [u8; 8] = [64, 96, 144, 208, 255, 208, 144, 96];
const COLORS: [[u8; 3]; 4] = [
    [14, 159, 110],  // connected
    [228, 155, 50],  // starting / reconnecting
    [211, 75, 75],   // action required
    [129, 144, 135], // inactive
];
const BASE_ICON: Image<'static> = tauri::include_image!("icons/32x32.png");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Palette {
    Connected = 0,
    Transitional = 1,
    Attention = 2,
    Inactive = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Presentation {
    palette: Palette,
    label: &'static str,
}

impl Presentation {
    fn for_state(state: &BridgeState) -> Self {
        match state {
            BridgeState::Connected => Self {
                palette: Palette::Connected,
                label: "Bridge connected",
            },
            BridgeState::Starting => Self {
                palette: Palette::Transitional,
                label: "Starting bridge",
            },
            BridgeState::Reconnecting => Self {
                palette: Palette::Transitional,
                label: "Reconnecting",
            },
            BridgeState::AgentUnavailable => Self {
                palette: Palette::Attention,
                label: "SSH agent unavailable",
            },
            BridgeState::NeedsPairing => Self {
                palette: Palette::Attention,
                label: "Pairing required",
            },
            BridgeState::Error => Self {
                palette: Palette::Attention,
                label: "Bridge needs attention",
            },
            BridgeState::Unpaired => Self {
                palette: Palette::Inactive,
                label: "Ready to pair",
            },
            BridgeState::Paused => Self {
                palette: Palette::Inactive,
                label: "Bridge paused",
            },
        }
    }
}

struct Inner<R: Runtime> {
    app: AppHandle<R>,
    status_item: MenuItem<R>,
    palette: AtomicU8,
    shutdown: AtomicBool,
    animation: Mutex<Option<JoinHandle<()>>>,
    blocking_shutdown: Mutex<()>,
    frames: Vec<Vec<Vec<u8>>>,
}

pub struct TrayStatus<R: Runtime = Wry>(Arc<Inner<R>>);

impl<R: Runtime> Clone for TrayStatus<R> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<R: Runtime> TrayStatus<R> {
    pub fn new(app: AppHandle<R>, status_item: MenuItem<R>, status: &BridgeStatus) -> Self {
        let frames = COLORS
            .iter()
            .map(|color| {
                PULSE_LEVELS
                    .iter()
                    .map(|level| recolor_status_tile(&BASE_ICON, *color, *level))
                    .collect()
            })
            .collect();
        let presentation = Presentation::for_state(&status.state);
        let tray_status = Self(Arc::new(Inner {
            app,
            status_item,
            palette: AtomicU8::new(presentation.palette as u8),
            shutdown: AtomicBool::new(false),
            animation: Mutex::new(None),
            blocking_shutdown: Mutex::new(()),
            frames,
        }));
        tray_status.update(status);
        tray_status
    }

    pub fn launch(&self) {
        let _shutdown = self
            .0
            .blocking_shutdown
            .lock()
            .expect("shutdown mutex poisoned");
        let mut animation = self.0.animation.lock().expect("animation mutex poisoned");
        if self.0.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let tray_status = self.clone();
        *animation = Some(thread::spawn(move || tray_status.animate()));
    }

    pub fn update(&self, status: &BridgeStatus) {
        let presentation = Presentation::for_state(&status.state);
        self.0
            .palette
            .store(presentation.palette as u8, Ordering::SeqCst);
        let _ = self
            .0
            .status_item
            .set_text(format!("Status: {}", presentation.label));
        if let Some(tray) = self.0.app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(format!("Mullion Helper — {}", presentation.label)));
        }
    }

    pub fn initial_icon(&self) -> Image<'_> {
        self.icon(0)
    }

    pub fn shutdown(&self) {
        self.0.shutdown.store(true, Ordering::SeqCst);
    }

    /// Waits for the last tray update to finish before Tauri tears down its
    /// event loop. Only call this from the updater's background callback.
    #[cfg(windows)]
    pub fn shutdown_for_update(&self) {
        let _shutdown = self
            .0
            .blocking_shutdown
            .lock()
            .expect("shutdown mutex poisoned");
        self.shutdown();
        let animation = self
            .0
            .animation
            .lock()
            .expect("animation mutex poisoned")
            .take();
        if let Some(animation) = animation {
            let _ = animation.join();
        }
    }

    fn animate(&self) {
        let mut frame = 0;
        while !self.0.shutdown.load(Ordering::SeqCst) {
            if let Some(tray) = self.0.app.tray_by_id(TRAY_ID) {
                let _ = tray.set_icon(Some(self.icon(frame)));
            }
            frame = (frame + 1) % PULSE_LEVELS.len();
            thread::sleep(PULSE_INTERVAL);
        }
    }

    fn icon(&self, frame: usize) -> Image<'_> {
        let palette = self.0.palette.load(Ordering::SeqCst) as usize;
        Image::new(
            &self.0.frames[palette][frame],
            BASE_ICON.width(),
            BASE_ICON.height(),
        )
    }
}

fn recolor_status_tile(base: &Image<'_>, color: [u8; 3], opacity: u8) -> Vec<u8> {
    let mut pixels = base.rgba().to_vec();
    let width = base.width() as usize;
    let height = base.height() as usize;
    for y in height / 2..height {
        for x in width / 2..width {
            let offset = (y * width + x) * 4;
            if pixels[offset + 3] == 0 {
                continue;
            }
            pixels[offset..offset + 3].copy_from_slice(&color);
            pixels[offset + 3] = ((u16::from(pixels[offset + 3]) * u16::from(opacity)) / 255) as u8;
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_bridge_state_to_the_expected_semantics() {
        let cases = [
            (
                BridgeState::Connected,
                Palette::Connected,
                "Bridge connected",
            ),
            (
                BridgeState::Starting,
                Palette::Transitional,
                "Starting bridge",
            ),
            (
                BridgeState::Reconnecting,
                Palette::Transitional,
                "Reconnecting",
            ),
            (
                BridgeState::AgentUnavailable,
                Palette::Attention,
                "SSH agent unavailable",
            ),
            (
                BridgeState::NeedsPairing,
                Palette::Attention,
                "Pairing required",
            ),
            (
                BridgeState::Error,
                Palette::Attention,
                "Bridge needs attention",
            ),
            (BridgeState::Unpaired, Palette::Inactive, "Ready to pair"),
            (BridgeState::Paused, Palette::Inactive, "Bridge paused"),
        ];

        for (state, palette, label) in cases {
            assert_eq!(
                Presentation::for_state(&state),
                Presentation { palette, label }
            );
        }
    }

    #[test]
    fn only_the_lower_right_status_tile_changes_between_frames() {
        let base = Image::new(
            &[1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
            2,
            2,
        );
        let dim = recolor_status_tile(&base, [100, 150, 200], 64);
        let bright = recolor_status_tile(&base, [100, 150, 200], 255);

        assert_eq!(&dim[..12], &base.rgba()[..12]);
        assert_eq!(&bright[..12], &base.rgba()[..12]);
        assert_ne!(&dim[12..], &bright[12..]);
    }

    #[test]
    fn status_tile_spans_the_selected_opacity_range_with_stable_rgb() {
        let base = Image::new(&[9, 8, 7, 255], 1, 1);
        let frames: Vec<_> = PULSE_LEVELS
            .iter()
            .map(|opacity| {
                recolor_status_tile(&base, COLORS[Palette::Connected as usize], *opacity)
            })
            .collect();

        assert_eq!(
            frames.iter().map(|frame| frame[3]).collect::<Vec<_>>(),
            PULSE_LEVELS
        );
        assert!(frames
            .iter()
            .all(|frame| frame[..3] == COLORS[Palette::Connected as usize]));
    }

    #[test]
    fn every_bridge_state_selects_its_intended_status_color() {
        let cases = [
            (BridgeState::Connected, COLORS[0]),
            (BridgeState::Starting, COLORS[1]),
            (BridgeState::Reconnecting, COLORS[1]),
            (BridgeState::AgentUnavailable, COLORS[2]),
            (BridgeState::NeedsPairing, COLORS[2]),
            (BridgeState::Error, COLORS[2]),
            (BridgeState::Unpaired, COLORS[3]),
            (BridgeState::Paused, COLORS[3]),
        ];

        for (state, color) in cases {
            let presentation = Presentation::for_state(&state);
            assert_eq!(COLORS[presentation.palette as usize], color);
        }
    }
}
