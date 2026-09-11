use crate::supervisor::{BridgeState, BridgeStatus};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
use tauri::{image::Image, menu::MenuItem, AppHandle, Wry};

pub const TRAY_ID: &str = "main";
const PULSE_INTERVAL: Duration = Duration::from_millis(300);
const PULSE_LEVELS: [u8; 8] = [170, 190, 215, 240, 255, 240, 215, 190];
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

struct Inner {
    app: AppHandle,
    status_item: MenuItem<Wry>,
    palette: AtomicU8,
    shutdown: AtomicBool,
    frames: Vec<Vec<Vec<u8>>>,
}

#[derive(Clone)]
pub struct TrayStatus(Arc<Inner>);

impl TrayStatus {
    pub fn new(app: AppHandle, status_item: MenuItem<Wry>, status: &BridgeStatus) -> Self {
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
            frames,
        }));
        tray_status.update(status);
        tray_status
    }

    pub fn launch(&self) {
        let tray_status = self.clone();
        thread::spawn(move || tray_status.animate());
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

fn recolor_status_tile(base: &Image<'_>, color: [u8; 3], level: u8) -> Vec<u8> {
    let mut pixels = base.rgba().to_vec();
    let width = base.width() as usize;
    let height = base.height() as usize;
    for y in height / 2..height {
        for x in width / 2..width {
            let offset = (y * width + x) * 4;
            if pixels[offset + 3] == 0 {
                continue;
            }
            for channel in 0..3 {
                pixels[offset + channel] =
                    ((u16::from(color[channel]) * u16::from(level)) / 255) as u8;
            }
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
    fn recolors_only_the_opaque_part_of_the_lower_right_tile() {
        let base = Image::new(
            &[1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
            2,
            2,
        );
        let recolored = recolor_status_tile(&base, [100, 150, 200], 255);

        assert_eq!(&recolored[..12], &base.rgba()[..12]);
        assert_eq!(&recolored[12..], &[100, 150, 200, 255]);
    }
}
