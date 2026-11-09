//! Direct connection: pause local sharing while the direct Ethernet cable to the
//! other computer is unplugged, and resume when it is plugged back in.
//!
//! Two computers joined by one Ethernet cable (no router) give themselves
//! link-local 169.254/16 addresses, so the cable counts as connected exactly
//! while this computer has such an address. Only a pause this monitor made is
//! ever resumed by it: a pause chosen by the user, a remote user or an emergency
//! is never lifted when the cable returns.
use super::*;
use crate::api::{AgentApi, DirectCableStatus};
use std::net::Ipv4Addr;

/// How often the network adapters are checked for the cable.
const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct Monitor {
    /// Cable presence at the previous check.
    last_present: Option<bool>,
    /// Setting at the previous check.
    last_enabled: bool,
    /// Sharing was paused by this monitor and not yet resumed.
    paused_by_cable: bool,
    addresses: Vec<Ipv4Addr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    None,
    Pause,
    Resume,
}

impl Monitor {
    /// One observation. `paused` is the current local pause state.
    pub(crate) fn step(&mut self, enabled: bool, present: bool, paused: bool) -> Action {
        let was_present = self.last_present.replace(present);
        let was_enabled = std::mem::replace(&mut self.last_enabled, enabled);
        if !enabled || present {
            // Setting turned off, or cable back: lift only our own pause.
            return if std::mem::take(&mut self.paused_by_cable) && paused {
                Action::Resume
            } else {
                Action::None
            };
        }
        // Unplugged. Pause once per unplug (or when the setting is switched on
        // while unplugged); a later manual resume is respected until the next unplug.
        let newly_absent = was_present != Some(false) || !was_enabled;
        if newly_absent && !paused {
            self.paused_by_cable = true;
            Action::Pause
        } else {
            Action::None
        }
    }
}

/// This computer's direct-cable (link-local IPv4) addresses.
pub(crate) fn direct_addresses() -> Vec<Ipv4Addr> {
    oink_network::discovery::local_ipv4()
        .into_iter()
        .map(|(ip, _)| ip)
        .filter(Ipv4Addr::is_link_local)
        .collect()
}

impl Node {
    pub(crate) fn direct_cable_status(&self) -> DirectCableStatus {
        let enabled = self.settings().direct_cable_pause;
        let monitor = lock(&self.inner.direct_cable);
        DirectCableStatus {
            enabled,
            connected: !monitor.addresses.is_empty(),
            paused_by_cable: monitor.paused_by_cable && self.pause_status().paused,
            addresses: monitor.addresses.iter().map(ToString::to_string).collect(),
        }
    }

    pub(super) fn start_direct_cable(&self) {
        let node = self.clone();
        let mut stop = self.stop_receiver();
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(POLL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = stop.changed() => break,
                    _ = tick.tick() => {}
                }
                let addresses = direct_addresses();
                let enabled = node.settings().direct_cable_pause;
                let paused = node.pause_status().paused;
                let action = {
                    let mut monitor = lock(&node.inner.direct_cable);
                    monitor.addresses = addresses.clone();
                    monitor.step(enabled, !addresses.is_empty(), paused)
                };
                let paused = match action {
                    Action::None => continue,
                    Action::Pause => true,
                    Action::Resume => false,
                };
                tracing::info!(
                    paused,
                    cable = !addresses.is_empty(),
                    "direct connection: cable change sets local sharing"
                );
                let control = node.clone();
                let result = tokio::task::spawn_blocking(move || {
                    control.set_local_paused(crate::device_controls::runtime::fresh_request_id(), paused)
                })
                .await;
                if !matches!(result, Ok(Ok(_))) {
                    tracing::warn!(paused, "direct connection: changing local sharing failed");
                    // Retry on the next check rather than claiming a pause we did not make.
                    let mut monitor = lock(&node.inner.direct_cable);
                    monitor.last_present = None;
                    if paused {
                        monitor.paused_by_cable = false;
                    } else {
                        monitor.paused_by_cable = true;
                    }
                }
            }
        });
        lock(&self.inner.tasks).push(task);
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Monitor};

    #[test]
    fn unplug_pauses_once_and_replug_resumes_only_its_own_pause() {
        let mut m = Monitor::default();
        assert_eq!(m.step(true, true, false), Action::None);
        assert_eq!(m.step(true, false, false), Action::Pause);
        assert_eq!(m.step(true, false, true), Action::None);
        assert_eq!(m.step(true, true, true), Action::Resume);
        assert_eq!(m.step(true, true, false), Action::None);
    }

    #[test]
    fn manual_resume_while_unplugged_is_respected_until_next_unplug() {
        let mut m = Monitor::default();
        m.step(true, true, false);
        assert_eq!(m.step(true, false, false), Action::Pause);
        // User resumes by hand: no immediate re-pause.
        assert_eq!(m.step(true, false, false), Action::None);
        // Replug does not resume (already running) and clears ownership.
        assert_eq!(m.step(true, true, false), Action::None);
        assert_eq!(m.step(true, false, false), Action::Pause);
    }

    #[test]
    fn user_pause_is_never_lifted_by_the_cable() {
        let mut m = Monitor::default();
        m.step(true, true, false);
        // Already paused by the user when the cable is pulled.
        assert_eq!(m.step(true, false, true), Action::None);
        assert_eq!(m.step(true, true, true), Action::None);
    }

    #[test]
    fn setting_controls_pausing_and_turning_it_off_resumes() {
        let mut m = Monitor::default();
        assert_eq!(m.step(false, false, false), Action::None);
        // Switching on while unplugged pauses.
        assert_eq!(m.step(true, false, false), Action::Pause);
        // Switching off lifts our pause.
        assert_eq!(m.step(false, false, true), Action::Resume);
        assert_eq!(m.step(false, false, false), Action::None);
    }
}
