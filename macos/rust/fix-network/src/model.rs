// Shared result types: check states, repair steps, remedies, findings and reports.

// Health of one checked dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Healthy,
    Failed,
    Absent,
    Unknown,
    NotTested,
    Unsupported,
}

impl State {
    // Return the label printed in reports.
    pub fn label(self) -> &'static str {
        match self {
            State::Healthy => "HEALTHY",
            State::Failed => "FAILED",
            State::Absent => "NONE",
            State::Unknown => "UNKNOWN",
            State::NotTested => "NOT TESTED",
            State::Unsupported => "UNSUPPORTED",
        }
    }
}

// One repair action the fix command can take.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    WifiOn,
    FlushDns,
    ClearArp,
    FixProxy,
    ReconnectVpn(String),
    RenewDhcp,
    CycleWifi,
    FlushRoutes,
}

impl Step {
    // Return the step's display name.
    pub fn name(&self) -> String {
        match self {
            Step::WifiOn => "turn Wi-Fi on".into(),
            Step::FlushDns => "flush DNS cache".into(),
            Step::ClearArp => "clear ARP entry for gateway".into(),
            Step::FixProxy => "disable unreachable proxies".into(),
            Step::ReconnectVpn(n) => format!("reconnect VPN \"{n}\""),
            Step::RenewDhcp => "renew DHCP lease".into(),
            Step::CycleWifi => "cycle Wi-Fi (watchdog-protected)".into(),
            Step::FlushRoutes => "route -n flush (broad)".into(),
        }
    }

    // Return the order in which steps run, least disruptive first.
    pub fn rank(&self) -> u8 {
        match self {
            Step::WifiOn => 0,
            Step::FlushDns => 1,
            Step::ClearArp => 2,
            Step::FixProxy => 3,
            Step::ReconnectVpn(_) => 4,
            Step::RenewDhcp => 5,
            Step::CycleWifi => 6,
            Step::FlushRoutes => 7,
        }
    }

    // Return the flag that must be given before the step may run.
    pub fn gate(&self) -> Option<&'static str> {
        match self {
            Step::FixProxy => Some("--fix-proxy"),
            Step::ReconnectVpn(_) => Some("--vpn"),
            Step::FlushRoutes => Some("--allow-route-flush"),
            _ => None,
        }
    }

    // Tell whether the step can cut an SSH or VPN session.
    pub fn disruptive(&self) -> bool {
        matches!(self, Step::ReconnectVpn(_) | Step::RenewDhcp | Step::CycleWifi | Step::FlushRoutes)
    }
}

// What it takes to resolve a finding.
#[derive(Clone, Debug, PartialEq)]
pub enum Remedy {
    Steps(Vec<Step>),
    Manual(String),
    Reboot(String),
}

// One diagnosed problem and its remedy.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub code: &'static str,
    pub summary: String,
    pub remedy: Remedy,
}

// The outcome of one dimension check.
#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub state: State,
    pub detail: String,
}

// All checks and findings from one snapshot.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub checks: Vec<Check>,
    pub findings: Vec<Finding>,
}

impl Report {
    // Tell whether no dimension failed.
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|c| c.state != State::Failed)
    }

    // Tell whether any remedy calls for cycling Wi-Fi.
    pub fn wifi_reset_needed(&self) -> bool {
        self.findings.iter().any(|f| matches!(&f.remedy, Remedy::Steps(s) if s.contains(&Step::CycleWifi)))
    }
}
