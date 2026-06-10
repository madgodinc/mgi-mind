//! Live pulse stream — the "taxi" impulses that carry data through the brain.
//!
//! When the brain writes, reads, or processes a memory, it emits a `PulseEvent`
//! on a process-global broadcast channel. The viewer's `/api/pulse` SSE route
//! subscribes and forwards each event to the browser, which animates an impulse
//! travelling a neuron toward the target core. This is the schematic "you can
//! see the data move" view — not a heavy 3D scene, a clear flow of pulses.
//!
//! The channel is in-process and best-effort: if no one is subscribed (no
//! viewer open), events are dropped on the floor at zero cost. Publishing never
//! blocks and never fails the caller (mirrors `audit::record`).

use once_cell::sync::OnceCell;
use serde::Serialize;
use tokio::sync::broadcast;

/// What kind of impulse this is. Drives the color in the UI legend.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PulseKind {
    /// New memory / fact written — a core is born. Green.
    Write,
    /// A read / search lit up cores. Blue.
    Read,
    /// Internal processing: duel, quarantine, consolidate. Amber.
    Process,
}

impl PulseKind {
    /// Hex color the UI uses for this impulse (single source of truth so the
    /// legend and the pulses always agree).
    pub fn color(self) -> &'static str {
        match self {
            PulseKind::Write => "#39d98a",   // green
            PulseKind::Read => "#3aa0ff",    // blue
            PulseKind::Process => "#ffb020", // amber
        }
    }
}

/// One impulse. `target` is the core it flows toward (a library, a memory id,
/// or an entity); `label` is the short human tag (op name, query, etc).
#[derive(Debug, Clone, Serialize)]
pub struct PulseEvent {
    pub kind: PulseKind,
    pub color: &'static str,
    /// Logical target core: "lib:<name>", "mem:<id>", "fact", or free text.
    pub target: String,
    /// Short label shown on/near the impulse.
    pub label: String,
    /// Optional actor (which agent caused it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

impl PulseEvent {
    pub fn new(kind: PulseKind, target: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            kind,
            color: kind.color(),
            target: target.into(),
            label: label.into(),
            actor: None,
        }
    }

    pub fn actor(mut self, actor: Option<String>) -> Self {
        self.actor = actor.filter(|a| !a.is_empty());
        self
    }
}

/// Process-global broadcast sender. Lazily created on first use. Capacity is
/// small: this is a live feed, a slow subscriber that lags just misses old
/// pulses (RecvError::Lagged) rather than blocking any writer.
static CHANNEL: OnceCell<broadcast::Sender<PulseEvent>> = OnceCell::new();

fn sender() -> &'static broadcast::Sender<PulseEvent> {
    CHANNEL.get_or_init(|| {
        let (tx, _rx) = broadcast::channel(256);
        tx
    })
}

/// Subscribe to the live pulse feed. Each subscriber gets its own receiver.
pub fn subscribe() -> broadcast::Receiver<PulseEvent> {
    sender().subscribe()
}

/// Emit a pulse. Best-effort: returns immediately, never blocks, never fails
/// the caller. With no subscribers the event is simply dropped.
pub fn emit(event: PulseEvent) {
    // `send` errors only when there are zero receivers — that's the common case
    // (no viewer open) and is not an error for us.
    let _ = sender().send(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_distinct_per_kind() {
        assert_ne!(PulseKind::Write.color(), PulseKind::Read.color());
        assert_ne!(PulseKind::Read.color(), PulseKind::Process.color());
    }

    #[tokio::test]
    async fn subscriber_receives_emitted_pulse() {
        let mut rx = subscribe();
        emit(PulseEvent::new(PulseKind::Write, "mem:abc", "add").actor(Some("soloist".into())));
        let got = rx.recv().await.expect("pulse delivered");
        assert_eq!(got.kind, PulseKind::Write);
        assert_eq!(got.target, "mem:abc");
        assert_eq!(got.color, "#39d98a");
        assert_eq!(got.actor.as_deref(), Some("soloist"));
    }

    #[test]
    fn emit_with_no_subscriber_is_silent() {
        // Must not panic / must not block when nobody is listening.
        emit(PulseEvent::new(PulseKind::Read, "search", "q"));
    }
}
