//! Turns check results into alerts.
//!
//! - A failure must repeat `fail_threshold` times in a row before alerting.
//! - Each incident alerts once, and once more when the device recovers.
//! - "Busy" rounds (this computer is printing) are skipped, but a print
//!   connection open for `busy_threshold` rounds becomes a `JobStuck` alert.

use std::time::SystemTime;

use crate::checks::{CheckResult, FailureCode};
use crate::i18n::Msg;
use crate::msg;
use crate::probe::DeviceProbe;

#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    pub fail: u32,
    pub busy: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Unknown,
    Ok,
    Busy,
    Problem,
}

/// State of a device after its last check.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub device: String,
    pub host: String,
    pub health: Health,
    pub failure: Option<FailureCode>,
    pub message: Option<Msg>,
    pub notes: Vec<Msg>,
    pub alerted: bool,
    pub last_check: Option<SystemTime>,
}

/// Something worth logging or notifying.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A failure code was seen for the first time in a row (logged).
    FailureObserved { failure: FailureCode, message: Msg },
    /// The failure persisted: notify people.
    AlertRaised { failure: FailureCode, message: Msg },
    /// A device that had been alerted works again.
    Recovered { previous: FailureCode },
    /// A failure cleared before reaching the threshold (logged only).
    TransientCleared { failure: FailureCode, checks: u32 },
}

pub struct DeviceMonitor {
    probe: DeviceProbe,
    thresholds: Thresholds,
    fails: u32,
    busy: u32,
    alerted: bool,
    last: Option<FailureCode>,
    snapshot: Snapshot,
}

impl DeviceMonitor {
    pub fn new(device: &str, host: &str, probe: DeviceProbe, thresholds: Thresholds) -> Self {
        DeviceMonitor {
            probe,
            thresholds,
            fails: 0,
            busy: 0,
            alerted: false,
            last: None,
            snapshot: Snapshot {
                device: device.to_owned(),
                host: host.to_owned(),
                health: Health::Unknown,
                failure: None,
                message: None,
                notes: Vec::new(),
                alerted: false,
                last_check: None,
            },
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub async fn check(&mut self, now: SystemTime) -> Vec<Event> {
        let result = self.probe.run().await;
        self.apply(result, now)
    }

    /// Pure state transition (used directly by tests).
    pub fn apply(&mut self, result: CheckResult, now: SystemTime) -> Vec<Event> {
        let mut events = Vec::new();
        let result = if result == CheckResult::Busy {
            self.busy += 1;
            if self.busy < self.thresholds.busy {
                self.snapshot.health = Health::Busy;
                self.snapshot.last_check = Some(now);
                return events;
            }
            // A receipt prints in seconds; this means the printer stopped taking data mid-job.
            self.fails = self.fails.max(self.thresholds.fail.saturating_sub(1)); // alert this round
            CheckResult::fail(FailureCode::JobStuck, msg!("msg.jobStuck", host = &self.snapshot.host, count = self.busy))
        } else {
            self.busy = 0;
            result
        };

        match result {
            CheckResult::Ok { notes } => {
                match self.last {
                    Some(previous) if self.alerted => events.push(Event::Recovered { previous }),
                    Some(failure) if self.fails > 0 => events.push(Event::TransientCleared { failure, checks: self.fails }),
                    _ => {}
                }
                self.fails = 0;
                self.alerted = false;
                self.last = None;
                self.snapshot = Snapshot {
                    health: Health::Ok,
                    failure: None,
                    message: None,
                    notes,
                    alerted: false,
                    last_check: Some(now),
                    ..self.snapshot.clone()
                };
            }
            CheckResult::Failed { code, message, notes } => {
                self.fails += 1;
                if self.last != Some(code) {
                    events.push(Event::FailureObserved { failure: code, message: message.clone() });
                }
                self.last = Some(code);
                if !self.alerted && self.fails >= self.thresholds.fail {
                    events.push(Event::AlertRaised { failure: code, message: message.clone() });
                    self.alerted = true;
                }
                self.snapshot = Snapshot {
                    health: Health::Problem,
                    failure: Some(code),
                    message: Some(message),
                    notes,
                    alerted: self.alerted,
                    last_check: Some(now),
                    ..self.snapshot.clone()
                };
            }
            CheckResult::Busy => unreachable!("handled above"),
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn monitor(fail: u32, busy: u32) -> DeviceMonitor {
        DeviceMonitor::new("Kitchen", "10.0.0.22", DeviceProbe::new(Vec::new()), Thresholds { fail, busy })
    }

    fn down() -> CheckResult {
        CheckResult::fail(FailureCode::Unreachable, msg!("msg.unreachable", host = "h"))
    }

    fn blocked() -> CheckResult {
        CheckResult::fail(FailureCode::PortBlocked, Msg::key("detail.refused"))
    }

    fn run(m: &mut DeviceMonitor, results: Vec<CheckResult>) -> Vec<Event> {
        let t0 = SystemTime::UNIX_EPOCH;
        results.into_iter().enumerate().flat_map(|(i, r)| m.apply(r, t0 + Duration::from_secs(60 * i as u64))).collect()
    }

    fn alerts(events: &[Event]) -> Vec<FailureCode> {
        events.iter().filter_map(|e| if let Event::AlertRaised { failure, .. } = e { Some(*failure) } else { None }).collect()
    }

    #[test]
    fn alerts_only_after_the_threshold_and_only_once() {
        let mut m = monitor(3, 10);
        let events = run(&mut m, vec![down(), down(), down(), down(), down()]);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], Event::FailureObserved { failure: FailureCode::Unreachable, .. }));
        assert_eq!(alerts(&events), [FailureCode::Unreachable]);
        assert!(m.snapshot().alerted);
        assert_eq!(m.snapshot().health, Health::Problem);
    }

    #[test]
    fn recovery_is_reported_after_an_alert() {
        let mut m = monitor(2, 10);
        let events = run(&mut m, vec![down(), down(), CheckResult::ok()]);
        assert_eq!(events.last(), Some(&Event::Recovered { previous: FailureCode::Unreachable }));
        assert_eq!(m.snapshot().health, Health::Ok);
        assert!(!m.snapshot().alerted);
    }

    #[test]
    fn short_glitches_are_logged_but_not_alerted() {
        let mut m = monitor(3, 10);
        let events = run(&mut m, vec![down(), down(), CheckResult::ok()]);
        assert!(alerts(&events).is_empty());
        assert_eq!(events.last(), Some(&Event::TransientCleared { failure: FailureCode::Unreachable, checks: 2 }));
    }

    #[test]
    fn a_changing_failure_keeps_counting() {
        let mut m = monitor(3, 10);
        let events = run(&mut m, vec![down(), blocked(), blocked()]);
        assert_eq!(events.iter().filter(|e| matches!(e, Event::FailureObserved { .. })).count(), 2);
        assert_eq!(alerts(&events), [FailureCode::PortBlocked]);
    }

    #[test]
    fn busy_rounds_are_skipped() {
        let mut m = monitor(1, 10);
        assert!(run(&mut m, vec![CheckResult::Busy, CheckResult::Busy, CheckResult::ok()]).is_empty());
        assert_eq!(m.snapshot().health, Health::Ok);
    }

    #[test]
    fn a_print_connection_open_too_long_is_a_stuck_job() {
        let mut m = monitor(3, 4);
        let events = run(&mut m, vec![CheckResult::Busy; 4]);
        assert_eq!(alerts(&events), [FailureCode::JobStuck]);
        let message =
            events.iter().find_map(|e| if let Event::AlertRaised { message, .. } = e { Some(message.clone()) } else { None }).unwrap();
        assert!(crate::i18n::Localizer::new(Some("en"), None).format(&message).contains("for 4 checks"));
        assert!(run(&mut m, vec![CheckResult::Busy]).is_empty());
        assert_eq!(run(&mut m, vec![CheckResult::ok()]), [Event::Recovered { previous: FailureCode::JobStuck }]);
    }

    #[test]
    fn notes_are_kept() {
        let mut m = monitor(3, 10);
        run(&mut m, vec![CheckResult::Ok { notes: vec![Msg::key("reason.paperNearEnd")] }]);
        assert_eq!(m.snapshot().notes, [Msg::key("reason.paperNearEnd")]);
    }
}
