//! Explicit arbitration for the Claude desktop live-state lease.
use crate::model::{Activity, Group, Session, REMOVE_AFTER_S};
use crate::observation::Source;
use std::collections::HashMap;

/// Keep scheduled background work discoverable for the existing retention horizon.
/// Ordinary idle work follows the user's recency setting.
pub fn session_visible(session: &Session, at: f64, recency: f64) -> bool {
    let horizon = if session.scheduled {
        REMOVE_AFTER_S
    } else {
        recency
    };
    (session.acknowledged && session.acknowledged_at.is_some_and(|ack| at - ack < horizon.min(REMOVE_AFTER_S)))
        || session.pending_attention
        || session.unread > 0
        || matches!(
            session.activity.group(),
            Group::Working | Group::Waiting | Group::Done
        )
        || at - session.updated < horizon
}

#[derive(Default)]
pub struct SourcePolicy {
    live: HashMap<String, f64>,
    last: HashMap<String, f64>,
    suppressed: std::collections::HashSet<String>,
}
impl SourcePolicy {
    pub fn observe(&mut self, agent: &str, id: &str, source: Source, at: f64) -> bool {
        if agent != "claude-code" {
            return true;
        }
        if matches!(source, Source::DesktopLive | Source::DesktopInactive) {
            if self.last.get(id).is_some_and(|last| *last > at) {
                return false;
            }
            self.last.insert(id.to_string(), at);
        }
        match source {
            Source::DesktopLive => {
                self.live.insert(id.to_string(), at + 8.0);
            }
            Source::DesktopInactive => {
                self.live.remove(id);
            }
            _ => {}
        }
        true
    }
    pub fn activity(
        &self,
        agent: &str,
        id: &str,
        source: Source,
        at: f64,
        old: Activity,
        next: Activity,
    ) -> Activity {
        if agent == "claude-code"
            && source == Source::LocalSnapshot
            && self.live.get(id).is_some_and(|until| *until > at)
            && matches!(old.group(), Group::Working | Group::Waiting)
            && (!matches!(next.group(), Group::Working | Group::Waiting)
                || (old.group() == Group::Waiting && next.group() == Group::Working))
        {
            old
        } else {
            next
        }
    }
    pub fn heartbeat_needed(&self, agent: &str, id: &str, source: Source, at: f64) -> bool {
        agent == "claude-code"
            && source == Source::LocalSnapshot
            && self.suppressed.contains(id)
            && !self.live.get(id).is_some_and(|until| *until > at)
    }
    pub fn record_local(&mut self, agent: &str, id: &str, source: Source, suppressed: bool) {
        if agent == "claude-code" && source == Source::LocalSnapshot {
            if suppressed {
                self.suppressed.insert(id.to_string());
            } else {
                self.suppressed.remove(id);
            }
        }
    }
    pub fn remap(&mut self, old: &str, new: &str) {
        if self.suppressed.remove(old) {
            self.suppressed.insert(new.to_string());
        }
        if let Some(value) = self.last.remove(old) {
            self.last
                .entry(new.to_string())
                .and_modify(|v| *v = v.max(value))
                .or_insert(value);
        }
        if let Some(value) = self.live.remove(old) {
            self.live
                .entry(new.to_string())
                .and_modify(|v| *v = v.max(value))
                .or_insert(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Core, Settings};
    use crate::observation::Observation;

    fn observed(
        agent: &str,
        id: &str,
        source: Source,
        at: f64,
        activity: Option<Activity>,
        text: Option<&str>,
    ) -> Observation {
        let mut obs = Observation::new(agent, id, source, at);
        obs.activity = activity;
        obs.text = text.map(str::to_string);
        obs
    }

    #[test]
    fn older_desktop_inactive_observation_cannot_replace_newer_live_state() {
        let mut core = Core::new(Settings::default());
        core.observe(observed(
            "claude-code",
            "session",
            Source::DesktopLive,
            20.0,
            Some(Activity::Streaming),
            Some("new response"),
        ));
        let updated = core.sessions[0].updated;
        core.observe(observed(
            "claude-code",
            "session",
            Source::DesktopInactive,
            10.0,
            Some(Activity::Idle),
            Some("old response"),
        ));
        assert_eq!(core.sessions[0].activity, Activity::Streaming);
        assert_eq!(core.sessions[0].text, "new response");
        assert_eq!(core.sessions[0].updated, updated);
    }

    #[test]
    fn local_snapshot_held_by_live_lease_replays_on_expired_lease_heartbeat() {
        let mut core = Core::new(Settings::default());
        core.observe(observed(
            "claude-code",
            "session",
            Source::Hook,
            10.0,
            Some(Activity::Streaming),
            Some("hook response"),
        ));
        core.observe(observed(
            "claude-code",
            "session",
            Source::DesktopLive,
            20.0,
            None,
            None,
        ));

        let mut snapshot = observed(
            "claude-code",
            "session",
            Source::LocalSnapshot,
            21.0,
            Some(Activity::Idle),
            Some("local final"),
        );
        snapshot.text_at = Some(19.0);
        core.observe(snapshot);
        assert_eq!(core.sessions[0].activity, Activity::Streaming);
        assert_eq!(core.sessions[0].text, "hook response");
        let updated_while_suppressed = core.sessions[0].updated;

        let mut heartbeat = Observation::new("claude-code", "session", Source::LocalSnapshot, 27.0);
        heartbeat.kind = crate::observation::ObservationKind::Heartbeat;
        core.observe(heartbeat.clone());
        assert_eq!(
            core.sessions[0].updated, updated_while_suppressed,
            "unchanged polling heartbeat must not refresh freshness while the lease is active"
        );

        heartbeat.observed_at = 29.0;
        core.observe(heartbeat);
        assert_eq!(core.sessions[0].activity, Activity::Idle);
        assert_eq!(core.sessions[0].text, "local final");
        assert_eq!(
            core.sessions[0].text_at, 19.0,
            "replayed provider timestamp remains authoritative"
        );
        assert_eq!(core.sessions[0].updated, 29.0);
    }

    #[test]
    fn scheduled_idle_visibility_uses_twelve_hour_horizon_but_respects_shelf_and_disable() {
        let mut core = Core::new(Settings::default());
        core.observe(observed("claude-code", "scheduled", Source::Other, 100.0, Some(Activity::Idle), None));
        let mut session = core.sessions[0].clone();
        session.updated = 100.0;
        session.scheduled = true;

        assert!(session_visible(&session, 100.0 + 4.0 * 3600.0, 3600.0), "scheduled idle work remains visible after four hours");
        let mut ordinary = session.clone();
        ordinary.scheduled = false;
        assert!(!session_visible(&ordinary, 100.0 + 4.0 * 3600.0, 3600.0), "ordinary idle work follows the one hour recency setting");
        assert!(!session_visible(&session, 100.0 + REMOVE_AFTER_S + 1.0, 3600.0), "scheduled work expires after the twelve hour retention horizon");

        session.updated = crate::model::now() - 4.0 * 3600.0;
        core.sessions[0] = session.clone();
        assert_eq!(core.visible().len(), 1);
        core.sessions[0].acknowledged = true;
        core.sessions[0].acknowledged_at = Some(crate::model::now());
        assert_eq!(core.visible().len(), 1, "acknowledgement does not hide scheduled work inside its retention horizon");
        core.sessions[0].acknowledged = false;
        core.settings.disabled_agents.push("claude-code".into());
        assert!(core.visible().is_empty(), "disabled agents remain hidden");
    }

    #[test]
    fn acknowledgement_starts_a_bounded_retention_window_without_rewriting_provider_times() {
        let mut settings = Settings::default();
        settings.recency_minutes = 120;
        let mut core = Core::new(settings);
        core.observe(observed("codex", "old-idle", Source::Other, crate::model::now(), Some(Activity::Idle), Some("old result")));
        let before_updated = crate::model::now() - 3.0 * 3600.0;
        let before_text_at = before_updated - 900.0;
        core.sessions[0].updated = before_updated;
        core.sessions[0].text_at = before_text_at;
        assert!(core.visible().is_empty());

        core.acknowledge_open("codex", "old-idle").unwrap();
        assert_eq!(core.sessions[0].updated, before_updated);
        assert_eq!(core.sessions[0].text_at, before_text_at);
        assert!(core.sessions[0].acknowledged_at.is_some());
        assert_eq!(core.visible().len(), 1, "click retains an old inactive tile for the normal two-hour recency window");

        let acked_at = crate::model::now() - 120.0 * 60.0 - 1.0;
        core.sessions[0].acknowledged_at = Some(acked_at);
        core.sessions[0].updated = crate::model::now() - REMOVE_AFTER_S - 1.0;
        core.tick();
        assert!(core.sessions.is_empty(), "acknowledged rows are eventually cleaned up after their bounded retention expires");
    }

    #[test]
    fn discovery_of_existing_cli_alias_only_latches_schedule_metadata() {
        let mut core = Core::new(Settings::default());
        core.observe(observed("claude-code", "cli-session", Source::Hook, 100.0, Some(Activity::WaitingQuestion), Some("existing answer")));
        let mut discovery = observed("claude-code", "host-session", Source::LocalSnapshot, 200.0, Some(Activity::Idle), Some("stale discovery text"));
        discovery.kind = crate::observation::ObservationKind::Discovery;
        discovery.aliases.push("cli-session".into());
        discovery.recorded_at = Some(50.0);
        discovery.scheduled = true;
        core.observe(discovery);

        assert_eq!(core.sessions.len(), 1);
        assert_eq!(core.sessions[0].id, "cli-session");
        assert_eq!(core.sessions[0].activity, Activity::WaitingQuestion);
        assert_eq!(core.sessions[0].text, "existing answer");
        assert_eq!(core.sessions[0].updated, 100.0);
        assert_eq!(core.sessions[0].since, 100.0);
        assert!(core.sessions[0].scheduled);
    }

    #[test]
    fn cli_snapshot_alias_fuses_into_previously_discovered_host_identity() {
        let mut core = Core::new(Settings::default());
        core.observe(observed("claude-code", "host-session", Source::DesktopLive, 100.0, Some(Activity::Thinking), Some("host activity")));
        let mut cli = observed("claude-code", "cli-session", Source::LocalSnapshot, 200.0, Some(Activity::Idle), Some("CLI final answer"));
        cli.aliases.push("host-session".into());
        cli.text_at = Some(190.0);
        core.observe(cli);

        assert_eq!(core.sessions.len(), 1);
        assert_eq!(core.sessions[0].id, "cli-session");
        assert_eq!(core.sessions[0].activity, Activity::Idle);
        assert_eq!(core.sessions[0].text, "CLI final answer");
        assert_eq!(core.sessions[0].text_at, 190.0);
    }

    #[test]
    fn discovery_creation_uses_valid_provider_recorded_timestamp() {
        let mut core = Core::new(Settings::default());
        let mut discovery = observed("claude-code", "host-session", Source::LocalSnapshot, 200.0, Some(Activity::Idle), Some("scheduled answer"));
        discovery.kind = crate::observation::ObservationKind::Discovery;
        discovery.recorded_at = Some(150.0);
        discovery.text_at = Some(150.0);
        discovery.scheduled = true;
        core.observe(discovery);

        assert_eq!(core.sessions.len(), 1);
        assert_eq!(core.sessions[0].since, 150.0);
        assert_eq!(core.sessions[0].updated, 150.0);
        assert_eq!(core.sessions[0].text_at, 150.0);
        assert_eq!(core.sessions[0].text, "scheduled answer");
    }
}
