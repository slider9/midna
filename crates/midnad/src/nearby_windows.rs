//! Nearby (pairing with other Midnas, sharing with them and driving them) isn't on Windows yet:
//! `nearby/` is built on Bonjour (`dns_sd`), CoreFoundation, `poll` and Unix file calls. This
//! stands in for it there with the same calls: `peers.list` shows Nearby off, the RPCs refuse,
//! and the hooks other modules call do nothing (no other Midna can be paired, asked or driven).
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok};
use midna_proto::nearby::{
    self as nb, DriveCloseParams, DriveOpenParams, InboxListParams, NearbyMe, PeersConfirmParams, PeersForgetParams, PeersListResult, PeersPairParams,
    PeersSessionsParams, ShareAcceptParams, ShareAnswerParams, ShareDeclineParams, ShareSendParams, Visibility,
};
use midna_proto::{NeedsYou, RpcError};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Why every Nearby call is refused here.
const NOT_YET: &str = "Nearby isn't on Windows yet";

fn not_yet() -> R {
    Err(RpcError::new(midna_proto::error::NOT_IMPLEMENTED, NOT_YET))
}

/// Which Bonjour service type a daemon uses (`Config::nearby`). Always off here.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Mode {
    #[default]
    Off,
    Auto,
    Service(String),
}

#[derive(Clone, Debug)]
pub struct Setup {
    pub mode: Mode,
    pub local_only: bool,
    pub everyone_secs: i64,
    pub serve_delay: Duration,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup { mode: Mode::Off, local_only: false, everyone_secs: nb::EVERYONE_SECS, serve_delay: Duration::ZERO }
    }
}

impl Setup {
    pub fn from_env() -> Setup {
        Setup::default()
    }

    /// Nearby is off: no service type.
    pub fn service(&self, _home: &Path) -> Option<String> {
        None
    }
}

#[derive(Default)]
pub struct Runtime;

/// `peers.list`: Nearby off, with nobody seen or paired.
pub fn list(d: &Daemon) -> R {
    let name = d.setting("nearby.name").as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(computer_name);
    ok(PeersListResult {
        me: NearbyMe {
            fingerprint: String::new(),
            name,
            visibility: Visibility::Off,
            everyone_until: None,
            service: None,
            advertising: false,
            browsing: false,
            port: None,
            protocol: nb::PROTOCOL,
            error: Some(NOT_YET.into()),
            local_network: None,
        },
        peers: vec![],
        paired: vec![],
        pairings: vec![],
    })
}

/// The PC's name, as other machines see it on the network.
fn computer_name() -> String {
    std::env::var("COMPUTERNAME").ok().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "PC".into())
}

pub fn visibility_set(_d: &Daemon, _value: &Value) {}

pub fn start(_d: &Arc<Daemon>) {}

pub mod pairing {
    use super::*;

    pub fn pair(_d: &Arc<Daemon>, _p: PeersPairParams) -> R {
        not_yet()
    }
    pub fn confirm(_d: &Daemon, _p: PeersConfirmParams) -> R {
        not_yet()
    }
    pub fn needs_you_closed(_d: &Daemon, _item: &NeedsYou, _resolution: &Value) {}
    pub fn set_tier(_d: &Daemon, _p: nb::PeersSetTierParams) -> R {
        not_yet()
    }
    pub fn set_auto_accept(_d: &Daemon, _p: nb::PeersSetAutoAcceptParams) -> R {
        not_yet()
    }
    pub fn set_agent_send(_d: &Daemon, _p: nb::PeersSetAgentSendParams) -> R {
        not_yet()
    }
    pub fn set_drive(_d: &Daemon, _p: nb::PeersSetDriveParams) -> R {
        not_yet()
    }
    pub fn forget(_d: &Daemon, _p: PeersForgetParams) -> R {
        not_yet()
    }
}

pub mod share {
    use super::*;

    pub fn send(_d: &Daemon, _ctx: &Ctx, _p: ShareSendParams) -> R {
        not_yet()
    }
    pub fn accept(_d: &Arc<Daemon>, _ctx: &Ctx, _p: ShareAcceptParams) -> R {
        not_yet()
    }
    pub fn decline(_d: &Daemon, _ctx: &Ctx, _p: ShareDeclineParams) -> R {
        not_yet()
    }
    pub fn needs_you_closed(_d: &Daemon, _item: &NeedsYou, _resolution: &Value) {}
    pub fn list(_d: &Daemon, _ctx: &Ctx, _p: InboxListParams) -> R {
        not_yet()
    }
}

pub mod agent_ask {
    use super::*;

    pub fn sessions(_d: &Daemon, _p: PeersSessionsParams) -> R {
        not_yet()
    }
    /// A queued message was sent: none came from another Midna here.
    pub fn sent(_d: &Daemon, _sid: &str, _id: &str) {}
    pub fn unqueued(_d: &Daemon, _sid: &str, _ids: &[String]) {}
}

pub mod agent_start {
    use super::*;

    pub fn answer(_d: &Daemon, _p: ShareAnswerParams) -> R {
        not_yet()
    }
    /// A turn ended: no other Midna started it here.
    pub fn turn_ended(_d: &Daemon, _sid: &str, _message: Option<&str>, _failed: Option<&str>) {}
    pub fn ended(_d: &Daemon, _sid: &str) {}
}

pub mod local_only {
    use super::*;

    /// Nobody drives this PC from another one, so nothing is refused for being local-only.
    pub fn approval(_d: &Daemon, _item: &NeedsYou) -> bool {
        false
    }
    pub fn refusal(_d: &Daemon, _ctx: &Ctx, _method_name: &str, _params: &Value) -> Option<RpcError> {
        None
    }
    pub fn refuse(_d: &Daemon, why: &str) -> RpcError {
        RpcError::human_only(why.to_string())
    }
}

pub mod guard {
    use super::*;

    pub fn refusal(_d: &Daemon, _method: &str, _params: &Value) -> Option<RpcError> {
        None
    }
}

pub mod relay {
    use super::*;

    pub fn open_rpc(_d: &Arc<Daemon>, _ctx: &Ctx, _p: DriveOpenParams) -> R {
        not_yet()
    }
    pub fn close_rpc(_d: &Daemon, _ctx: &Ctx, _p: DriveCloseParams) -> R {
        not_yet()
    }
    /// Drive links this PC opened: none.
    pub fn list(_d: &Daemon) -> Vec<nb::DriveLink> {
        vec![]
    }
}
