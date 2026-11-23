//! Unified Stacked HUD Manager (`Volume`, `Toast`, `Progress`, `Confirmation`).
//!
//! All HUD cards share one vertical stack (`index 0` = topmost / newest slot at `REST_Y = 22.0`)
//! with deterministic delta-time choreography:
//! - Every new arrival enters at `index 0` via top slide-in while pushing existing cards smoothly downward.
//! - Singleton `Volume` updates in place at `index 0` or promotes smoothly from `index > 0` back to `index 0`.
//! - Identical `Toast` keys coalesce and reset hold timers (promoting to `index 0` if lower).
//! - Topmost (`index == 0`) dismissals exit upward (`SlideUpAndFade`); lower (`index > 0`) dismissals
//!   exit strictly in place (`FadeInPlace`); remaining items below smoothly reflow upward.

use crate::layout::{hud_stack_spec as hs, hud_toast_spec as ht, volume_popup_spec as vp};

#[derive(Clone, Debug, PartialEq)]
pub enum HudCardPayload {
    Volume {
        level_norm: f32,
    },
    Toast {
        key: String,
        line1: String,
        line2: String,
    },
    Progress {
        handle: u32,
        title: String,
        detail: String,
        /// `Some(0.0..=1.0)` for determinate bar, `None` for indeterminate animated shuttle.
        progress: Option<f32>,
    },
    Confirmation {
        id: u32,
        question: String,
        detail: String,
        confirm_label: String,
        cancel_label: String,
    },
}

impl HudCardPayload {
    #[inline]
    pub fn height(&self) -> f32 {
        match self {
            Self::Volume { .. } => vp::HEIGHT,
            Self::Toast { line2, .. } => {
                if line2.is_empty() {
                    ht::HEIGHT
                } else {
                    hs::TOAST_TWO_LINE_HEIGHT
                }
            }
            Self::Progress { .. } => hs::PROGRESS_HEIGHT,
            Self::Confirmation { detail, .. } => {
                if detail.is_empty() {
                    hs::CONFIRM_HEIGHT_SINGLE
                } else {
                    hs::CONFIRM_HEIGHT_DETAIL
                }
            }
        }
    }

    #[inline]
    pub fn is_toast(&self) -> bool {
        matches!(self, Self::Toast { .. })
    }

    #[inline]
    pub fn is_volume(&self) -> bool {
        matches!(self, Self::Volume { .. })
    }

    #[inline]
    pub fn is_confirm(&self) -> bool {
        matches!(self, Self::Confirmation { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HudExitMode {
    /// Used exclusively when dismissing from `index == 0`: upward slide off-screen + fade.
    SlideUpAndFade { start_y: f32, hidden_y: f32 },
    /// Used strictly when dismissing from `index > 0`: stationary in-place fade-out (`y` constant).
    FadeInPlace { fixed_y: f32 },
}

#[derive(Clone, Debug)]
struct ActiveHudCard {
    payload: HudCardPayload,
    open_ms: u64,
    /// `Some(deadline_ms)` for auto-dismissing cards (`Volume`, `Toast`); `None` for `Progress`/`Confirmation`.
    hold_until_ms: Option<u64>,
    slot_y: f32,
    target_slot_y: f32,
}

impl ActiveHudCard {
    #[inline]
    fn hidden_y(&self) -> f32 {
        -(self.payload.height() + hs::HIDDEN_TOP_PAD)
    }

    #[inline]
    fn enter_progress(&self, now_ms: u64) -> f32 {
        let el = now_ms.saturating_sub(self.open_ms) as f32;
        let u = (el / hs::SLIDE_IN_MS).clamp(0.0, 1.0);
        1.0 - (1.0 - u).powi(3)
    }

    #[inline]
    fn rendered_y(&self, now_ms: u64) -> f32 {
        let ease_in = self.enter_progress(now_ms);
        let enter_offset = (self.hidden_y() - hs::REST_Y) * (1.0 - ease_in);
        self.slot_y + enter_offset
    }
}

#[derive(Clone, Debug)]
struct ExitingHudCard {
    payload: HudCardPayload,
    exit_start_ms: u64,
    start_opacity: f32,
    mode: HudExitMode,
}

impl ExitingHudCard {
    fn sample(&self, now_ms: u64) -> Option<(f32, f32)> {
        let el = now_ms.saturating_sub(self.exit_start_ms) as f32;
        match self.mode {
            HudExitMode::SlideUpAndFade { start_y, hidden_y } => {
                if el >= hs::EXIT_TOP_MS {
                    return None;
                }
                let u = (el / hs::EXIT_TOP_MS).clamp(0.0, 1.0);
                let y = start_y + (hidden_y - start_y) * u.powi(3);
                let opacity = (self.start_opacity * (1.0 - u * u)).clamp(0.0, 1.0);
                Some((y, opacity))
            }
            HudExitMode::FadeInPlace { fixed_y } => {
                if el >= hs::EXIT_FADE_MS {
                    return None;
                }
                let u = (el / hs::EXIT_FADE_MS).clamp(0.0, 1.0);
                let opacity = (self.start_opacity * (1.0 - u * (2.0 - u))).clamp(0.0, 1.0);
                Some((fixed_y, opacity))
            }
        }
    }
}

/// Snapshot of a single visible or exiting card ready to be drawn by `UiEngine`.
#[derive(Clone, Debug, PartialEq)]
pub struct HudRenderItem {
    pub payload: HudCardPayload,
    pub y: f32,
    pub opacity: f32,
    pub is_exiting: bool,
    pub exit_mode: Option<HudExitMode>,
}

/// Normalizes raw C/Rockbox splash text into `(line1, line2)` while stripping control escapes.
pub fn normalize_toast_lines(raw: &str) -> (String, String) {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '\x0C' | '\r' | '\x0B' | '\x08' => ' ',
            '\t' => ' ',
            other => other,
        })
        .collect();
    let mut parts = cleaned
        .split('\n')
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let first = parts.next().unwrap_or("").to_string();
    let rest: Vec<&str> = parts.collect();
    let second = if rest.is_empty() {
        String::new()
    } else {
        rest.join(" · ")
    };
    (first, second)
}

#[derive(Clone, Debug)]
pub struct HudStack {
    active: Vec<ActiveHudCard>,
    exiting: Vec<ExitingHudCard>,
    last_step_ms: Option<u64>,
    clock_ms: u64,
    next_id: u32,
}

impl Default for HudStack {
    fn default() -> Self {
        Self {
            active: Vec::new(),
            exiting: Vec::new(),
            last_step_ms: None,
            clock_ms: 0,
            next_id: 1,
        }
    }
}

impl HudStack {
    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    #[inline]
    pub fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    pub fn set_clock(&mut self, now_ms: u64) {
        self.step(now_ms);
    }

    fn recompute_target_slots(&mut self) {
        let mut cursor_y = hs::REST_Y;
        for card in &mut self.active {
            card.target_slot_y = cursor_y;
            cursor_y += card.payload.height() + hs::CARD_GAP_Y;
        }
    }

    fn dismiss_active_at(&mut self, index: usize, exit_start_ms: u64) {
        if index >= self.active.len() {
            return;
        }
        let removed = self.active.remove(index);
        let cur_y = removed.rendered_y(exit_start_ms);
        let mode = if index == 0 {
            HudExitMode::SlideUpAndFade {
                start_y: cur_y,
                hidden_y: removed.hidden_y(),
            }
        } else {
            HudExitMode::FadeInPlace { fixed_y: cur_y }
        };
        self.exiting.push(ExitingHudCard {
            payload: removed.payload,
            exit_start_ms,
            start_opacity: 1.0,
            mode,
        });
        self.recompute_target_slots();
    }

    fn enforce_capacity(&mut self, now_ms: u64) {
        while self.active.len() > hs::MAX_VISIBLE_CARDS {
            // Expire oldest Toast (`index > 0` -> strictly FadeInPlace); never drop Progress/Confirmation/Volume.
            if let Some(oldest_toast_idx) = self.active.iter().rposition(|c| c.payload.is_toast()) {
                let idx = if oldest_toast_idx == 0 && self.active.len() > 1 {
                    // Never drop a brand-new index 0 toast unless every lower card is also non-toast;
                    // if forced, still fade in place cleanly.
                    oldest_toast_idx
                } else {
                    oldest_toast_idx
                };
                if idx == 0 {
                    let removed = self.active.remove(0);
                    let cur_y = removed.rendered_y(now_ms);
                    self.exiting.push(ExitingHudCard {
                        payload: removed.payload,
                        exit_start_ms: now_ms,
                        start_opacity: 1.0,
                        mode: HudExitMode::FadeInPlace { fixed_y: cur_y },
                    });
                    self.recompute_target_slots();
                } else {
                    self.dismiss_active_at(idx, now_ms);
                }
            } else {
                break;
            }
        }
    }

    /// Converts an `open_ms` timestamp so a revived mid-exit card continues seamlessly from `cur_y`.
    fn open_ms_from_current_y(cur_y: f32, hidden_y: f32, now_ms: u64) -> u64 {
        let span = (hs::REST_Y - hidden_y).max(1.0);
        let prog = ((cur_y - hidden_y) / span).clamp(0.0, 1.0);
        if prog >= 0.995 {
            now_ms.saturating_sub(hs::SLIDE_IN_MS as u64)
        } else if prog > 0.001 {
            let u = (1.0 - (1.0 - prog).cbrt()).clamp(0.0, 1.0);
            now_ms.saturating_sub((u * hs::SLIDE_IN_MS).round() as u64)
        } else {
            now_ms
        }
    }

    fn advance_reflow(&mut self, dt_ms: f32) {
        if dt_ms <= 0.0 {
            return;
        }
        let alpha = 1.0 - (-dt_ms / hs::REFLOW_TAU_MS).exp();
        for card in &mut self.active {
            let diff = card.target_slot_y - card.slot_y;
            if diff.abs() <= 0.05 {
                card.slot_y = card.target_slot_y;
            } else {
                card.slot_y += diff * alpha;
            }
        }
    }

    /// Advances stack timers, reflow springs, and exit animations to `now_ms`.
    pub fn step(&mut self, now_ms: u64) {
        self.clock_ms = now_ms;
        let mut cursor_ms = self.last_step_ms.unwrap_or(now_ms);
        self.last_step_ms = Some(now_ms);

        // Process any auto-dismiss deadlines inside `(cursor_ms..=now_ms]` in chronological order
        // so upward reflow of lower cards begins at the exact `deadline` timestamp!
        loop {
            let next_exp = self
                .active
                .iter()
                .filter_map(|c| c.hold_until_ms)
                .filter(|&dl| dl <= now_ms)
                .min();
            let Some(dl) = next_exp else {
                break;
            };
            if dl > cursor_ms {
                self.advance_reflow((dl - cursor_ms) as f32);
                cursor_ms = dl;
            }
            if let Some(idx) = self
                .active
                .iter()
                .position(|c| c.hold_until_ms == Some(dl))
            {
                self.dismiss_active_at(idx, dl);
            } else {
                break;
            }
        }

        if now_ms > cursor_ms {
            self.advance_reflow((now_ms - cursor_ms) as f32);
        }

        // Prune completed exit animations.
        self.exiting.retain(|ex| ex.sample(now_ms).is_some());

        if self.active.is_empty() && self.exiting.is_empty() {
            self.last_step_ms = None;
        }
    }

    /// Triggers or updates the singleton `Volume` card according to Section 3.1.
    pub fn trigger_volume(&mut self, level_norm: f32, now_ms: u64) {
        self.step(now_ms);
        let clamped = level_norm.clamp(0.0, 1.0);
        let hold_until = Some(now_ms.saturating_add(hs::DEFAULT_HOLD_MS));

        // Case 1: Active Volume card already in stack
        if let Some(idx) = self.active.iter().position(|c| c.payload.is_volume()) {
            if idx == 0 {
                // Already at index 0: update level & extend hold in place with zero jump
                self.active[0].payload = HudCardPayload::Volume {
                    level_norm: clamped,
                };
                self.active[0].hold_until_ms = hold_until;
            } else {
                // At index > 0: promote smoothly back to index 0, sliding 0..idx-1 down by one slot
                let mut card = self.active.remove(idx);
                card.payload = HudCardPayload::Volume {
                    level_norm: clamped,
                };
                card.hold_until_ms = hold_until;
                self.active.insert(0, card);
                self.recompute_target_slots();
            }
            return;
        }

        // Case 2: Revive a Volume card that is mid-exit at the top if stack is otherwise empty
        if self.active.is_empty() {
            if let Some(ex_idx) = self.exiting.iter().position(|e| e.payload.is_volume()) {
                let ex = self.exiting.remove(ex_idx);
                if let Some((cur_y, _)) = ex.sample(now_ms) {
                    let hidden_y = -(vp::HEIGHT + hs::HIDDEN_TOP_PAD);
                    let open_ms = Self::open_ms_from_current_y(cur_y, hidden_y, now_ms);
                    self.active.insert(
                        0,
                        ActiveHudCard {
                            payload: HudCardPayload::Volume {
                                level_norm: clamped,
                            },
                            open_ms,
                            hold_until_ms: hold_until,
                            slot_y: hs::REST_Y,
                            target_slot_y: hs::REST_Y,
                        },
                    );
                    self.recompute_target_slots();
                    return;
                }
            }
        } else {
            // Remove any stale exiting Volume ghost so singleton visual invariant holds
            self.exiting.retain(|e| !e.payload.is_volume());
        }

        // Case 3: Spawn fresh Volume card at index 0, smoothly pushing existing cards down
        self.active.insert(
            0,
            ActiveHudCard {
                payload: HudCardPayload::Volume {
                    level_norm: clamped,
                },
                open_ms: now_ms,
                hold_until_ms: hold_until,
                slot_y: hs::REST_Y,
                target_slot_y: hs::REST_Y,
            },
        );
        self.recompute_target_slots();
        self.enforce_capacity(now_ms);
    }

    /// Triggers or coalesces a non-blocking `Toast` card according to Section 3.2.
    pub fn trigger_toast(&mut self, key: &str, message: &str, hold_ms: u64, now_ms: u64) {
        let (line1, line2) = normalize_toast_lines(message);
        if line1.is_empty() {
            return;
        }
        self.step(now_ms);

        let eff_key = if key.trim().is_empty() {
            if line2.is_empty() {
                line1.clone()
            } else {
                format!("{line1}\n{line2}")
            }
        } else {
            key.trim().to_string()
        };
        let eff_hold = if hold_ms == 0 {
            hs::DEFAULT_HOLD_MS
        } else {
            hold_ms.clamp(600, 5000)
        };
        let hold_until = Some(now_ms.saturating_add(eff_hold));

        // Coalesce matching active toast key
        if let Some(idx) = self.active.iter().position(
            |c| matches!(&c.payload, HudCardPayload::Toast { key: k, .. } if k == &eff_key),
        ) {
            if idx == 0 {
                self.active[0].payload = HudCardPayload::Toast {
                    key: eff_key,
                    line1,
                    line2,
                };
                self.active[0].hold_until_ms = hold_until;
                self.recompute_target_slots();
            } else {
                let mut card = self.active.remove(idx);
                card.payload = HudCardPayload::Toast {
                    key: eff_key,
                    line1,
                    line2,
                };
                card.hold_until_ms = hold_until;
                self.active.insert(0, card);
                self.recompute_target_slots();
            }
            return;
        }

        // Revive matching toast if mid-exit on an empty stack
        if self.active.is_empty() {
            if let Some(ex_idx) = self.exiting.iter().position(
                |e| matches!(&e.payload, HudCardPayload::Toast { key: k, .. } if k == &eff_key),
            ) {
                let ex = self.exiting.remove(ex_idx);
                if let Some((cur_y, _)) = ex.sample(now_ms) {
                    let payload = HudCardPayload::Toast {
                        key: eff_key,
                        line1,
                        line2,
                    };
                    let hidden_y = -(payload.height() + hs::HIDDEN_TOP_PAD);
                    let open_ms = Self::open_ms_from_current_y(cur_y, hidden_y, now_ms);
                    self.active.insert(
                        0,
                        ActiveHudCard {
                            payload,
                            open_ms,
                            hold_until_ms: hold_until,
                            slot_y: hs::REST_Y,
                            target_slot_y: hs::REST_Y,
                        },
                    );
                    self.recompute_target_slots();
                    return;
                }
            }
        }

        // If an exiting top-sliding card hasn't cleared the top slot yet, convert it to fade-in-place
        // below the incoming toast so the two cards never pass through each other.
        for ex in &mut self.exiting {
            if matches!(ex.mode, HudExitMode::SlideUpAndFade { .. }) {
                if let Some((cur_y, cur_op)) = ex.sample(now_ms) {
                    if cur_y > 0.0 {
                        ex.exit_start_ms = now_ms;
                        ex.start_opacity = cur_op;
                        ex.mode = HudExitMode::FadeInPlace { fixed_y: cur_y };
                    }
                }
            }
        }

        self.active.insert(
            0,
            ActiveHudCard {
                payload: HudCardPayload::Toast {
                    key: eff_key,
                    line1,
                    line2,
                },
                open_ms: now_ms,
                hold_until_ms: hold_until,
                slot_y: hs::REST_Y,
                target_slot_y: hs::REST_Y,
            },
        );
        self.recompute_target_slots();
        self.enforce_capacity(now_ms);
    }

    /// Spawns a persistent `Progress` card at `index 0` and returns its unique handle (`> 0`).
    pub fn progress_begin(
        &mut self,
        title: &str,
        detail: &str,
        progress: Option<f32>,
        now_ms: u64,
    ) -> u32 {
        self.step(now_ms);
        let handle = self.alloc_id();
        let norm_prog = progress.map(|p| p.clamp(0.0, 1.0));
        self.active.insert(
            0,
            ActiveHudCard {
                payload: HudCardPayload::Progress {
                    handle,
                    title: title.trim().to_string(),
                    detail: detail.trim().to_string(),
                    progress: norm_prog,
                },
                open_ms: now_ms,
                hold_until_ms: None,
                slot_y: hs::REST_Y,
                target_slot_y: hs::REST_Y,
            },
        );
        self.recompute_target_slots();
        self.enforce_capacity(now_ms);
        handle
    }

    /// Updates an active `Progress` card in place (preserving its current stack index).
    pub fn progress_update(
        &mut self,
        handle: u32,
        title: &str,
        detail: &str,
        progress: Option<f32>,
        now_ms: u64,
    ) -> bool {
        self.step(now_ms);
        let norm_prog = progress.map(|p| p.clamp(0.0, 1.0));
        if let Some(card) = self.active.iter_mut().find(
            |c| matches!(&c.payload, HudCardPayload::Progress { handle: h, .. } if *h == handle),
        ) {
            if let HudCardPayload::Progress {
                title: t,
                detail: d,
                progress: p,
                ..
            } = &mut card.payload
            {
                if !title.trim().is_empty() {
                    *t = title.trim().to_string();
                }
                *d = detail.trim().to_string();
                *p = norm_prog;
            }
            return true;
        }
        false
    }

    /// Ends and dismisses an active `Progress` card (`SlideUpAndFade` if `index 0`, `FadeInPlace` if `index > 0`).
    pub fn progress_end(&mut self, handle: u32, now_ms: u64) -> bool {
        self.step(now_ms);
        if let Some(idx) = self.active.iter().position(
            |c| matches!(&c.payload, HudCardPayload::Progress { handle: h, .. } if *h == handle),
        ) {
            self.dismiss_active_at(idx, now_ms);
            return true;
        }
        false
    }

    /// Pushes a pinned `Confirmation` card onto `index 0` and returns its unique ID (`> 0`).
    pub fn confirm_push(
        &mut self,
        question: &str,
        detail: &str,
        confirm_label: &str,
        cancel_label: &str,
        now_ms: u64,
    ) -> u32 {
        self.step(now_ms);
        let id = self.alloc_id();
        self.active.insert(
            0,
            ActiveHudCard {
                payload: HudCardPayload::Confirmation {
                    id,
                    question: question.trim().to_string(),
                    detail: detail.trim().to_string(),
                    confirm_label: if confirm_label.trim().is_empty() {
                        "OK".to_string()
                    } else {
                        confirm_label.trim().to_string()
                    },
                    cancel_label: if cancel_label.trim().is_empty() {
                        "Cancel".to_string()
                    } else {
                        cancel_label.trim().to_string()
                    },
                },
                open_ms: now_ms,
                hold_until_ms: None,
                slot_y: hs::REST_Y,
                target_slot_y: hs::REST_Y,
            },
        );
        self.recompute_target_slots();
        self.enforce_capacity(now_ms);
        id
    }

    /// Dismisses a `Confirmation` card once the user presses `A` or `B`.
    pub fn confirm_resolve(&mut self, id: u32, now_ms: u64) -> bool {
        self.step(now_ms);
        if let Some(idx) = self.active.iter().position(
            |c| matches!(&c.payload, HudCardPayload::Confirmation { id: cid, .. } if *cid == id),
        ) {
            self.dismiss_active_at(idx, now_ms);
            return true;
        }
        false
    }

    /// Returns `true` if one or more `Confirmation` prompts are currently waiting for `A` / `B`.
    #[inline]
    pub fn has_active_confirm(&self) -> bool {
        self.active.iter().any(|c| c.payload.is_confirm())
    }

    /// Returns the ID of the topmost active `Confirmation` in the stack (lowest index).
    #[inline]
    pub fn top_confirm_id(&self) -> Option<u32> {
        for c in &self.active {
            if let HudCardPayload::Confirmation { id, .. } = &c.payload {
                return Some(*id);
            }
        }
        None
    }

    /// Returns `true` if any card is visible or exiting on screen.
    pub fn is_visible(&mut self, now_ms: u64) -> bool {
        self.step(now_ms);
        !self.active.is_empty() || !self.exiting.is_empty()
    }

    /// Returns `true` when the 60 FPS animation loop must keep ticking (`Volume`/`Toast` timer active,
    /// entrance/exit motion in flight, reflow settling, or indeterminate `Progress` shuttle moving).
    pub fn needs_frame_tick(&mut self, now_ms: u64) -> bool {
        self.step(now_ms);
        if !self.exiting.is_empty() {
            return true;
        }
        for c in &self.active {
            if c.hold_until_ms.is_some() {
                return true;
            }
            if c.enter_progress(now_ms) < 0.999 {
                return true;
            }
            if (c.target_slot_y - c.slot_y).abs() > 0.2 {
                return true;
            }
            if matches!(
                &c.payload,
                HudCardPayload::Progress { progress: None, .. }
            ) {
                return true;
            }
        }
        false
    }

    /// Returns all exiting + active cards in back-to-front draw order (`index 0` drawn last on top).
    pub fn collect_render_items(&mut self, now_ms: u64) -> Vec<HudRenderItem> {
        self.step(now_ms);
        let mut out = Vec::with_capacity(self.exiting.len() + self.active.len());
        for ex in &self.exiting {
            if let Some((y, opacity)) = ex.sample(now_ms) {
                if opacity > 0.005 {
                    out.push(HudRenderItem {
                        payload: ex.payload.clone(),
                        y,
                        opacity,
                        is_exiting: true,
                        exit_mode: Some(ex.mode),
                    });
                }
            }
        }
        for c in self.active.iter().rev() {
            out.push(HudRenderItem {
                payload: c.payload.clone(),
                y: c.rendered_y(now_ms),
                opacity: 1.0,
                is_exiting: false,
                exit_mode: None,
            });
        }
        out
    }

    #[inline]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    #[inline]
    pub fn active_payload_at(&self, index: usize) -> Option<&HudCardPayload> {
        self.active.get(index).map(|c| &c.payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn singleton_volume_pushes_confirm_down_and_promotes_from_lower_slot() {
        let mut stack = HudStack::default();

        // 1. Spawn "Remove this folder?" Confirmation at index 0
        let cid = stack.confirm_push(
            "Remove this folder?",
            "/mnt/SDCARD/Media/Music",
            "Confirm",
            "Cancel",
            1000,
        );
        stack.step(1300);
        assert_eq!(stack.active_count(), 1);
        assert_eq!(stack.top_confirm_id(), Some(cid));

        // 2. Press VOL + while Confirmation is waiting -> Volume enters at index 0, pushes Confirm to index 1
        stack.trigger_volume(0.55, 1400);
        assert_eq!(stack.active_count(), 2);
        assert!(stack.active_payload_at(0).unwrap().is_volume());
        assert!(stack.active_payload_at(1).unwrap().is_confirm());
        // Confirmation still routes A/B!
        assert_eq!(stack.top_confirm_id(), Some(cid));

        // Confirmation smoothly glides downward toward `22 + 72 + 12 = 106`
        let items_t0 = stack.collect_render_items(1400);
        let confirm_y0 = items_t0
            .iter()
            .find(|it| it.payload.is_confirm())
            .unwrap()
            .y;
        assert!((confirm_y0 - hs::REST_Y).abs() < 0.5);

        let items_t1 = stack.collect_render_items(1650);
        let confirm_y1 = items_t1
            .iter()
            .find(|it| it.payload.is_confirm())
            .unwrap()
            .y;
        assert!(confirm_y1 > 98.0 && confirm_y1 <= 106.05);

        // 3. Push a Toast ("Added to playlist") -> Toast at 0, Volume at 1, Confirm at 2
        stack.trigger_toast("added", "Added to playlist", 2000, 1700);
        assert_eq!(stack.active_count(), 3);
        assert!(stack.active_payload_at(0).unwrap().is_toast());
        assert!(stack.active_payload_at(1).unwrap().is_volume());
        assert!(stack.active_payload_at(2).unwrap().is_confirm());
        let _ = stack.collect_render_items(1900);

        // 4. Press VOL + again while Volume is at index 1 -> promotes Volume to index 0 (no duplicate!)
        stack.trigger_volume(0.65, 1950);
        assert_eq!(stack.active_count(), 3);
        assert_eq!(
            stack.active_payload_at(0),
            Some(&HudCardPayload::Volume { level_norm: 0.65 })
        );
        assert!(stack.active_payload_at(1).unwrap().is_toast());
        assert!(stack.active_payload_at(2).unwrap().is_confirm());
    }

    #[test]
    fn topmost_exits_upward_while_lower_items_exit_strictly_by_fade_in_place() {
        let mut stack = HudStack::default();

        // Spawn Confirmation, then Volume above it, then Toast above Volume:
        // index 0 = Toast, index 1 = Volume, index 2 = Confirmation
        let cid = stack.confirm_push("Remove this folder?", "", "OK", "Cancel", 1000);
        stack.trigger_volume(0.5, 1200); // expires at 3200
        stack.trigger_toast("info", "Track queued", 3000, 1400); // expires at 4400
        let _ = stack.collect_render_items(2000);

        // At t = 3200, Volume (at index 1 > 0!) expires while Toast sits at index 0.
        // Volume MUST exit via FadeInPlace (never sliding upward!), while Confirmation glides up from index 2 -> 1!
        let items_3250 = stack.collect_render_items(3250);
        let exiting_vol = items_3250
            .iter()
            .find(|it| it.is_exiting && it.payload.is_volume())
            .expect("Volume should be fading out in place");
        match exiting_vol.exit_mode {
            Some(HudExitMode::FadeInPlace { fixed_y }) => {
                assert!((exiting_vol.y - fixed_y).abs() < 1e-5);
                assert!(exiting_vol.opacity < 1.0 && exiting_vol.opacity > 0.0);
            }
            other => panic!("Expected FadeInPlace for lower item exit, got {other:?}"),
        }

        // At t = 4400, Toast (at index 0!) expires above Confirmation:
        // Toast MUST exit via SlideUpAndFade, while Confirmation glides smoothly up to REST_Y (22.0)!
        let items_4450 = stack.collect_render_items(4450);
        let exiting_toast = items_4450
            .iter()
            .find(|it| it.is_exiting && it.payload.is_toast())
            .expect("Topmost Toast should slide up and fade");
        match exiting_toast.exit_mode {
            Some(HudExitMode::SlideUpAndFade { start_y, .. }) => {
                assert!(exiting_toast.y < start_y, "topmost card must move upward");
            }
            other => panic!("Expected SlideUpAndFade for index 0 exit, got {other:?}"),
        }

        // By t = 4800, Confirmation has glided smoothly back to REST_Y (index 0)
        let items_4800 = stack.collect_render_items(4800);
        assert_eq!(items_4800.len(), 1);
        assert!((items_4800[0].y - hs::REST_Y).abs() < 0.25);

        // Resolve Confirmation at index 0 -> exits upward!
        assert!(stack.confirm_resolve(cid, 5000));
        assert!(!stack.has_active_confirm());
        let items_5050 = stack.collect_render_items(5050);
        assert!(matches!(
            items_5050[0].exit_mode,
            Some(HudExitMode::SlideUpAndFade { .. })
        ));
    }

    #[test]
    fn progress_persists_until_ended_and_capacity_only_evicts_oldest_toast() {
        let mut stack = HudStack::default();
        let ph = stack.progress_begin("Scanning Library", "0%", Some(0.0), 1000);

        // Progress never auto-expires even after 30 seconds
        assert!(stack.is_visible(31_000));
        assert!(stack.progress_update(ph, "Scanning Library", "65%", Some(0.65), 31_500));

        // Flood 5 distinct toasts -> cap (4) evicts oldest toasts via FadeInPlace, NEVER dropping Progress!
        for n in 0..5 {
            stack.trigger_toast(&format!("k{n}"), &format!("Toast {n}"), 2000, 32_000 + n * 20);
        }
        assert!(stack.active_count() <= hs::MAX_VISIBLE_CARDS);
        assert!(
            stack.active.iter().any(
                |c| matches!(&c.payload, HudCardPayload::Progress { handle, .. } if *handle == ph)
            ),
            "Active Progress card must never be evicted by Toast overflow"
        );

        assert!(stack.progress_end(ph, 32_200));
    }
}
